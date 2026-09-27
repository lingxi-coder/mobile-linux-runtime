//! Android PRoot implementation of the shared mobile-Linux runtime seam.
//!
//! PRoot is a compatibility layer, not a security boundary. Admission remains
//! the responsibility of `MobileLinuxSandbox`; this module owns process and
//! rootfs lifecycle and fails closed when its native/runtime payload is absent.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use mobile_linux_api::mobile_linux::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, LinuxCommandResult, LinuxProcessHandle, MobileLinuxCapability,
    MobileLinuxError, MobileLinuxEvent, MobileLinuxEventKind, MobileLinuxRuntime,
    MobileLinuxRuntimeMode, MobileLinuxTaskSnapshot, MobileLinuxTaskStatus, MountPurpose,
    MountSpec, NetworkPolicy, ProcessStreamSink, PtyOpenRequest, PtySessionHandle, PtySize,
    RawStdioOpenRequest, RawStdioReadResult, RawStdioSessionHandle, RootfsState, RootfsStatus,
    SandboxBackend,
};
use mobile_linux_core::{RootfsManifest, RootfsStore, RootfsStoreError};
use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::Pid;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

const MAX_EVENTS: usize = 4096;
const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const MAX_STDOUT_FRAGMENT_BYTES: usize = 16 * 1024;
const MAX_RAW_STDIO_BUFFER_BYTES: usize = 512 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const REAP_BUDGET: Duration = Duration::from_secs(2);
const ENFORCEMENT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(3);
const MEMORY_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Caller-supplied isolated build layout. The SDK does not select a product profile.
#[derive(Debug, Clone)]
pub struct IsolatedBuildProfile {
    /// Absolute guest prefix containing app/channel/project directories.
    pub guest_root: String,
    /// Final guest project directory name.
    pub project_directory: String,
    /// Optional shared dependency store's guest mount path.
    pub dependency_store: String,
    /// Per-project directory containing HOME/TMP/XDG state.
    pub state_directory: String,
    /// Host application directory below app_sandbox_root.
    pub host_apps_directory: String,
    /// Host build directory below each application.
    pub host_build_directory: String,
    /// Host workspace directory below each application.
    pub host_workspace_directory: String,
    /// Accepted distribution/channel identifiers.
    pub channels: Vec<String>,
}

impl IsolatedBuildProfile {
    fn validate(&self) -> Result<(), MobileLinuxError> {
        validate_guest_path(&self.guest_root)?;
        validate_guest_path(&self.dependency_store)?;
        for component in [
            &self.project_directory,
            &self.state_directory,
            &self.host_apps_directory,
            &self.host_build_directory,
            &self.host_workspace_directory,
        ]
        .into_iter()
        .chain(self.channels.iter())
        {
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.contains('/')
                || component.as_bytes().contains(&0)
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "build profile directories and channels must be single path components".into(),
                ));
            }
        }
        if self.channels.is_empty() {
            return Err(MobileLinuxError::InvalidRequest(
                "build profile requires at least one channel".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
/// Paths and immutable identity expected by one managed Android PRoot runtime.
pub struct AndroidProotRuntimeConfig {
    /// App-private directory containing `active`, `staged`, `bin`, and `tmp`.
    pub managed_root: PathBuf,
    /// Canonical app sandbox root (`Context.filesDir`) that owns app build roots.
    pub app_sandbox_root: PathBuf,
    /// Guest ABI label (`arm64-v8a` or `x86_64`).
    pub abi: String,
    /// Version directory selected below `staged`.
    pub rootfs_version: String,
    /// Expected source archive digest surfaced in status/diagnostics.
    pub archive_sha256: Option<String>,
    /// Android Context.applicationInfo.nativeLibraryDir, supplied by the caller.
    pub native_library_dir: Option<PathBuf>,
    /// Optional explicit product build profile; absent profiles reject isolated builds.
    pub isolated_build_profile: Option<IsolatedBuildProfile>,
}

impl AndroidProotRuntimeConfig {
    fn active_root(&self) -> PathBuf {
        self.managed_root.join("active")
    }

    fn staged_root(&self) -> PathBuf {
        self.managed_root.join("staged").join(&self.rootfs_version)
    }
}

#[derive(Debug)]
struct TaskControl {
    snapshot: Mutex<MobileLinuxTaskSnapshot>,
    pid: AtomicU64,
    cancel_requested: AtomicBool,
    terminal_emitted: AtomicBool,
}

impl TaskControl {
    fn new(task_id: String, command: String, status: MobileLinuxTaskStatus) -> Self {
        Self {
            snapshot: Mutex::new(MobileLinuxTaskSnapshot {
                task_id,
                status,
                command,
                started_at_ms: Some(now_ms()),
                finished_at_ms: None,
                exit_code: None,
                detail: None,
            }),
            pid: AtomicU64::new(0),
            cancel_requested: AtomicBool::new(false),
            terminal_emitted: AtomicBool::new(false),
        }
    }
}

struct PtyControl {
    task: Arc<TaskControl>,
    process: Arc<platform_pty::ProcessHandle>,
}

#[derive(Default)]
struct RawStdioBuffers {
    stdout: VecDeque<u8>,
    stderr: VecDeque<u8>,
    closed: bool,
    exit_code: Option<i32>,
    overflowed: bool,
    error: Option<MobileLinuxError>,
}

struct RawStdioControl {
    task: Arc<TaskControl>,
    writes: tokio::sync::mpsc::Sender<RawStdioWrite>,
    stop: tokio::sync::watch::Sender<bool>,
    worker: Mutex<Option<tokio::task::JoinHandle<Result<(), MobileLinuxError>>>>,
    buffers: Arc<Mutex<RawStdioBuffers>>,
    snapshot_roots: Vec<PathBuf>,
}

struct CallerCancellation {
    task: Arc<TaskControl>,
    stop: tokio::sync::watch::Sender<bool>,
    armed: bool,
}

impl Drop for CallerCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.task.cancel_requested.store(true, Ordering::Release);
            self.stop.send_replace(true);
        }
    }
}

#[derive(Clone)]
struct StreamDelivery {
    task: Arc<TaskControl>,
    stopped: tokio::sync::watch::Receiver<bool>,
    deadline: tokio::time::Instant,
    timed_out: Arc<AtomicBool>,
}

impl StreamDelivery {
    async fn deliver(
        &mut self,
        callback: impl std::future::Future<Output = Result<(), mobile_linux_api::ProcessError>>,
    ) -> Result<(), MobileLinuxError> {
        tokio::select! {
            biased;
            () = async {
                while !self.task.cancel_requested.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => Ok(()),
            _ = wait_raw_stop(&mut self.stopped) => Ok(()),
            () = tokio::time::sleep_until(self.deadline) => {
                self.timed_out.store(true, Ordering::Release);
                Ok(())
            }
            result = callback => result.map_err(MobileLinuxError::from),
        }
    }
}

struct SnapshotCleanup(Option<PathBuf>);

impl Drop for SnapshotCleanup {
    fn drop(&mut self) {
        if let Some(root) = &self.0 {
            let _ = fs::remove_dir_all(root);
        }
    }
}

struct RawStdioWrite {
    input: Vec<u8>,
    reply: tokio::sync::oneshot::Sender<Result<(), MobileLinuxError>>,
}

/// Own the input task even if its caller is cancelled while waiting for the child.
struct StdinWriter(Option<tokio::task::JoinHandle<Result<(), MobileLinuxError>>>);

impl StdinWriter {
    fn start(stdin: Option<ChildStdin>, input: Option<String>) -> Self {
        Self(Some(tokio::spawn(async move {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin
                    .write_all(input.as_bytes())
                    .await
                    .map_err(|error| MobileLinuxError::Io(format!("write stdin: {error}")))?;
            }
            // Closing the owned pipe supplies EOF, including when input is absent.
            Ok(())
        })))
    }

    async fn finish(mut self) -> Result<(), MobileLinuxError> {
        let task = self.0.take().expect("owned stdin task");
        if !task.is_finished() {
            task.abort();
        }
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(MobileLinuxError::Io(format!(
                "stdin writer failed: {error}"
            ))),
        }
    }
}

impl Drop for StdinWriter {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

struct SpawnedChild {
    child: Child,
    enforcement: LinuxEnforcementReceipt,
    memory_limit_bytes: Option<u64>,
}

fn check_snapshot_cancelled(cancelled: &AtomicBool) -> Result<(), MobileLinuxError> {
    if cancelled.load(Ordering::Acquire) {
        Err(MobileLinuxError::InvalidRequest(
            "raw stdio snapshot startup cancelled".into(),
        ))
    } else {
        Ok(())
    }
}

fn copy_raw_stdio_snapshot(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
) -> Result<(), MobileLinuxError> {
    check_snapshot_cancelled(cancelled)?;
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        MobileLinuxError::Io(format!(
            "inspect read-only LSP workspace {}: {error}",
            source.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(|error| {
            MobileLinuxError::Io(format!(
                "read LSP workspace symlink {}: {error}",
                source.display()
            ))
        })?;
        std::os::unix::fs::symlink(target, destination).map_err(|error| {
            MobileLinuxError::Io(format!(
                "copy LSP workspace symlink {}: {error}",
                source.display()
            ))
        })?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|error| {
            MobileLinuxError::Io(format!(
                "create LSP workspace snapshot {}: {error}",
                destination.display()
            ))
        })?;
        for entry in fs::read_dir(source).map_err(|error| {
            MobileLinuxError::Io(format!("read LSP workspace {}: {error}", source.display()))
        })? {
            let entry = entry.map_err(|error| MobileLinuxError::Io(error.to_string()))?;
            copy_raw_stdio_snapshot(
                &entry.path(),
                &destination.join(entry.file_name()),
                cancelled,
            )?;
        }
        fs::set_permissions(destination, metadata.permissions()).map_err(|error| {
            MobileLinuxError::Io(format!("preserve LSP snapshot permissions: {error}"))
        })?;
        return Ok(());
    }
    if metadata.is_file() {
        (|| -> std::io::Result<()> {
            let mut input = fs::File::open(source)?;
            let mut output = fs::File::create(destination)?;
            let mut bytes = [0; 64 * 1024];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "snapshot cancelled",
                    ));
                }
                let count = std::io::Read::read(&mut input, &mut bytes)?;
                if count == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &bytes[..count])?;
            }
            Ok(())
        })()
        .map_err(|error| {
            MobileLinuxError::Io(format!(
                "copy LSP workspace file {}: {error}",
                source.display()
            ))
        })?;
        fs::set_permissions(destination, metadata.permissions()).map_err(|error| {
            MobileLinuxError::Io(format!("preserve LSP snapshot permissions: {error}"))
        })?;
        return Ok(());
    }
    Err(MobileLinuxError::InvalidRequest(format!(
        "unsupported special file in LSP workspace snapshot: {}",
        source.display()
    )))
}

fn snapshot_read_only_mounts(
    mounts: Vec<MountSpec>,
    snapshot_root: &Path,
    cancelled: &AtomicBool,
) -> Result<(Vec<MountSpec>, Vec<PathBuf>), MobileLinuxError> {
    check_snapshot_cancelled(cancelled)?;
    if snapshot_root.exists() {
        fs::remove_dir_all(snapshot_root).map_err(|error| {
            MobileLinuxError::Io(format!("clear stale LSP workspace snapshot: {error}"))
        })?;
    }
    let mut prepared = Vec::with_capacity(mounts.len());
    let mut roots = Vec::new();
    for (index, mut mount) in mounts.into_iter().enumerate() {
        check_snapshot_cancelled(cancelled)?;
        if mount.read_only {
            let destination = snapshot_root.join(index.to_string());
            let node_modules = mount.host_path.join("node_modules");
            if mount.host_path.is_dir() {
                fs::create_dir_all(&destination).map_err(|error| {
                    MobileLinuxError::Io(format!(
                        "create LSP workspace snapshot {}: {error}",
                        destination.display()
                    ))
                })?;
                for entry in fs::read_dir(&mount.host_path).map_err(|error| {
                    MobileLinuxError::Io(format!(
                        "read LSP workspace {}: {error}",
                        mount.host_path.display()
                    ))
                })? {
                    let entry = entry.map_err(|error| MobileLinuxError::Io(error.to_string()))?;
                    if entry.file_name() == "node_modules" {
                        continue;
                    }
                    copy_raw_stdio_snapshot(
                        &entry.path(),
                        &destination.join(entry.file_name()),
                        cancelled,
                    )?;
                }
            } else {
                copy_raw_stdio_snapshot(&mount.host_path, &destination, cancelled)?;
            }
            let guest_root = mount.guest_path.clone();
            mount.host_path = destination;
            mount.read_only = false;
            roots.push(snapshot_root.to_path_buf());
            prepared.push(mount);
            if node_modules.is_dir() {
                prepared.push(MountSpec {
                    host_path: node_modules,
                    guest_path: format!("{guest_root}/node_modules"),
                    read_only: false,
                    purpose: MountPurpose::External,
                });
            }
            continue;
        }
        prepared.push(mount);
    }
    roots.sort();
    roots.dedup();
    Ok((prepared, roots))
}

#[derive(Clone, Copy)]
enum ForegroundMountMode {
    Merged,
    RequestOnly,
    ExplicitOnly,
}

struct RuntimeState {
    config: AndroidProotRuntimeConfig,
    mounts: RwLock<Vec<MountSpec>>,
    tasks: Mutex<HashMap<String, Arc<TaskControl>>>,
    ptys: Mutex<HashMap<String, Arc<PtyControl>>>,
    raw_stdio: Mutex<HashMap<String, Arc<RawStdioControl>>>,
    events: Mutex<VecDeque<MobileLinuxEvent>>,
    next_id: AtomicU64,
    next_sequence: AtomicU64,
    booted: AtomicBool,
    #[cfg(test)]
    receipt_start_barrier: Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

#[derive(Clone)]
/// Process, PTY, mount, event, and rootfs lifecycle owner for Android PRoot.
pub struct AndroidProotRuntime {
    state: Arc<RuntimeState>,
}

impl std::fmt::Debug for AndroidProotRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AndroidProotRuntime")
            .field("config", &self.state.config)
            .field("booted", &self.state.booted.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl AndroidProotRuntime {
    /// Construct a stopped runtime. No filesystem state is mutated until boot.
    #[must_use]
    pub fn new(config: AndroidProotRuntimeConfig) -> Self {
        Self {
            state: Arc::new(RuntimeState {
                config,
                mounts: RwLock::new(Vec::new()),
                tasks: Mutex::new(HashMap::new()),
                ptys: Mutex::new(HashMap::new()),
                raw_stdio: Mutex::new(HashMap::new()),
                events: Mutex::new(VecDeque::new()),
                next_id: AtomicU64::new(1),
                next_sequence: AtomicU64::new(1),
                booted: AtomicBool::new(false),
                #[cfg(test)]
                receipt_start_barrier: Mutex::new(None),
            }),
        }
    }

    fn next_id(&self, prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            self.state.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn emit(&self, task_id: Option<String>, kind: MobileLinuxEventKind) {
        let mut events = self.state.events.lock().expect("mobile-linux events mutex");
        events.push_back(MobileLinuxEvent {
            sequence: self.state.next_sequence.fetch_add(1, Ordering::Relaxed),
            task_id,
            kind,
        });
        while events.len() > MAX_EVENTS {
            events.pop_front();
        }
    }

    fn proot_binary(&self) -> Result<PathBuf, MobileLinuxError> {
        let root = &self.state.config.managed_root;
        let mut candidates = vec![
            root.join("bin/libproot.so"),
            root.join("libproot.so"),
            root.join("bin/proot"),
        ];
        if let Some(native_lib_dir) = self.state.config.native_library_dir.clone() {
            candidates.insert(0, native_lib_dir.join("libproot.so"));
        }
        candidates
            .into_iter()
            .find(|candidate| executable_regular_file(candidate))
            .ok_or_else(|| {
                MobileLinuxError::Unavailable(format!(
                    "PRoot executable is missing under {}",
                    root.display()
                ))
            })
    }

    fn policy_launcher(&self) -> Result<PathBuf, MobileLinuxError> {
        let mut candidates = vec![
            self.state
                .config
                .managed_root
                .join("bin/libmobile_linux_policy_launcher.so"),
            self.state
                .config
                .managed_root
                .join("libmobile_linux_policy_launcher.so"),
        ];
        if let Some(native_lib_dir) = self.state.config.native_library_dir.clone() {
            candidates.insert(0, native_lib_dir.join("libmobile_linux_policy_launcher.so"));
        }
        candidates
            .into_iter()
            .find(|candidate| executable_regular_file(candidate))
            .ok_or_else(|| {
                MobileLinuxError::NetworkPolicyUnavailable(
                    "Android network policy launcher is not packaged or executable".to_string(),
                )
            })
    }

    fn checked_active_root(&self) -> Result<PathBuf, MobileLinuxError> {
        let root = self.state.config.active_root();
        let metadata = fs::symlink_metadata(&root).map_err(|error| {
            MobileLinuxError::Unavailable(format!(
                "active rootfs is missing at {}: {error}",
                root.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(MobileLinuxError::Integrity(
                "active rootfs must be a real directory".to_string(),
            ));
        }
        let shell = root.join("bin/sh");
        if !executable_regular_file(&shell) {
            return Err(MobileLinuxError::Integrity(format!(
                "rootfs shell is missing or not executable: {}",
                shell.display()
            )));
        }
        Ok(root)
    }

    fn readiness(&self) -> Result<(PathBuf, PathBuf), MobileLinuxError> {
        Ok((self.proot_binary()?, self.checked_active_root()?))
    }

    fn prepare_execution(&self) -> Result<(), MobileLinuxError> {
        self.readiness()?;
        fs::create_dir_all(self.state.config.managed_root.join("tmp"))
            .map_err(|error| MobileLinuxError::Io(format!("create PRoot tmp: {error}")))?;
        self.state.booted.store(true, Ordering::Release);
        Ok(())
    }

    fn rootfs_store(&self) -> RootfsStore {
        RootfsStore::new(
            self.state.config.managed_root.clone(),
            SandboxBackend::AndroidProot,
            MobileLinuxRuntimeMode::MobileLinux,
            "android",
            self.state.config.abi.clone(),
        )
    }

    fn rootfs_manifest_paths(&self) -> [PathBuf; 2] {
        [
            self.state.config.managed_root.join("rootfs-manifest.json"),
            self.state.config.active_root().join("rootfs-manifest.json"),
        ]
    }

    fn load_rootfs_manifest(&self) -> Result<RootfsManifest, MobileLinuxError> {
        let paths = self.rootfs_manifest_paths();
        for path in &paths {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(MobileLinuxError::Io(format!(
                        "read rootfs manifest metadata {}: {error}",
                        path.display()
                    )))
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(MobileLinuxError::Integrity(format!(
                    "rootfs manifest must be a regular file: {}",
                    path.display()
                )));
            }
            let bytes = fs::read(path).map_err(|error| {
                MobileLinuxError::Io(format!("read rootfs manifest {}: {error}", path.display()))
            })?;
            let manifest = serde_json::from_slice::<RootfsManifest>(&bytes).map_err(|error| {
                MobileLinuxError::Integrity(format!(
                    "parse rootfs manifest {}: {error}",
                    path.display()
                ))
            })?;
            return Ok(manifest);
        }
        Err(MobileLinuxError::Unavailable(format!(
            "rootfs manifest is missing (looked for {} and {})",
            paths[0].display(),
            paths[1].display()
        )))
    }

    fn execution_mounts(
        &self,
        request_mounts: &[MountSpec],
        mode: ForegroundMountMode,
    ) -> Result<Vec<MountSpec>, MobileLinuxError> {
        if matches!(mode, ForegroundMountMode::RequestOnly) {
            validate_isolated_local_app_mounts(
                request_mounts,
                &self.state.config.managed_root,
                &self.state.config.app_sandbox_root,
                self.state
                    .config
                    .isolated_build_profile
                    .as_ref()
                    .ok_or_else(|| {
                        MobileLinuxError::InvalidRequest(
                            "isolated build profile is not configured".into(),
                        )
                    })?,
            )?;
        }
        let mut mounts = match mode {
            ForegroundMountMode::Merged => self
                .state
                .mounts
                .read()
                .expect("mobile-linux mounts rwlock")
                .clone(),
            ForegroundMountMode::RequestOnly | ForegroundMountMode::ExplicitOnly => {
                Vec::with_capacity(request_mounts.len())
            }
        };
        for mount in request_mounts {
            validate_mount(mount, &self.state.config.managed_root)?;
            mounts.retain(|existing| existing.guest_path != mount.guest_path);
            mounts.push(mount.clone());
        }
        Ok(mounts)
    }

    async fn spawn_child_with_mounts(
        &self,
        request: &LinuxCommandRequest,
        mounts: &[MountSpec],
        isolated_local_app_build_mounts: Option<&[MountSpec]>,
    ) -> Result<SpawnedChild, MobileLinuxError> {
        validate_request(
            request,
            isolated_local_app_build_mounts,
            self.state.config.isolated_build_profile.as_ref(),
        )?;
        let memory_limit_bytes = requested_memory_limit_bytes(request)?;
        if memory_limit_bytes.is_some() {
            ensure_process_group_rss_available()?;
        }
        let receipt_policy = enforced_network_policy_name(request.network);
        let receipt_path = if receipt_policy.is_some() {
            let path = self.state.config.managed_root.join("tmp").join(format!(
                "network-policy-receipt-{}",
                self.state.next_id.fetch_add(1, Ordering::Relaxed)
            ));
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| {
                    MobileLinuxError::Io(format!(
                        "create network enforcement receipt {}: {error}",
                        path.display()
                    ))
                })?;
            Some(path)
        } else {
            None
        };
        let mut command = match self.build_command(
            request,
            request.cwd.as_deref(),
            &request.env,
            mounts,
            receipt_path.as_deref(),
        ) {
            Ok(command) => command,
            Err(error) => {
                if let Some(path) = receipt_path.as_deref() {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = receipt_path.as_deref() {
                    let _ = fs::remove_file(path);
                }
                return Err(MobileLinuxError::Io(format!("spawn PRoot: {error}")));
            }
        };
        let network_policy_enforced = if let Some(path) = receipt_path.as_deref() {
            #[cfg(test)]
            {
                let barrier = self.state.receipt_start_barrier.lock().unwrap().clone();
                if let Some(barrier) = barrier {
                    barrier.wait().await;
                }
            }
            match wait_for_network_policy_receipt(
                &mut child,
                path,
                receipt_policy.expect("restricted policy has receipt name"),
            )
            .await
            {
                Ok(()) => true,
                Err(error) => {
                    if let Some(pid) = child.id() {
                        terminate_group(pid, Signal::SIGKILL);
                    }
                    let _ = child.wait().await;
                    let _ = fs::remove_file(path);
                    return Err(error);
                }
            }
        } else {
            false
        };
        if let Some(path) = receipt_path {
            let _ = fs::remove_file(path);
        }
        Ok(SpawnedChild {
            child,
            enforcement: LinuxEnforcementReceipt {
                network_policy_enforced,
                memory_limit_enforced: memory_limit_bytes.is_some(),
            },
            memory_limit_bytes,
        })
    }

    fn build_command(
        &self,
        request: &LinuxCommandRequest,
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
        mounts: &[MountSpec],
        receipt_path: Option<&Path>,
    ) -> Result<Command, MobileLinuxError> {
        let (proot, rootfs) = self.readiness()?;
        let native_lib_dir = proot.parent().map(Path::to_path_buf);
        let mut proot_args = vec![
            "-0".to_string(),
            "--link2symlink".to_string(),
            "-r".to_string(),
            rootfs.display().to_string(),
            "-b".to_string(),
            "/dev".to_string(),
            "-b".to_string(),
            "/proc".to_string(),
            "-b".to_string(),
            "/sys".to_string(),
            "-w".to_string(),
            cwd.unwrap_or("/root").to_string(),
        ];
        if matches!(request.network, NetworkPolicy::LoopbackOnly) {
            // This enables LingXi's sockaddr-aware extension in the pinned
            // PRoot build. The extension, not the outer launcher, publishes
            // the LoopbackOnly enforcement receipt after initialization.
            proot_args.push("-p".to_string());
        }
        for mount in mounts {
            proot_args.push("-b".to_string());
            proot_args.push(format!(
                "{}:{}",
                mount.host_path.display(),
                mount.guest_path
            ));
        }
        proot_args.push(request.command.clone());
        proot_args.extend(request.args.iter().cloned());

        let mut command = match enforced_network_policy_name(request.network) {
            None => Command::new(&proot),
            Some(policy) => {
                let launcher = self.policy_launcher()?;
                let mut command = Command::new(launcher);
                command.arg(policy).arg(&proot);
                command
            }
        };
        command
            .args(proot_args)
            .env_clear()
            .env("PROOT_TMP_DIR", self.state.config.managed_root.join("tmp"))
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/bin",
            )
            .env("HOME", "/root")
            .envs(env);
        if let Some(receipt_path) = receipt_path {
            command.env("LINGXI_ENFORCEMENT_RECEIPT_PATH", receipt_path);
        }
        if let Some(native_lib_dir) = native_lib_dir {
            command.env("LD_LIBRARY_PATH", &native_lib_dir);
            let loader = native_lib_dir.join("libproot-loader.so");
            if executable_regular_file(&loader) {
                command.env("PROOT_LOADER", loader);
            }
            let loader32 = native_lib_dir.join("libproot-loader32.so");
            if executable_regular_file(&loader32) {
                command.env("PROOT_LOADER_32", loader32);
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        Ok(command)
    }

    fn build_pty_invocation(
        &self,
        executable: &str,
        executable_args: &[String],
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
        mounts: &[MountSpec],
    ) -> Result<(String, Vec<String>, HashMap<String, String>), MobileLinuxError> {
        let (proot, rootfs) = self.readiness()?;
        let mut args = vec![
            "-0".to_string(),
            "--link2symlink".to_string(),
            "-r".to_string(),
            rootfs.display().to_string(),
            "-b".to_string(),
            "/dev".to_string(),
            "-b".to_string(),
            "/proc".to_string(),
            "-b".to_string(),
            "/sys".to_string(),
            "-w".to_string(),
            cwd.unwrap_or("/root").to_string(),
        ];
        for mount in mounts {
            args.push("-b".to_string());
            args.push(format!(
                "{}:{}",
                mount.host_path.display(),
                mount.guest_path
            ));
        }
        args.push(executable.to_string());
        args.extend(executable_args.iter().cloned());
        let mut child_env = HashMap::from([
            (
                "PROOT_TMP_DIR".to_string(),
                self.state
                    .config
                    .managed_root
                    .join("tmp")
                    .display()
                    .to_string(),
            ),
            (
                "PATH".to_string(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/bin".to_string(),
            ),
            ("HOME".to_string(), "/root".to_string()),
        ]);
        child_env.extend(env.iter().map(|(key, value)| (key.clone(), value.clone())));
        if let Some(native_lib_dir) = proot.parent() {
            child_env.insert(
                "LD_LIBRARY_PATH".to_string(),
                native_lib_dir.display().to_string(),
            );
            let loader = native_lib_dir.join("libproot-loader.so");
            if executable_regular_file(&loader) {
                child_env.insert("PROOT_LOADER".to_string(), loader.display().to_string());
            }
            let loader32 = native_lib_dir.join("libproot-loader32.so");
            if executable_regular_file(&loader32) {
                child_env.insert(
                    "PROOT_LOADER_32".to_string(),
                    loader32.display().to_string(),
                );
            }
        }
        Ok((proot.display().to_string(), args, child_env))
    }

    fn create_task(
        &self,
        prefix: &str,
        command: String,
        status: MobileLinuxTaskStatus,
    ) -> (String, Arc<TaskControl>) {
        let id = self.next_id(prefix);
        let task = Arc::new(TaskControl::new(id.clone(), command, status));
        self.state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .insert(id.clone(), task.clone());
        self.emit(
            Some(id.clone()),
            MobileLinuxEventKind::TaskStatusChanged {
                status,
                exit_code: None,
                detail: None,
            },
        );
        (id, task)
    }

    fn finish_task(
        &self,
        id: &str,
        task: &TaskControl,
        status: MobileLinuxTaskStatus,
        exit_code: Option<i32>,
        detail: Option<String>,
    ) {
        if task.terminal_emitted.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut snapshot = task.snapshot.lock().expect("task snapshot mutex");
        snapshot.status = status;
        snapshot.finished_at_ms = Some(now_ms());
        snapshot.exit_code = exit_code;
        snapshot.detail.clone_from(&detail);
        drop(snapshot);
        self.emit(
            Some(id.to_string()),
            MobileLinuxEventKind::TaskStatusChanged {
                status,
                exit_code,
                detail,
            },
        );
    }

    async fn spawn_child(
        &self,
        request: &LinuxCommandRequest,
    ) -> Result<SpawnedChild, MobileLinuxError> {
        let mounts = self.execution_mounts(&request.mounts, ForegroundMountMode::Merged)?;
        self.spawn_child_with_mounts(request, &mounts, None).await
    }

    async fn run_inner(
        &self,
        request: LinuxCommandRequest,
        sink: Option<Arc<dyn ProcessStreamSink>>,
        mount_mode: ForegroundMountMode,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.prepare_execution()?;
        let mounts = self.execution_mounts(&request.mounts, mount_mode)?;
        let (id, task) = self.create_task(
            "task",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let mut cancellation = CallerCancellation {
            task: task.clone(),
            stop,
            armed: true,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        // Foreign callers may cancel the future while input is blocked or while
        // the policy launcher is starting. The owner must survive to reap every
        // child and publish a terminal task instead of detaching its drains.
        tokio::spawn(async move {
            let result = runtime
                .run_owned(request, sink, mount_mode, mounts, (&id, &task), stopped)
                .await;
            if let Err(error) = &result {
                runtime.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
            }
            let _ = reply.send(result);
        });
        let result = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("foreground process owner failed: {error}"))
        })?;
        cancellation.armed = false;
        result
    }

    async fn run_owned(
        &self,
        request: LinuxCommandRequest,
        sink: Option<Arc<dyn ProcessStreamSink>>,
        mount_mode: ForegroundMountMode,
        mounts: Vec<MountSpec>,
        task: (&str, &Arc<TaskControl>),
        mut stopped: tokio::sync::watch::Receiver<bool>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        let (id, task) = task;
        let isolated_local_app_build_mounts =
            matches!(mount_mode, ForegroundMountMode::RequestOnly).then_some(mounts.as_slice());
        let spawned = match self
            .spawn_child_with_mounts(&request, &mounts, isolated_local_app_build_mounts)
            .await
        {
            Ok(spawned) => spawned,
            Err(error) => {
                self.finish_task(
                    id,
                    task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let SpawnedChild {
            mut child,
            enforcement,
            memory_limit_bytes,
        } = spawned;
        let pid = child
            .id()
            .ok_or_else(|| MobileLinuxError::Io("PRoot child has no pid".to_string()))?;
        task.pid.store(u64::from(pid), Ordering::Release);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stdout unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stderr unavailable".to_string()))?;
        let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let deadline = tokio::time::Instant::now() + timeout;
        let stream_timed_out = Arc::new(AtomicBool::new(false));
        let (stop_delivery, delivery_stopped) = tokio::sync::watch::channel(false);
        let delivery = StreamDelivery {
            task: task.clone(),
            stopped: delivery_stopped,
            deadline,
            timed_out: stream_timed_out.clone(),
        };
        let stdout_delivery = delivery.clone();
        let stdout_runtime = self.clone();
        let stdout_id = id.to_owned();
        let stdout_sink = sink.clone();
        let stdout_task = tokio::spawn(async move {
            read_stdout(stdout, move |line| {
                stdout_runtime.emit(
                    Some(stdout_id.clone()),
                    MobileLinuxEventKind::StdoutLine { line: line.clone() },
                );
                let sink = stdout_sink.clone();
                let mut delivery = stdout_delivery.clone();
                async move {
                    if let Some(sink) = sink {
                        delivery.deliver(sink.stdout_line(line)).await?;
                    }
                    Ok(())
                }
            })
            .await
        });
        let stderr_runtime = self.clone();
        let stderr_id = id.to_owned();
        let stderr_task = tokio::spawn(async move {
            read_stderr(stderr, move |chunk| {
                stderr_runtime.emit(
                    Some(stderr_id.clone()),
                    MobileLinuxEventKind::StderrChunk {
                        chunk: chunk.clone(),
                    },
                );
                let sink = sink.clone();
                let mut delivery = delivery.clone();
                async move {
                    if let Some(sink) = sink {
                        delivery.deliver(sink.stderr_chunk(chunk)).await?;
                    }
                    Ok(())
                }
            })
            .await
        });
        // Both output drains are live before input can fill either pipe. The
        // writer runs concurrently with timeout and resident-memory enforcement.
        let stdin_writer = StdinWriter::start(child.stdin.take(), request.stdin);
        let waited = if task.cancel_requested.load(Ordering::Acquire) {
            terminate_and_reap(&mut child, pid)
                .await
                .map(ChildWaitOutcome::Exited)
        } else {
            tokio::select! {
                biased;
                _ = wait_raw_stop(&mut stopped) => {
                    terminate_and_reap(&mut child, pid).await.map(ChildWaitOutcome::Exited)
                }
                result = wait_for_child(&mut child, pid, Some(deadline.saturating_duration_since(tokio::time::Instant::now())), memory_limit_bytes) => result,
            }
        };
        // Interrupted execution must not wait for foreign callbacks to return.
        // Readers continue capturing/draining bytes after delivery is cancelled.
        // A normal child exit does not truncate a slow, still-valid sink.
        if task.cancel_requested.load(Ordering::Acquire)
            || !matches!(&waited, Ok(ChildWaitOutcome::Exited(_)))
        {
            stop_delivery.send_replace(true);
        }
        let stdin_result = stdin_writer.finish().await;
        let (exit_code, mut timed_out, memory_limit_exceeded) = match waited {
            Ok(ChildWaitOutcome::Exited(status)) => (status.code().unwrap_or(-1), false, None),
            Ok(ChildWaitOutcome::TimedOut) => (-1, true, None),
            Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic)) => (-1, false, Some(diagnostic)),
            Err(error) => {
                terminate_group(pid, Signal::SIGKILL);
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                let _ = tokio::join!(stdout_task, stderr_task);
                self.finish_task(
                    id,
                    task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let stdout = join_reader(stdout_task, "stdout").await?;
        let stderr = join_reader(stderr_task, "stderr").await?;
        timed_out |= stream_timed_out.load(Ordering::Acquire);
        let cancelled = task.cancel_requested.load(Ordering::Acquire);
        if exit_code == 0 && !cancelled && !timed_out {
            if let Err(error) = stdin_result {
                self.finish_task(
                    id,
                    task,
                    MobileLinuxTaskStatus::Failed,
                    Some(exit_code),
                    Some(error.to_string()),
                );
                return Err(error);
            }
        }
        let status = if memory_limit_exceeded.is_some() {
            MobileLinuxTaskStatus::Failed
        } else if cancelled {
            MobileLinuxTaskStatus::Cancelled
        } else if timed_out {
            MobileLinuxTaskStatus::TimedOut
        } else if exit_code == 0 {
            MobileLinuxTaskStatus::Completed
        } else {
            MobileLinuxTaskStatus::Failed
        };
        self.finish_task(
            id,
            task,
            status,
            (!timed_out && !cancelled).then_some(exit_code),
            if let Some(diagnostic) = &memory_limit_exceeded {
                Some(diagnostic.detail())
            } else if cancelled {
                Some("command cancelled".to_string())
            } else {
                timed_out.then(|| "command timed out".to_string())
            },
        );
        if let Some(diagnostic) = memory_limit_exceeded {
            return Err(MobileLinuxError::ResourceLimitExceeded(
                diagnostic.summary(),
            ));
        }
        Ok(LinuxCommandResult {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
            timed_out,
            cancelled,
            enforcement,
        })
    }

    async fn spawn_background_owned(
        &self,
        request: LinuxCommandRequest,
        id: String,
        task: Arc<TaskControl>,
    ) -> Result<LinuxProcessHandle, MobileLinuxError> {
        let spawned = match self.spawn_child(&request).await {
            Ok(spawned) => spawned,
            Err(error) => {
                self.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let SpawnedChild {
            mut child,
            enforcement,
            memory_limit_bytes,
        } = spawned;
        let pid = child
            .id()
            .ok_or_else(|| MobileLinuxError::Io("PRoot child has no pid".to_string()))?;
        task.pid.store(u64::from(pid), Ordering::Release);
        if task.cancel_requested.load(Ordering::Acquire) {
            terminate_and_reap(&mut child, pid).await?;
            return Err(MobileLinuxError::InvalidRequest(
                "process startup cancelled".into(),
            ));
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let runtime = self.clone();
        let reaper_id = id.clone();
        tokio::spawn(async move {
            let stdout_task = stdout.map(|stdout| {
                let rt = runtime.clone();
                let stream_id = reaper_id.clone();
                tokio::spawn(async move {
                    read_stdout(stdout, move |line| {
                        rt.emit(
                            Some(stream_id.clone()),
                            MobileLinuxEventKind::StdoutLine { line },
                        );
                        async { Ok(()) }
                    })
                    .await
                })
            });
            let stderr_task = stderr.map(|stderr| {
                let rt = runtime.clone();
                let stream_id = reaper_id.clone();
                tokio::spawn(async move {
                    read_stderr(stderr, move |chunk| {
                        rt.emit(
                            Some(stream_id.clone()),
                            MobileLinuxEventKind::StderrChunk { chunk },
                        );
                        async { Ok(()) }
                    })
                    .await
                })
            });
            let stdin_writer = StdinWriter::start(child.stdin.take(), request.stdin);
            let result = wait_for_child(
                &mut child,
                pid,
                request.timeout_ms.map(Duration::from_millis),
                memory_limit_bytes,
            )
            .await;
            let stdin_result = stdin_writer.finish().await;
            let stdout_result = match stdout_task {
                Some(task) => join_reader(task, "stdout").await,
                None => Ok(Vec::new()),
            };
            let stderr_result = match stderr_task {
                Some(task) => join_reader(task, "stderr").await,
                None => Ok(Vec::new()),
            };
            let reader_error = stdout_result
                .err()
                .or_else(|| stderr_result.err())
                .or_else(|| {
                    if matches!(&result, Ok(ChildWaitOutcome::Exited(status)) if status.success()) {
                        stdin_result.err()
                    } else {
                        None
                    }
                });
            let cancelled = task.cancel_requested.load(Ordering::Acquire);
            match (result, reader_error) {
                (_, Some(error)) => runtime.finish_task(
                    &reaper_id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                ),
                (Ok(ChildWaitOutcome::Exited(status)), None) => {
                    let code = status.code().unwrap_or(-1);
                    runtime.finish_task(
                        &reaper_id,
                        &task,
                        if cancelled {
                            MobileLinuxTaskStatus::Cancelled
                        } else if code == 0 {
                            MobileLinuxTaskStatus::Completed
                        } else {
                            MobileLinuxTaskStatus::Failed
                        },
                        Some(code),
                        cancelled.then(|| "task cancelled".to_string()),
                    );
                }
                (Ok(ChildWaitOutcome::TimedOut), None) => runtime.finish_task(
                    &reaper_id,
                    &task,
                    MobileLinuxTaskStatus::TimedOut,
                    None,
                    Some("command timed out".to_string()),
                ),
                (Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic)), None) => {
                    runtime.finish_task(
                        &reaper_id,
                        &task,
                        MobileLinuxTaskStatus::Failed,
                        None,
                        Some(diagnostic.detail()),
                    );
                }
                (Err(error), None) => runtime.finish_task(
                    &reaper_id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                ),
            }
        });
        Ok(LinuxProcessHandle { id, enforcement })
    }

    async fn open_raw_stdio_owned(
        &self,
        request: RawStdioOpenRequest,
        id: String,
        task: Arc<TaskControl>,
    ) -> Result<RawStdioSessionHandle, MobileLinuxError> {
        let snapshot_root = self
            .state
            .config
            .app_sandbox_root
            .join("cache/raw-stdio-snapshots")
            .join(&id);
        let mut snapshot_cleanup = SnapshotCleanup(Some(snapshot_root.clone()));
        let snapshot_root_for_copy = snapshot_root.clone();
        let copy_task = task.clone();
        let request_mounts = request.mounts.clone();
        // 0 = queued, 1 = running, 2 = cancelled before admission. Tokio's
        // blocking pool does not resolve even an aborted queued job until a
        // worker becomes free, so its admission must be revocable separately.
        let copy_phase = Arc::new(AtomicU8::new(0));
        let worker_phase = copy_phase.clone();
        let mut copy = tokio::task::spawn_blocking(move || {
            if worker_phase
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "snapshot cancelled before admission".into(),
                ));
            }
            snapshot_read_only_mounts(
                request_mounts,
                &snapshot_root_for_copy,
                &copy_task.cancel_requested,
            )
        });
        let snapshot_result = tokio::select! {
            result = &mut copy => result,
            _ = async {
                while !task.cancel_requested.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {
                if copy_phase.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    copy.abort();
                    // The revoked closure cannot write even if the pool later
                    // dequeues it. Releasing the owner and path guard is safe.
                    Ok(Err(MobileLinuxError::InvalidRequest("snapshot cancelled before admission".into())))
                } else {
                    copy.abort();
                    copy.await
                }
            }
        }.map_err(|error| MobileLinuxError::Io(format!("join LSP workspace snapshot: {error}")))?;
        let (prepared_mounts, snapshot_roots) = match snapshot_result {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_dir_all(&snapshot_root);
                self.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        check_snapshot_cancelled(&task.cancel_requested)?;
        let mounts =
            match self.execution_mounts(&prepared_mounts, ForegroundMountMode::ExplicitOnly) {
                Ok(mounts) => mounts,
                Err(error) => {
                    for root in &snapshot_roots {
                        let _ = fs::remove_dir_all(root);
                    }
                    self.finish_task(
                        &id,
                        &task,
                        if task.cancel_requested.load(Ordering::Acquire) {
                            MobileLinuxTaskStatus::Cancelled
                        } else {
                            MobileLinuxTaskStatus::Failed
                        },
                        None,
                        Some(error.to_string()),
                    );
                    return Err(error);
                }
            };
        let command_request = LinuxCommandRequest {
            command: request.command,
            args: request.args,
            cwd: request.cwd,
            env: request.env,
            stdin: None,
            timeout_ms: None,
            network: request.network,
            resource_limits: request.resource_limits,
            mounts: mounts.clone(),
        };
        let spawned = match self
            .spawn_child_with_mounts(&command_request, &mounts, None)
            .await
        {
            Ok(spawned) => spawned,
            Err(error) => {
                for root in &snapshot_roots {
                    let _ = fs::remove_dir_all(root);
                }
                self.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let SpawnedChild {
            mut child,
            enforcement,
            memory_limit_bytes,
        } = spawned;
        let pid = child
            .id()
            .ok_or_else(|| MobileLinuxError::Io("PRoot child has no pid".to_string()))?;
        task.pid.store(u64::from(pid), Ordering::Release);
        if task.cancel_requested.load(Ordering::Acquire) {
            terminate_and_reap(&mut child, pid).await?;
            return Err(MobileLinuxError::InvalidRequest(
                "process startup cancelled".into(),
            ));
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stdin unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stdout unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stderr unavailable".to_string()))?;
        let buffers = Arc::new(Mutex::new(RawStdioBuffers::default()));
        let (writes, receiver) = tokio::sync::mpsc::channel(1);
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let control = Arc::new(RawStdioControl {
            task: task.clone(),
            writes,
            stop,
            worker: Mutex::new(None),
            buffers: buffers.clone(),
            snapshot_roots,
        });
        let stdout_task = tokio::spawn(read_raw_stream(stdout, {
            let buffers = buffers.clone();
            move |chunk| {
                let buffers = buffers.clone();
                async move {
                    let mut guard = buffers.lock().expect("raw stdio stdout mutex");
                    if !Self::append_raw_stdio_bytes(&mut guard.stdout, &chunk) {
                        guard.overflowed = true;
                    }
                    Ok(())
                }
            }
        }));
        let stderr_task = tokio::spawn(read_raw_stream(stderr, {
            let buffers = buffers.clone();
            move |chunk| {
                let buffers = buffers.clone();
                async move {
                    let mut guard = buffers.lock().expect("raw stdio stderr mutex");
                    if !Self::append_raw_stdio_bytes(&mut guard.stderr, &chunk) {
                        guard.overflowed = true;
                    }
                    Ok(())
                }
            }
        }));
        let writer = tokio::spawn(write_raw_stream(stdin, receiver, stopped.clone()));
        let runtime = self.clone();
        let reaper_id = id.clone();
        let worker_control = control.clone();
        let worker = tokio::spawn(async move {
            let mut stopped = stopped;
            // The same process-group watchdog used by run/background execution
            // owns the child for the whole raw session; receipt and enforcement
            // therefore describe the same configured limit.
            let outcome = tokio::select! {
                result = wait_for_child(&mut child, pid, None, memory_limit_bytes) => result,
                _ = wait_raw_stop(&mut stopped) => {
                    terminate_and_reap(&mut child, pid).await.map(ChildWaitOutcome::Exited)
                }
            };
            if outcome.is_err() {
                let _ = terminate_and_reap(&mut child, pid).await;
            }
            worker_control.stop.send_replace(true);
            let writer_result = writer
                .await
                .map_err(|error| MobileLinuxError::Io(format!("raw stdio writer failed: {error}")));
            let (stdout_result, stderr_result) = tokio::join!(
                join_raw_reader(stdout_task, pid, "stdout"),
                join_raw_reader(stderr_task, pid, "stderr"),
            );
            let cancelled = task.cancel_requested.load(Ordering::Acquire);
            let (status, exit_code, mut error, detail) = match outcome {
                Ok(ChildWaitOutcome::Exited(status)) => (
                    if cancelled {
                        MobileLinuxTaskStatus::Cancelled
                    } else if status.success() {
                        MobileLinuxTaskStatus::Completed
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    Some(status.code().unwrap_or(-1)),
                    None,
                    cancelled.then(|| "raw stdio session closed".to_string()),
                ),
                Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic)) => (
                    MobileLinuxTaskStatus::Failed,
                    Some(-1),
                    Some(MobileLinuxError::ResourceLimitExceeded(
                        diagnostic.summary(),
                    )),
                    Some(diagnostic.detail()),
                ),
                Ok(ChildWaitOutcome::TimedOut) => (
                    MobileLinuxTaskStatus::TimedOut,
                    Some(-1),
                    Some(MobileLinuxError::Timeout),
                    Some("raw stdio session timed out".to_string()),
                ),
                Err(error) => (MobileLinuxTaskStatus::Failed, Some(-1), Some(error), None),
            };
            error = error
                .or_else(|| writer_result.err())
                .or_else(|| stdout_result.err())
                .or_else(|| stderr_result.err());
            let detail = detail.or_else(|| error.as_ref().map(ToString::to_string));
            runtime.finish_task(
                &reaper_id,
                &task,
                if error.is_some() {
                    MobileLinuxTaskStatus::Failed
                } else {
                    status
                },
                exit_code,
                detail,
            );
            // Publish closure only after the child is reaped and BOTH readers
            // have reached EOF (or their explicit errors have been recorded).
            let mut guard = buffers.lock().expect("raw stdio buffers mutex");
            guard.exit_code = exit_code;
            guard.error = error.clone();
            guard.closed = true;
            error.map_or(Ok(()), Err)
        });
        *control.worker.lock().expect("raw stdio worker mutex") = Some(worker);
        self.state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .insert(id.clone(), control);
        snapshot_cleanup.0 = None;
        Ok(RawStdioSessionHandle { id, enforcement })
    }

    async fn close_raw_stdio_if_present(&self, id: &str) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("raw stdio mutex")
            .remove(id);
        if let Some(control) = control {
            Self::dispose_raw_control(control).await?;
        }
        Ok(())
    }

    async fn dispose_raw_control(control: Arc<RawStdioControl>) -> Result<(), MobileLinuxError> {
        // Never wait on a writer's pipe or mutex before signalling the child.
        // The owner task cancels stdin, reaps the process and joins both drains.
        control.task.cancel_requested.store(true, Ordering::Release);
        if !control.task.terminal_emitted.load(Ordering::Acquire) {
            let pid = control.task.pid.load(Ordering::Acquire) as u32;
            terminate_group(pid, Signal::SIGTERM);
        }
        control.stop.send_replace(true);
        let worker = control
            .worker
            .lock()
            .expect("raw stdio worker mutex")
            .take();
        let result = match worker {
            Some(worker) => worker.await.map_err(|error| {
                MobileLinuxError::Io(format!("raw stdio owner failed: {error}"))
            })?,
            None => Ok(()),
        };
        for root in &control.snapshot_roots {
            let _ = fs::remove_dir_all(root);
        }
        result
    }

    fn append_raw_stdio_bytes(queue: &mut VecDeque<u8>, chunk: &[u8]) -> bool {
        if queue.len().saturating_add(chunk.len()) > MAX_RAW_STDIO_BUFFER_BYTES {
            return false;
        }
        queue.extend(chunk.iter().copied());
        true
    }

    fn drain_raw_stdio_bytes(queue: &mut VecDeque<u8>, max_bytes: usize) -> Vec<u8> {
        queue.drain(..queue.len().min(max_bytes)).collect()
    }

    fn rootfs_snapshot(&self) -> RootfsStatus {
        if let Ok(manifest) = self.load_rootfs_manifest() {
            return self.rootfs_store().status(&manifest);
        }
        let active = self.state.config.active_root();
        let staged = self.state.config.staged_root();
        let root = self.checked_active_root();
        let proot = self.proot_binary();
        let (state, last_error) = match (&root, &proot) {
            (Ok(_), Ok(_)) => (RootfsState::Ready, None),
            (Err(error), _) if path_present(&active) => {
                (RootfsState::Corrupt, Some(error.to_string()))
            }
            (Err(error), _) if path_present(&staged) => {
                (RootfsState::Installing, Some(error.to_string()))
            }
            (Err(error), _) => (RootfsState::Missing, Some(error.to_string())),
            (_, Err(error)) => (RootfsState::Unsupported, Some(error.to_string())),
        };
        RootfsStatus {
            state,
            backend: SandboxBackend::AndroidProot,
            mode: MobileLinuxRuntimeMode::MobileLinux,
            platform: "android".to_string(),
            abi: self.state.config.abi.clone(),
            version: Some(self.state.config.rootfs_version.clone()),
            managed_root: Some(self.state.config.managed_root.clone()),
            active_root: path_present(&active).then_some(active.clone()),
            staged_root: path_present(&staged).then_some(staged),
            archive_sha256: self.state.config.archive_sha256.clone(),
            installed_size_bytes: directory_size(&active).ok(),
            writable_guest_paths: vec![
                "/root".to_string(),
                "/tmp".to_string(),
                "/var/tmp".to_string(),
            ],
            last_error,
        }
    }
}

#[async_trait]
impl MobileLinuxRuntime for AndroidProotRuntime {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::AndroidProot
    }

    fn mode(&self) -> MobileLinuxRuntimeMode {
        MobileLinuxRuntimeMode::MobileLinux
    }

    async fn probe_capability(&self) -> MobileLinuxCapability {
        let readiness = self.readiness();
        MobileLinuxCapability {
            available: readiness.is_ok(),
            backend: SandboxBackend::AndroidProot,
            mode: MobileLinuxRuntimeMode::MobileLinux,
            reason: readiness.err().map(|error| error.to_string()),
            streaming_output: true,
            background_processes: true,
            pty: true,
            bind_mounts: true,
            rootfs_integrity: self.load_rootfs_manifest().is_ok(),
        }
    }

    async fn boot(&self) -> Result<RootfsStatus, MobileLinuxError> {
        self.prepare_execution()?;
        Ok(self.rootfs_snapshot())
    }

    async fn shutdown(&self) -> Result<(), MobileLinuxError> {
        let pty_ids: HashSet<_> = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .keys()
            .cloned()
            .collect();
        let raw_stdio_ids: HashSet<_> = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .keys()
            .cloned()
            .collect();
        let task_ids: Vec<_> = self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .keys()
            .filter(|id| !pty_ids.contains(*id) && !raw_stdio_ids.contains(*id))
            .cloned()
            .collect();
        let mut errors = Vec::new();
        for id in pty_ids {
            if let Err(error) = self.close_pty(&PtySessionHandle { id }).await {
                errors.push(error.to_string());
            }
        }
        for id in raw_stdio_ids {
            if let Err(error) = self
                .close_raw_stdio(&RawStdioSessionHandle {
                    id,
                    enforcement: LinuxEnforcementReceipt::default(),
                })
                .await
            {
                errors.push(error.to_string());
            }
        }
        for id in task_ids {
            if let Err(error) = self
                .kill(&LinuxProcessHandle {
                    id,
                    enforcement: LinuxEnforcementReceipt::default(),
                })
                .await
            {
                errors.push(error.to_string());
            }
        }
        self.state.booted.store(false, Ordering::Release);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(MobileLinuxError::Io(format!(
                "shutdown reaping failed: {}",
                errors.join("; ")
            )))
        }
    }

    async fn run(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, None, ForegroundMountMode::Merged)
            .await
    }

    async fn run_isolated(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, None, ForegroundMountMode::RequestOnly)
            .await
    }

    async fn run_streaming(
        &self,
        request: LinuxCommandRequest,
        sink: Arc<dyn ProcessStreamSink>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, Some(sink), ForegroundMountMode::Merged)
            .await
    }

    async fn spawn_background(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxProcessHandle, MobileLinuxError> {
        self.prepare_execution()?;
        let (id, task) = self.create_task(
            "bg",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Backgrounded,
        );
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let mut cancellation = CallerCancellation {
            task: task.clone(),
            stop,
            armed: true,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        tokio::spawn(async move {
            let result = runtime
                .spawn_background_owned(request, id.clone(), task.clone())
                .await;
            if let Err(error) = &result {
                runtime.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
            }
            let handle = result.as_ref().ok().cloned();
            let delivered = reply.send(result).is_ok();
            if let Some(handle) = handle {
                // Sending the result is not acceptance: the caller can disappear
                // before polling it. Its guard closes the channel on acceptance
                // or publishes cancellation, leaving this owner to dispose it.
                let abandoned = task.cancel_requested.load(Ordering::Acquire)
                    || !delivered
                    || stopped.wait_for(|cancelled| *cancelled).await.is_ok();
                if abandoned || task.cancel_requested.load(Ordering::Acquire) {
                    task.cancel_requested.store(true, Ordering::Release);
                    if let Err(error) = runtime.kill(&handle).await {
                        runtime.emit(
                            Some(id),
                            MobileLinuxEventKind::RuntimeError {
                                detail: error.to_string(),
                            },
                        );
                    }
                }
            }
        });
        let result = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("process startup owner failed: {error}"))
        })?;
        cancellation.armed = false;
        result
    }

    async fn kill(&self, handle: &LinuxProcessHandle) -> Result<(), MobileLinuxError> {
        let task = self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("unknown task handle".to_string()))?;
        if task.terminal_emitted.load(Ordering::Acquire) {
            return self.close_raw_stdio_if_present(&handle.id).await;
        }
        task.cancel_requested.store(true, Ordering::Release);
        let starting = task.pid.load(Ordering::Acquire) == 0;
        // A foreground owner may still be waiting for its enforcement receipt.
        // Keep the cancellation request pending until it publishes the child;
        // never abandon an owned startup solely because its PID is not ready.
        let budget = REAP_BUDGET
            + if starting {
                ENFORCEMENT_RECEIPT_TIMEOUT
            } else {
                Duration::ZERO
            };
        let deadline = tokio::time::Instant::now() + budget;
        let mut hard_kill_at = None;
        let mut sent_sigkill = false;
        while !task.terminal_emitted.load(Ordering::Acquire) {
            let pid = u32::try_from(task.pid.load(Ordering::Acquire))
                .map_err(|_| MobileLinuxError::Io("invalid task pid".to_string()))?;
            if pid != 0 && hard_kill_at.is_none() {
                terminate_group(pid, Signal::SIGTERM);
                hard_kill_at = Some(tokio::time::Instant::now() + REAP_BUDGET / 2);
            }
            if !sent_sigkill && hard_kill_at.is_some_and(|at| tokio::time::Instant::now() >= at) {
                terminate_group(pid, Signal::SIGKILL);
                sent_sigkill = true;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MobileLinuxError::Io(format!(
                    "task {} did not reap within {} seconds",
                    handle.id,
                    budget.as_secs()
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.close_raw_stdio_if_present(&handle.id).await
    }

    async fn open_pty(
        &self,
        request: PtyOpenRequest,
    ) -> Result<PtySessionHandle, MobileLinuxError> {
        self.prepare_execution()?;
        validate_pty_request(&request)?;
        let mounts = self.execution_mounts(&request.mounts, ForegroundMountMode::Merged)?;
        let (program, args, env) = self.build_pty_invocation(
            &request.command,
            &request.args,
            request.cwd.as_deref(),
            &request.env,
            &mounts,
        )?;
        let (id, task) = self.create_task(
            "pty",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        let spawned = platform_pty::spawn_pty_process(
            &program,
            &args,
            &self.state.config.managed_root,
            &env,
            &None,
            platform_pty::TerminalSize {
                cols: request.size.cols,
                rows: request.size.rows,
            },
            &[],
        )
        .await;
        let spawned = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                let error = MobileLinuxError::Io(format!("spawn PRoot PTY: {error}"));
                self.finish_task(
                    &id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let process = Arc::new(spawned.session);
        if let Some(pid) = process.process_id() {
            task.pid.store(u64::from(pid), Ordering::Release);
        }
        let control = Arc::new(PtyControl {
            task: task.clone(),
            process: process.clone(),
        });
        self.state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .insert(id.clone(), control);
        let mut stdout = spawned.stdout_rx;
        let exit = spawned.exit_rx;
        let runtime = self.clone();
        let reaper_id = id.clone();
        tokio::spawn(async move {
            let output_runtime = runtime.clone();
            let output_id = reaper_id.clone();
            let output_task = tokio::spawn(async move {
                while let Some(data) = stdout.recv().await {
                    output_runtime.emit(
                        Some(output_id.clone()),
                        MobileLinuxEventKind::PtyOutput {
                            session_id: output_id.clone(),
                            data,
                        },
                    );
                }
            });
            let result = exit.await;
            let _ = output_task.await;
            let cancelled = task.cancel_requested.load(Ordering::Acquire);
            let (status, code, detail) = match result {
                Ok(code) => (
                    if cancelled {
                        MobileLinuxTaskStatus::Cancelled
                    } else if code == 0 {
                        MobileLinuxTaskStatus::Completed
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    Some(code),
                    cancelled.then(|| "PTY closed".to_string()),
                ),
                Err(error) => (
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(format!("PTY exit channel closed: {error}")),
                ),
            };
            runtime.finish_task(&reaper_id, &task, status, code, detail.clone());
            runtime.emit(
                Some(reaper_id.clone()),
                MobileLinuxEventKind::PtyClosed {
                    session_id: reaper_id.clone(),
                    exit_code: code,
                    detail,
                },
            );
            runtime
                .state
                .ptys
                .lock()
                .expect("mobile-linux PTY mutex")
                .remove(&reaper_id);
        });
        Ok(PtySessionHandle { id })
    }

    async fn open_raw_stdio(
        &self,
        request: RawStdioOpenRequest,
    ) -> Result<RawStdioSessionHandle, MobileLinuxError> {
        self.prepare_execution()?;
        let (id, task) = self.create_task(
            "stdio",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let mut cancellation = CallerCancellation {
            task: task.clone(),
            stop,
            armed: true,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        tokio::spawn(async move {
            let result = runtime
                .open_raw_stdio_owned(request, id.clone(), task.clone())
                .await;
            if let Err(error) = &result {
                runtime.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
            }
            let handle = result.as_ref().ok().cloned();
            let delivered = reply.send(result).is_ok();
            if let Some(handle) = handle {
                // Sending the result is not acceptance: the caller can disappear
                // before polling it. Its guard closes the channel on acceptance
                // or publishes cancellation, leaving this owner to dispose it.
                let abandoned = task.cancel_requested.load(Ordering::Acquire)
                    || !delivered
                    || stopped.wait_for(|cancelled| *cancelled).await.is_ok();
                if abandoned || task.cancel_requested.load(Ordering::Acquire) {
                    task.cancel_requested.store(true, Ordering::Release);
                    if let Err(error) = runtime.close_raw_stdio_if_present(&handle.id).await {
                        runtime.emit(
                            Some(id),
                            MobileLinuxEventKind::RuntimeError {
                                detail: error.to_string(),
                            },
                        );
                    }
                }
            }
        });
        let result = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("process startup owner failed: {error}"))
        })?;
        cancellation.armed = false;
        result
    }

    async fn write_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest("unknown raw stdio session".to_string())
            })?;
        let mut stopped = control.stop.subscribe();
        let (reply, response) = tokio::sync::oneshot::channel();
        tokio::select! {
            biased;
            _ = wait_raw_stop(&mut stopped) => Err(raw_stdin_closed()),
            result = async {
                control.writes.send(RawStdioWrite { input, reply }).await
                    .map_err(|_| raw_stdin_closed())?;
                response.await.map_err(|_| raw_stdin_closed())?
            } => result,
        }
    }

    async fn read_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        max_bytes: usize,
    ) -> Result<RawStdioReadResult, MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest("unknown raw stdio session".to_string())
            })?;
        let mut guard = control.buffers.lock().expect("raw stdio buffers mutex");
        if let Some(error) = &guard.error {
            return Err(error.clone());
        }
        if guard.overflowed {
            return Err(MobileLinuxError::Io(
                "raw stdio unread output exceeded 512 KiB".to_string(),
            ));
        }
        let stdout = Self::drain_raw_stdio_bytes(&mut guard.stdout, max_bytes);
        let stderr =
            Self::drain_raw_stdio_bytes(&mut guard.stderr, max_bytes.saturating_sub(stdout.len()));
        Ok(RawStdioReadResult {
            stdout,
            stderr,
            closed: guard.closed && guard.stdout.is_empty() && guard.stderr.is_empty(),
            exit_code: guard.exit_code,
        })
    }

    async fn close_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .remove(&handle.id)
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest("unknown raw stdio session".to_string())
            })?;
        Self::dispose_raw_control(control).await
    }

    async fn write_pty(
        &self,
        handle: &PtySessionHandle,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("stale PTY handle".to_string()))?;
        if input == [3] {
            return control
                .process
                .signal(platform_pty::ProcessSignal::Interrupt)
                .map_err(|error| MobileLinuxError::Io(format!("interrupt PTY: {error}")));
        }
        control
            .process
            .write(input)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("write PTY: {error}")))
    }

    async fn resize_pty(
        &self,
        handle: &PtySessionHandle,
        size: PtySize,
    ) -> Result<(), MobileLinuxError> {
        if size.cols == 0 || size.rows == 0 {
            return Err(MobileLinuxError::InvalidRequest(
                "PTY dimensions must be non-zero".to_string(),
            ));
        }
        let control = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("stale PTY handle".to_string()))?;
        control
            .process
            .resize(platform_pty::TerminalSize {
                cols: size.cols,
                rows: size.rows,
            })
            .map_err(|error| MobileLinuxError::Io(format!("resize PTY: {error}")))
    }

    async fn close_pty(&self, handle: &PtySessionHandle) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("stale PTY handle".to_string()))?;
        control.task.cancel_requested.store(true, Ordering::Release);
        control.process.close_stdin();
        let _ = control
            .process
            .signal(platform_pty::ProcessSignal::Terminate);
        let deadline = tokio::time::Instant::now() + REAP_BUDGET;
        let hard_kill_at = tokio::time::Instant::now() + REAP_BUDGET / 2;
        let mut forced = false;
        while !control.task.terminal_emitted.load(Ordering::Acquire) {
            if !forced && tokio::time::Instant::now() >= hard_kill_at {
                control.process.terminate();
                forced = true;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MobileLinuxError::Io(format!(
                    "PTY {} did not reap within 2 seconds",
                    handle.id
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    async fn rootfs_status(&self) -> Result<RootfsStatus, MobileLinuxError> {
        Ok(self.rootfs_snapshot())
    }

    async fn verify_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let manifest = self.load_rootfs_manifest()?;
        Ok(self.rootfs_store().status(&manifest))
    }

    async fn repair_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let manifest = self.load_rootfs_manifest()?;
        let store = self.rootfs_store();
        let status = store
            .recover_interrupted_activation(&manifest)
            .map_err(rootfs_store_error)?;
        if matches!(status.state, RootfsState::Ready) || status.staged_root.is_none() {
            return Ok(status);
        }
        store
            .activate_staged_rootfs(&manifest)
            .map_err(rootfs_store_error)
    }

    async fn reset_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let manifest = self.load_rootfs_manifest()?;
        self.shutdown().await?;
        let store = self.rootfs_store();
        store
            .reset_writable_state(&manifest)
            .map_err(rootfs_store_error)?;
        Ok(store.status(&manifest))
    }

    async fn configure_mounts(&self, mounts: Vec<MountSpec>) -> Result<(), MobileLinuxError> {
        for mount in &mounts {
            validate_mount(mount, &self.state.config.managed_root)?;
        }
        *self
            .state
            .mounts
            .write()
            .expect("mobile-linux mounts rwlock") = mounts;
        Ok(())
    }

    fn current_mounts(&self) -> Vec<MountSpec> {
        self.state
            .mounts
            .read()
            .expect("mobile-linux mounts rwlock")
            .clone()
    }

    async fn read_events(
        &self,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Vec<MobileLinuxEvent>, MobileLinuxError> {
        let after = after_sequence.unwrap_or(0);
        Ok(self
            .state
            .events
            .lock()
            .expect("mobile-linux events mutex")
            .iter()
            .filter(|event| event.sequence > after)
            .take(limit.min(mobile_linux_api::mobile_linux::MAX_MOBILE_LINUX_EVENT_BATCH))
            .cloned()
            .collect())
    }

    async fn list_tasks(&self) -> Result<Vec<MobileLinuxTaskSnapshot>, MobileLinuxError> {
        let mut tasks: Vec<_> = self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .values()
            .map(|task| task.snapshot.lock().expect("task snapshot mutex").clone())
            .collect();
        tasks.sort_by_key(|task| task.started_at_ms);
        Ok(tasks)
    }

    async fn task_status(
        &self,
        task_id: &str,
    ) -> Result<Option<MobileLinuxTaskSnapshot>, MobileLinuxError> {
        Ok(self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .get(task_id)
            .map(|task| task.snapshot.lock().expect("task snapshot mutex").clone()))
    }
}

fn validate_request(
    request: &LinuxCommandRequest,
    isolated_local_app_build_mounts: Option<&[MountSpec]>,
    profile: Option<&IsolatedBuildProfile>,
) -> Result<(), MobileLinuxError> {
    if request.command.trim().is_empty() || request.command.as_bytes().contains(&0) {
        return Err(MobileLinuxError::InvalidRequest(
            "command must not be empty or contain NUL".to_string(),
        ));
    }
    if matches!(request.timeout_ms, Some(0)) {
        return Err(MobileLinuxError::InvalidRequest(
            "timeout must be greater than zero".to_string(),
        ));
    }
    let _ = requested_memory_limit_bytes(request)?;
    validate_guest_path(request.cwd.as_deref().unwrap_or("/root"))?;
    for value in &request.args {
        if value.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(
                "argument contains NUL".to_string(),
            ));
        }
    }
    validate_env_map(&request.env, isolated_local_app_build_mounts, profile)
}

fn validate_pty_request(request: &PtyOpenRequest) -> Result<(), MobileLinuxError> {
    if request.command.trim().is_empty()
        || request.command.as_bytes().contains(&0)
        || request.size.cols == 0
        || request.size.rows == 0
    {
        return Err(MobileLinuxError::InvalidRequest(
            "PTY command and dimensions must be valid".to_string(),
        ));
    }
    for value in &request.args {
        if value.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(
                "PTY argument contains NUL".to_string(),
            ));
        }
    }
    validate_guest_path(request.cwd.as_deref().unwrap_or("/root"))?;
    validate_env_map(&request.env, None, None)
}

fn validate_mount(mount: &MountSpec, managed_root: &Path) -> Result<(), MobileLinuxError> {
    if !mount.host_path.is_absolute() {
        return Err(MobileLinuxError::InvalidRequest(
            "mount host path must be absolute".to_string(),
        ));
    }
    if mount.read_only {
        return Err(MobileLinuxError::InvalidRequest(
            "Android PRoot does not enforce read-only bind mounts".to_string(),
        ));
    }
    validate_guest_path(&mount.guest_path)?;
    let host = mount.host_path.canonicalize().map_err(|error| {
        MobileLinuxError::InvalidRequest(format!(
            "mount host path is unavailable ({}): {error}",
            mount.host_path.display()
        ))
    })?;
    let managed = managed_root
        .canonicalize()
        .unwrap_or_else(|_| managed_root.to_path_buf());
    if host.starts_with(&managed) || managed.starts_with(&host) {
        return Err(MobileLinuxError::InvalidRequest(
            "mount must not expose the managed rootfs".to_string(),
        ));
    }
    Ok(())
}

fn validate_isolated_local_app_mounts(
    mounts: &[MountSpec],
    managed_root: &Path,
    app_sandbox_root: &Path,
    profile: &IsolatedBuildProfile,
) -> Result<(), MobileLinuxError> {
    profile.validate()?;
    let build_mounts: Vec<_> = mounts
        .iter()
        .filter(|mount| matches!(mount.purpose, MountPurpose::LocalAppBuild))
        .collect();
    let store_mounts: Vec<_> = mounts
        .iter()
        .filter(|mount| {
            matches!(mount.purpose, MountPurpose::Shared)
                && mount.guest_path == profile.dependency_store
        })
        .collect();
    if build_mounts.len() != 1 || mounts.len() != 1 + store_mounts.len() || store_mounts.len() > 1 {
        return Err(MobileLinuxError::InvalidRequest(
            "isolated local-app execution requires exactly one LocalAppBuild mount and at most one validated dependency store mount".to_string(),
        ));
    }
    for mount in &store_mounts {
        let host = mount.host_path.canonicalize().map_err(|error| {
            MobileLinuxError::InvalidRequest(format!(
                "dependency store mount is unavailable ({}): {error}",
                mount.host_path.display()
            ))
        })?;
        if !host.starts_with(app_sandbox_root) {
            return Err(MobileLinuxError::InvalidRequest(
                "dependency store mount must remain inside the app sandbox".to_string(),
            ));
        }
    }
    let mount = build_mounts[0];
    let host_path = mount.host_path.canonicalize().map_err(|error| {
        MobileLinuxError::InvalidRequest(format!(
            "mount host path is unavailable ({}): {error}",
            mount.host_path.display()
        ))
    })?;
    let managed_root = managed_root
        .canonicalize()
        .unwrap_or_else(|_| managed_root.to_path_buf());
    if host_path.starts_with(&managed_root) || managed_root.starts_with(&host_path) {
        return Err(MobileLinuxError::InvalidRequest(
            "mount must not expose the managed rootfs".to_string(),
        ));
    }
    let (app_id, channel) = parse_local_app_build_guest_path(&mount.guest_path, profile)?;
    let sandbox_root = app_sandbox_root
        .canonicalize()
        .unwrap_or_else(|_| app_sandbox_root.to_path_buf());
    let expected = sandbox_root
        .join(&profile.host_apps_directory)
        .join(app_id)
        .join(&profile.host_build_directory)
        .join(channel);
    let workspace = sandbox_root
        .join(&profile.host_apps_directory)
        .join(app_id)
        .join(&profile.host_workspace_directory);
    if host_path != workspace
        && host_path != expected
        && !local_app_build_host_path_matches(&host_path, &expected, channel)
    {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "local-app build mount host_path must match {} or workspace {} or its .{channel}.staging-<numeric nonce> sibling (got {})",
            expected.display(), workspace.display(),
            host_path.display()
        )));
    }
    Ok(())
}

fn local_app_build_host_path_matches(
    host_path: &Path,
    expected_host_path: &Path,
    channel: &str,
) -> bool {
    if host_path == expected_host_path {
        return true;
    }
    if host_path.parent() != expected_host_path.parent() {
        return false;
    }
    let staging_prefix = format!(".{channel}.staging-");
    let Some(nonce) = host_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(staging_prefix.as_str()))
    else {
        return false;
    };
    !nonce.is_empty() && nonce.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_local_app_build_guest_path<'a>(
    path: &'a str,
    profile: &IsolatedBuildProfile,
) -> Result<(&'a str, &'a str), MobileLinuxError> {
    profile.validate()?;
    let relative = path
        .strip_prefix(profile.guest_root.as_str())
        .and_then(|suffix| suffix.strip_prefix('/'))
        .ok_or_else(|| {
            MobileLinuxError::InvalidRequest(format!(
                "local-app build guest_path must be {}/<app-id>/<channel>/project",
                profile.guest_root.as_str()
            ))
        })?;
    let mut segments = relative.split('/');
    let app_id = segments.next().unwrap_or_default();
    let channel = segments.next().unwrap_or_default();
    let project = segments.next().unwrap_or_default();
    if segments.next().is_some()
        || !is_valid_local_app_id(app_id)
        || !profile.channels.iter().any(|allowed| allowed == channel)
        || project != profile.project_directory
    {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "local-app build guest_path must be {}/<app-id>/<store|full>/project",
            profile.guest_root.as_str()
        )));
    }
    Ok((app_id, channel))
}

fn is_valid_local_app_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn validate_env_map(
    env: &BTreeMap<String, String>,
    isolated_local_app_build_mounts: Option<&[MountSpec]>,
    profile: Option<&IsolatedBuildProfile>,
) -> Result<(), MobileLinuxError> {
    let fixed_local_app_build_env = isolated_local_app_build_mounts
        .map(|mounts| {
            expected_local_app_build_env(
                mounts,
                profile.ok_or_else(|| {
                    MobileLinuxError::InvalidRequest(
                        "isolated build profile is not configured".into(),
                    )
                })?,
            )
        })
        .transpose()?;
    if let Some(expected_env) = fixed_local_app_build_env.as_ref() {
        for (key, expected_value) in expected_env {
            match env.get(key) {
                Some(value) if value == expected_value => {}
                Some(_) => {
                    return Err(MobileLinuxError::InvalidRequest(format!(
                        "isolated local-app build environment variable {key} must equal {expected_value}"
                    )))
                }
                None => {
                    return Err(MobileLinuxError::InvalidRequest(format!(
                        "isolated local-app build requires environment variable {key}"
                    )))
                }
            }
        }
    }
    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "invalid environment variable name: {key}"
            )));
        }
        if value.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "environment variable value contains NUL: {key}"
            )));
        }
        if let Some(expected_value) = fixed_local_app_build_env
            .as_ref()
            .and_then(|expected| expected.get(key))
        {
            debug_assert_eq!(value, expected_value);
            continue;
        }
        if is_host_reserved_env_var(key) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "host-reserved environment variable: {key}"
            )));
        }
    }
    Ok(())
}

fn expected_local_app_build_env(
    mounts: &[MountSpec],
    profile: &IsolatedBuildProfile,
) -> Result<BTreeMap<String, String>, MobileLinuxError> {
    let mount = mounts
        .iter()
        .find(|mount| matches!(mount.purpose, MountPurpose::LocalAppBuild))
        .ok_or_else(|| {
            MobileLinuxError::InvalidRequest("missing LocalAppBuild mount".to_string())
        })?;
    parse_local_app_build_guest_path(&mount.guest_path, profile)?;
    let build_state_root = format!("{}/{}", mount.guest_path, profile.state_directory);
    Ok(BTreeMap::from([
        ("HOME".into(), format!("{build_state_root}/home")),
        ("TMPDIR".into(), format!("{build_state_root}/tmp")),
        ("TMP".into(), format!("{build_state_root}/tmp")),
        ("TEMP".into(), format!("{build_state_root}/tmp")),
        (
            "XDG_CACHE_HOME".into(),
            format!("{build_state_root}/xdg-cache"),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            format!("{build_state_root}/xdg-config"),
        ),
        (
            "XDG_DATA_HOME".into(),
            format!("{build_state_root}/xdg-data"),
        ),
    ]))
}

fn is_host_reserved_env_var(key: &str) -> bool {
    matches!(
        key,
        "HOME"
            | "PATH"
            | "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "PROOT_LOADER"
            | "PROOT_LOADER_32"
            | "PROOT_TMP_DIR"
    )
}

fn validate_guest_path(path: &str) -> Result<(), MobileLinuxError> {
    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(MobileLinuxError::InvalidRequest(
            "guest path must be normalized and absolute".to_string(),
        ));
    }
    Ok(())
}

fn executable_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.permissions().mode() & 0o111 != 0
    })
}

fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_symlink())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn display_command(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn terminate_group(pid: u32, signal: Signal) {
    if let Ok(raw) = i32::try_from(pid) {
        let pid = Pid::from_raw(raw);
        if killpg(pid, signal).is_err() {
            let _ = kill(pid, signal);
        }
    }
}

enum ChildWaitOutcome {
    Exited(std::process::ExitStatus),
    TimedOut,
    MemoryLimitExceeded(MemoryLimitExceededDiagnostic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryLimitExceededDiagnostic {
    observed_rss: u64,
    peak_rss: u64,
    limit: u64,
}

impl MemoryLimitExceededDiagnostic {
    fn summary(self) -> String {
        format!(
            "Android PRoot process group exceeded resident-memory limit: observed_rss_bytes={} peak_rss_bytes={} limit_bytes={}",
            self.observed_rss, self.peak_rss, self.limit
        )
    }

    fn detail(self) -> String {
        format!("resource_limit_exceeded: {}", self.summary())
    }
}

#[derive(Debug)]
struct MemoryWatchdog {
    limit: u64,
    peak_rss: u64,
}

impl MemoryWatchdog {
    fn new(limit_bytes: u64) -> Self {
        Self {
            limit: limit_bytes,
            peak_rss: 0,
        }
    }

    fn observe(&mut self, observed_rss_bytes: u64) -> Option<MemoryLimitExceededDiagnostic> {
        self.peak_rss = self.peak_rss.max(observed_rss_bytes);
        (observed_rss_bytes > self.limit).then_some(MemoryLimitExceededDiagnostic {
            observed_rss: observed_rss_bytes,
            peak_rss: self.peak_rss,
            limit: self.limit,
        })
    }
}

async fn wait_for_network_policy_receipt(
    child: &mut Child,
    path: &Path,
    expected_policy: &str,
) -> Result<(), MobileLinuxError> {
    let expected = format!("{expected_policy}\n");
    let deadline = tokio::time::Instant::now() + ENFORCEMENT_RECEIPT_TIMEOUT;
    loop {
        match fs::read_to_string(path) {
            Ok(value) if value == expected => return Ok(()),
            Ok(value) if !value.is_empty() => {
                return Err(MobileLinuxError::NetworkPolicyUnavailable(format!(
                    "Android policy launcher returned an invalid enforcement receipt: {value:?}"
                )));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(MobileLinuxError::NetworkPolicyUnavailable(format!(
                    "Android policy launcher receipt became unreadable: {error}"
                )));
            }
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| MobileLinuxError::Io(format!("poll policy launcher: {error}")))?
        {
            return Err(MobileLinuxError::NetworkPolicyUnavailable(format!(
                "Android policy launcher exited before enforcement (status {status})"
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(MobileLinuxError::NetworkPolicyUnavailable(
                "Android policy launcher did not prove enforcement before spawn timeout"
                    .to_string(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn enforced_network_policy_name(policy: NetworkPolicy) -> Option<&'static str> {
    match policy {
        NetworkPolicy::Disabled => Some("disabled"),
        NetworkPolicy::LoopbackOnly => Some("loopback_only"),
        NetworkPolicy::Allowed => None,
    }
}

async fn wait_raw_stop(stopped: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = stopped.wait_for(|stopped| *stopped).await;
}

fn raw_stdin_closed() -> MobileLinuxError {
    MobileLinuxError::InvalidRequest("raw stdio stdin is closed".to_string())
}

async fn write_raw_stream(
    mut stdin: ChildStdin,
    mut writes: tokio::sync::mpsc::Receiver<RawStdioWrite>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let write = tokio::select! {
            biased;
            _ = wait_raw_stop(&mut stopped) => break,
            write = writes.recv() => match write { Some(write) => write, None => break },
        };
        let result = tokio::select! {
            biased;
            _ = wait_raw_stop(&mut stopped) => Err(raw_stdin_closed()),
            result = stdin.write_all(&write.input) => result.map_err(|error| {
                MobileLinuxError::Io(format!("write raw stdio stdin: {error}"))
            }),
        };
        let failed = result.is_err();
        let _ = write.reply.send(result);
        if failed {
            break;
        }
    }
}

async fn terminate_and_reap(
    child: &mut Child,
    pid: u32,
) -> Result<std::process::ExitStatus, MobileLinuxError> {
    terminate_group(pid, Signal::SIGTERM);
    if let Ok(result) = tokio::time::timeout(REAP_BUDGET / 2, child.wait()).await {
        // Reaping the launcher does not prove its descendants exited. A child
        // can ignore TERM and retain the output pipes after the group leader
        // has gone; kill the remaining group before waiting for either drain.
        terminate_group(pid, Signal::SIGKILL);
        return result
            .map_err(|error| MobileLinuxError::Io(format!("reap raw stdio child: {error}")));
    }
    terminate_group(pid, Signal::SIGKILL);
    tokio::time::timeout(REAP_BUDGET / 2, child.wait())
        .await
        .map_err(|_| MobileLinuxError::Io("raw stdio process tree survived SIGKILL".to_string()))?
        .map_err(|error| MobileLinuxError::Io(format!("reap raw stdio child: {error}")))
}

async fn join_raw_reader(
    mut task: tokio::task::JoinHandle<Result<(), MobileLinuxError>>,
    pid: u32,
    stream: &str,
) -> Result<(), MobileLinuxError> {
    match tokio::time::timeout(REAP_BUDGET, &mut task).await {
        Ok(result) => result.map_err(|error| {
            MobileLinuxError::Io(format!("raw {stream} reader failed: {error}"))
        })?,
        Err(_) => {
            // A descendant can retain a pipe after the direct child exits.
            terminate_group(pid, Signal::SIGKILL);
            match tokio::time::timeout(REAP_BUDGET, &mut task).await {
                Ok(result) => result.map_err(|error| {
                    MobileLinuxError::Io(format!("raw {stream} reader failed: {error}"))
                })?,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Err(MobileLinuxError::Io(format!(
                        "raw {stream} did not reach EOF after process termination"
                    )))
                }
            }
        }
    }
}

async fn wait_for_child(
    child: &mut Child,
    pid: u32,
    timeout: Option<Duration>,
    memory_limit_bytes: Option<u64>,
) -> Result<ChildWaitOutcome, MobileLinuxError> {
    let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
    let mut memory_watchdog = memory_limit_bytes.map(MemoryWatchdog::new);
    let mut memory_poll = tokio::time::interval(MEMORY_POLL_INTERVAL);
    memory_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = child.wait() => {
                return result
                    .map(ChildWaitOutcome::Exited)
                    .map_err(|error| MobileLinuxError::Io(format!("wait for PRoot: {error}")));
            }
            _ = memory_poll.tick(), if memory_watchdog.is_some() => {
                let resident = match process_group_rss_bytes(pid) {
                    Ok(resident) => resident,
                    Err(error) => {
                        terminate_group(pid, Signal::SIGKILL);
                        let _ = child.wait().await;
                        return Err(error);
                    }
                };
                let watchdog = memory_watchdog.as_mut().expect("guarded memory watchdog");
                if let Some(diagnostic) = watchdog.observe(resident) {
                    terminate_group(pid, Signal::SIGKILL);
                    let _ = child.wait().await;
                    return Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic));
                }
            }
            () = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                terminate_group(pid, Signal::SIGKILL);
                let _ = child.wait().await;
                return Ok(ChildWaitOutcome::TimedOut);
            }
        }
    }
}

fn ensure_process_group_rss_available() -> Result<(), MobileLinuxError> {
    if Path::new("/proc/self/stat").is_file() && Path::new("/proc/self/status").is_file() {
        Ok(())
    } else {
        Err(MobileLinuxError::ResourceLimitExceeded(
            "Android process-group RSS accounting is unavailable".to_string(),
        ))
    }
}

fn process_group_rss_bytes(process_group: u32) -> Result<u64, MobileLinuxError> {
    let entries = fs::read_dir("/proc").map_err(|error| {
        MobileLinuxError::ResourceLimitExceeded(format!(
            "read Android process table for memory watchdog: {error}"
        ))
    })?;
    let mut resident_bytes = 0_u64;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let process_dir = entry.path();
        let Ok(stat) = fs::read_to_string(process_dir.join("stat")) else {
            continue;
        };
        let Some(after_name) = stat.rsplit_once(')').map(|(_, tail)| tail.trim()) else {
            continue;
        };
        let mut fields = after_name.split_whitespace();
        let _state = fields.next();
        let _parent_pid = fields.next();
        let Some(group) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if group != process_group {
            continue;
        }
        let Ok(status) = fs::read_to_string(process_dir.join("status")) else {
            continue;
        };
        let resident_kib = status.lines().find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        });
        if let Some(resident_kib) = resident_kib {
            resident_bytes = resident_bytes.saturating_add(resident_kib.saturating_mul(1024));
        } else if pid == process_group {
            return Err(MobileLinuxError::ResourceLimitExceeded(
                "Android memory watchdog could not read root process RSS".to_string(),
            ));
        }
    }
    Ok(resident_bytes)
}

fn requested_memory_limit_bytes(
    request: &LinuxCommandRequest,
) -> Result<Option<u64>, MobileLinuxError> {
    let limits = request.resource_limits;
    if limits.max_cpu_seconds.is_some()
        || limits.max_processes.is_some()
        || limits.max_open_files.is_some()
    {
        return Err(MobileLinuxError::ResourceLimitExceeded(
            "Android PRoot currently enforces only max_memory_mb for local-app commands"
                .to_string(),
        ));
    }
    match limits.max_memory_mb {
        Some(0) => Err(MobileLinuxError::InvalidRequest(
            "max_memory_mb must be greater than zero".to_string(),
        )),
        Some(megabytes) => Ok(Some(u64::from(megabytes).saturating_mul(1024 * 1024))),
        None => Ok(None),
    }
}

async fn read_stdout<R, F, Fut>(reader: R, mut on_line: F) -> Result<Vec<u8>, MobileLinuxError>
where
    R: AsyncRead + Unpin,
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), MobileLinuxError>>,
{
    let mut captured = Vec::new();
    let mut pending = Vec::new();
    let mut reader = BufReader::new(reader);
    let mut buffer = vec![0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("read stdout: {error}")))?;
        if read == 0 {
            break;
        }
        append_capped(&mut captured, &buffer[..read]);
        pending.extend_from_slice(&buffer[..read]);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let mut line = pending.drain(..=newline).collect::<Vec<_>>();
            trim_line_endings(&mut line);
            on_line(String::from_utf8_lossy(&line).into_owned()).await?;
        }
        if pending.len() >= MAX_STDOUT_FRAGMENT_BYTES {
            let fragment = std::mem::take(&mut pending);
            on_line(String::from_utf8_lossy(&fragment).into_owned()).await?;
        }
    }
    trim_line_endings(&mut pending);
    if !pending.is_empty() {
        on_line(String::from_utf8_lossy(&pending).into_owned()).await?;
    }
    Ok(captured)
}

async fn read_stderr<R, F, Fut>(mut reader: R, mut on_chunk: F) -> Result<Vec<u8>, MobileLinuxError>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(), MobileLinuxError>>,
{
    let mut captured = Vec::new();
    let mut buffer = vec![0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("read stderr: {error}")))?;
        if read == 0 {
            break;
        }
        let chunk = buffer[..read].to_vec();
        append_capped(&mut captured, &chunk);
        on_chunk(chunk).await?;
    }
    Ok(captured)
}

async fn read_raw_stream<R, F, Fut>(mut reader: R, mut on_chunk: F) -> Result<(), MobileLinuxError>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(), MobileLinuxError>>,
{
    let mut buffer = vec![0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("read raw stdio: {error}")))?;
        if read == 0 {
            return Ok(());
        }
        on_chunk(buffer[..read].to_vec()).await?;
    }
}

async fn join_reader(
    task: tokio::task::JoinHandle<Result<Vec<u8>, MobileLinuxError>>,
    stream: &str,
) -> Result<Vec<u8>, MobileLinuxError> {
    task.await
        .map_err(|error| MobileLinuxError::Io(format!("{stream} reader failed: {error}")))?
}

fn directory_size(path: &Path) -> std::io::Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut size = 0_u64;
    for entry in fs::read_dir(path)? {
        size = size.saturating_add(directory_size(&entry?.path())?);
    }
    Ok(size)
}

fn append_capped(captured: &mut Vec<u8>, chunk: &[u8]) {
    let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
    if remaining == 0 {
        return;
    }
    captured.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}

fn trim_line_endings(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
}

fn rootfs_store_error(error: RootfsStoreError) -> MobileLinuxError {
    match error {
        RootfsStoreError::Manifest(error) => MobileLinuxError::Integrity(error.to_string()),
        RootfsStoreError::Io { path, message } => {
            MobileLinuxError::Io(format!("{}: {message}", path.display()))
        }
        RootfsStoreError::ArchiveSizeMismatch { expected, actual } => MobileLinuxError::Integrity(
            format!("archive size mismatch: expected {expected}, got {actual}"),
        ),
        RootfsStoreError::ArchiveHashMismatch { expected, actual } => MobileLinuxError::Integrity(
            format!("archive hash mismatch: expected {expected}, got {actual}"),
        ),
        RootfsStoreError::TargetMismatch { store, manifest } => MobileLinuxError::Integrity(
            format!("rootfs manifest target {manifest} does not match store target {store}"),
        ),
        RootfsStoreError::UnsafeResetPath(path) | RootfsStoreError::UnsafeManagedPath(path) => {
            MobileLinuxError::Integrity(format!("unsafe rootfs path: {}", path.display()))
        }
        RootfsStoreError::Integrity(message) => MobileLinuxError::Integrity(message),
        RootfsStoreError::ExecutionDenied { path, reason } => {
            MobileLinuxError::Integrity(format!("{path}: {reason}"))
        }
    }
}

#[cfg(test)]
mod tests {
    fn test_build_profile() -> super::IsolatedBuildProfile {
        super::IsolatedBuildProfile {
            guest_root: "/var/lingxi/local-app-build".into(),
            project_directory: "project".into(),
            dependency_store: "/var/lingxi/local-app-dependency-store".into(),
            state_directory: ".lingxi-build-state".into(),
            host_apps_directory: "apps".into(),
            host_build_directory: "build".into(),
            host_workspace_directory: "workspace".into(),
            channels: vec!["store".into(), "full".into()],
        }
    }

    use super::*;
    use mobile_linux_api::MountPurpose;
    use tempfile::TempDir;

    #[test]
    fn independent_profile_uses_only_caller_layout() {
        let (temp, mut runtime) = runtime();
        let profile = IsolatedBuildProfile {
            guest_root: "/opt/example/builds".into(),
            project_directory: "source".into(),
            dependency_store: "/opt/example/dependencies".into(),
            state_directory: ".state".into(),
            host_apps_directory: "units".into(),
            host_build_directory: "outputs".into(),
            host_workspace_directory: "source".into(),
            channels: vec!["debug".into()],
        };
        Arc::get_mut(&mut runtime.state)
            .unwrap()
            .config
            .isolated_build_profile = Some(profile.clone());
        let host = temp.path().join("sandbox/units/example/source");
        fs::create_dir_all(&host).unwrap();
        let mounts = vec![MountSpec {
            host_path: host,
            guest_path: "/opt/example/builds/example/debug/source".into(),
            read_only: false,
            purpose: MountPurpose::LocalAppBuild,
        }];
        runtime
            .execution_mounts(&mounts, ForegroundMountMode::RequestOnly)
            .unwrap();
        let env = expected_local_app_build_env(&mounts, &profile).unwrap();
        assert_eq!(
            env["HOME"],
            "/opt/example/builds/example/debug/source/.state/home"
        );
        assert!(parse_local_app_build_guest_path(
            "/var/lingxi/local-app-build/example/store/project",
            &profile
        )
        .is_err());
    }

    #[test]
    fn isolated_builds_require_explicit_profile() {
        let (_temp, mut runtime) = runtime();
        Arc::get_mut(&mut runtime.state)
            .unwrap()
            .config
            .isolated_build_profile = None;
        let error = runtime
            .execution_mounts(&[], ForegroundMountMode::RequestOnly)
            .unwrap_err();
        assert!(error.to_string().contains("profile is not configured"));
    }

    #[test]
    fn build_profile_rejects_path_escape_components() {
        let mut profile = test_build_profile();
        profile.host_apps_directory = "../outside".into();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn native_library_directory_is_explicit_and_not_host_library_named() {
        let (temp, mut runtime) = runtime();
        let native = temp.path().join("generic-native-libraries");
        fs::create_dir_all(&native).unwrap();
        let executable = native.join("libproot.so");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        Arc::get_mut(&mut runtime.state)
            .unwrap()
            .config
            .native_library_dir = Some(native);
        assert_eq!(runtime.proot_binary().unwrap(), executable);
    }

    fn runtime() -> (TempDir, AndroidProotRuntime) {
        let temp = tempfile::tempdir().expect("temp");
        let managed = temp.path().join("runtime");
        fs::create_dir_all(managed.join("active/bin")).expect("active");
        fs::create_dir_all(managed.join("bin")).expect("bin");
        fs::write(managed.join("active/bin/sh"), b"#!/bin/sh\n").expect("shell");
        fs::write(
            managed.join("bin/libproot.so"),
            b"#!/bin/sh\n\
              while [ \"$#\" -gt 0 ]; do\n\
                case \"$1\" in\n\
                  -0|--link2symlink) shift ;;\n\
                  -r|-b|-w) shift 2 ;;\n\
                  *) break ;;\n\
                esac\n\
              done\n\
              exec \"$@\"\n",
        )
        .expect("proot");
        for path in [
            managed.join("active/bin/sh"),
            managed.join("bin/libproot.so"),
        ] {
            let mut permissions = fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("permissions");
        }
        let runtime = AndroidProotRuntime::new(AndroidProotRuntimeConfig {
            native_library_dir: None,
            isolated_build_profile: Some(test_build_profile()),
            managed_root: managed,
            app_sandbox_root: temp.path().join("sandbox"),
            abi: "x86_64".to_string(),
            rootfs_version: "test".to_string(),
            archive_sha256: None,
        });
        (temp, runtime)
    }

    fn request() -> LinuxCommandRequest {
        LinuxCommandRequest {
            command: "/bin/true".to_string(),
            args: vec![],
            cwd: Some("/root".to_string()),
            env: BTreeMap::new(),
            stdin: None,
            // Generous on purpose. These commands are `printf`/`true`, so the
            // only thing this bound can catch is the machine being busy -- and
            // at 1000ms it did: under a full `--workspace` run the spawn alone
            // could exceed it, the runtime SIGKILLed the group as instructed,
            // and the test failed with an empty stdout that looked like a drain
            // race rather than a timeout. The one test that actually exercises
            // the timeout sets its own `timeout_ms` (50ms), so raising this
            // weakens no assertion.
            timeout_ms: Some(30_000),
            network: NetworkPolicy::Allowed,
            resource_limits: Default::default(),
            mounts: vec![],
        }
    }

    fn raw_request(script: &str) -> RawStdioOpenRequest {
        RawStdioOpenRequest {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: BTreeMap::new(),
            network: NetworkPolicy::Allowed,
            resource_limits: Default::default(),
            mounts: vec![],
        }
    }

    async fn io_test_timeout<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(8), future)
            .await
            .expect("I/O lifecycle must complete within eight seconds")
    }

    #[tokio::test]
    async fn run_drains_output_while_writing_large_stdin() {
        let (_temp, runtime) = runtime();
        let input = "abc0123456789".repeat(15_000);
        let mut request = request();
        request.command = "/bin/cat".into();
        request.stdin = Some(input.clone());
        let result = io_test_timeout(runtime.run(request)).await.expect("cat");
        assert_eq!(result.stdout, input);
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn run_timeout_covers_a_child_that_never_reads_stdin() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec!["-c".into(), "sleep 30".into()];
        request.stdin = Some("x".repeat(2 * 1024 * 1024));
        request.timeout_ms = Some(50);
        let result = io_test_timeout(runtime.run(request))
            .await
            .expect("timeout result");
        assert!(result.timed_out);
        assert_eq!(
            runtime.list_tasks().await.unwrap()[0].status,
            MobileLinuxTaskStatus::TimedOut
        );
    }

    #[tokio::test]
    async fn background_spawn_does_not_block_on_large_stdin() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.command = "/bin/cat".into();
        request.stdin = Some("x".repeat(2 * 1024 * 1024));
        let handle = io_test_timeout(runtime.spawn_background(request))
            .await
            .expect("background cat");
        io_test_timeout(async {
            loop {
                let tasks = runtime.list_tasks().await.unwrap();
                let task = tasks.iter().find(|task| task.task_id == handle.id).unwrap();
                if task.finished_at_ms.is_some() {
                    assert_eq!(task.status, MobileLinuxTaskStatus::Completed);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn foreground_caller_cancellation_reaps_blocked_stdin_and_finishes_task() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec![
            "-c".into(),
            "sh -c 'trap \"\" TERM; printf \"ready\\n\"; sleep 30' & wait".into(),
        ];
        request.stdin = Some("x".repeat(2 * 1024 * 1024));
        let execution_runtime = runtime.clone();
        let caller = tokio::spawn(async move { execution_runtime.run(request).await });
        let (task_id, pid) = io_test_timeout(async {
            loop {
                let tasks = runtime.state.tasks.lock().unwrap();
                let task = tasks.iter().next().map(|(id, task)| {
                    (id.clone(), task.pid.load(Ordering::Acquire) as u32)
                });
                drop(tasks);
                if let Some((id, pid)) = task {
                    let ready = runtime.state.events.lock().unwrap().iter().any(|event| {
                        matches!(&event.kind, MobileLinuxEventKind::StdoutLine { line } if line == "ready")
                    });
                    if pid != 0 && ready {
                        break (id, pid);
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await;
        assert!(!caller.is_finished(), "fixture must still be blocked");
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        io_test_timeout(async {
            loop {
                let tasks = runtime.list_tasks().await.unwrap();
                let task = tasks.iter().find(|task| task.task_id == task_id).unwrap();
                if task.finished_at_ms.is_some() {
                    assert_eq!(task.status, MobileLinuxTaskStatus::Cancelled);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            kill(Pid::from_raw(pid as i32), None).is_err(),
            "cancelled child must be reaped"
        );
        io_test_timeout(runtime.shutdown())
            .await
            .expect("shutdown after cancelled caller");
    }

    fn install_receipt_startup_barrier(runtime: &AndroidProotRuntime) -> Arc<tokio::sync::Barrier> {
        let launcher = runtime
            .state
            .config
            .managed_root
            .join("bin/libmobile_linux_policy_launcher.so");
        fs::write(&launcher, b"#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$LINGXI_ENFORCEMENT_RECEIPT_PATH\"\nshift\nexec \"$@\"\n").unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        *runtime.state.receipt_start_barrier.lock().unwrap() = Some(barrier.clone());
        barrier
    }

    async fn wait_for_pending_receipt(runtime: &AndroidProotRuntime) -> Arc<TaskControl> {
        io_test_timeout(async {
            loop {
                let task = runtime.state.tasks.lock().unwrap().values().next().cloned();
                let receipt_ready = fs::read_dir(runtime.state.config.managed_root.join("tmp"))
                    .is_ok_and(|entries| {
                        entries.flatten().any(|entry| {
                            fs::read(entry.path()).is_ok_and(|bytes| bytes == b"disabled\n")
                        })
                    });
                if let Some(task) = task.filter(|_| receipt_ready) {
                    assert_eq!(
                        task.pid.load(Ordering::Acquire),
                        0,
                        "owner must be paused before receipt acceptance"
                    );
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
    }

    async fn shutdown_through_receipt_barrier(
        runtime: &AndroidProotRuntime,
        task: &TaskControl,
        barrier: &tokio::sync::Barrier,
    ) {
        let stopping = runtime.clone();
        let shutdown = tokio::spawn(async move { stopping.shutdown().await });
        io_test_timeout(async {
            while !task.cancel_requested.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        barrier.wait().await;
        io_test_timeout(shutdown)
            .await
            .unwrap()
            .expect("shutdown must own pending startup");
    }

    async fn cancel_pending_foreground_startup(shutdown: bool) {
        let (temp, runtime) = runtime();
        let barrier = install_receipt_startup_barrier(&runtime);
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec!["-c".into(), "sleep 30".into()];
        request.network = NetworkPolicy::Disabled;
        request.stdin = Some("x".repeat(2 * 1024 * 1024));
        let execution_runtime = runtime.clone();
        let caller = tokio::spawn(async move { execution_runtime.run(request).await });
        let task = wait_for_pending_receipt(&runtime).await;
        if shutdown {
            shutdown_through_receipt_barrier(&runtime, &task, &barrier).await;
            assert!(io_test_timeout(caller).await.unwrap().unwrap().cancelled);
        } else {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            barrier.wait().await;
        }
        io_test_timeout(async {
            while !task.terminal_emitted.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert_eq!(
            task.snapshot.lock().unwrap().status,
            MobileLinuxTaskStatus::Cancelled
        );
        let pid = task.pid.load(Ordering::Acquire) as i32;
        assert!(
            pid > 0,
            "owner must observe and reap the launcher it created"
        );
        assert!(kill(Pid::from_raw(pid), None).is_err());
        assert_eq!(
            fs::read_dir(temp.path().join("runtime/tmp"))
                .unwrap()
                .count(),
            0,
            "receipt cleanup"
        );
        io_test_timeout(runtime.shutdown()).await.unwrap();
    }

    #[tokio::test]
    async fn caller_cancellation_during_receipt_startup_still_reaps() {
        cancel_pending_foreground_startup(false).await;
    }

    #[tokio::test]
    async fn shutdown_during_receipt_startup_waits_for_owned_child() {
        cancel_pending_foreground_startup(true).await;
    }

    async fn wait_cancelled_startup(runtime: &AndroidProotRuntime, task: &TaskControl) {
        io_test_timeout(async {
            while !task.terminal_emitted.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert_eq!(
            task.snapshot.lock().unwrap().status,
            MobileLinuxTaskStatus::Cancelled
        );
        let pid = task.pid.load(Ordering::Acquire) as i32;
        if pid > 0 {
            assert!(
                kill(Pid::from_raw(pid), None).is_err(),
                "startup child must be reaped"
            );
        }
        io_test_timeout(async {
            while !runtime.state.raw_stdio.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let snapshots = runtime
            .state
            .config
            .app_sandbox_root
            .join("cache/raw-stdio-snapshots");
        assert!(!snapshots.exists() || fs::read_dir(snapshots).unwrap().next().is_none());
        io_test_timeout(runtime.shutdown())
            .await
            .expect("shutdown after cancelled startup");
    }

    async fn cancel_pending_handle_startup(raw: bool, shutdown: bool) {
        let (temp, runtime) = runtime();
        let barrier = install_receipt_startup_barrier(&runtime);
        let source = temp.path().join("workspace");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("sample"), b"private snapshot").unwrap();
        let execution_runtime = runtime.clone();
        let caller = tokio::spawn(async move {
            if raw {
                let mut request = raw_request("sleep 30");
                request.network = NetworkPolicy::Disabled;
                request.mounts.push(MountSpec {
                    host_path: source,
                    guest_path: "/workspace/source".into(),
                    read_only: true,
                    purpose: MountPurpose::External,
                });
                execution_runtime.open_raw_stdio(request).await.map(|_| ())
            } else {
                let mut request = request();
                request.command = "/bin/sh".into();
                request.args = vec!["-c".into(), "sleep 30".into()];
                request.network = NetworkPolicy::Disabled;
                request.stdin = Some("x".repeat(2 * 1024 * 1024));
                execution_runtime
                    .spawn_background(request)
                    .await
                    .map(|_| ())
            }
        });
        let task = wait_for_pending_receipt(&runtime).await;
        if raw {
            assert!(temp
                .path()
                .join("sandbox/cache/raw-stdio-snapshots")
                .join(&task.snapshot.lock().unwrap().task_id)
                .join("0/sample")
                .is_file());
        }
        if shutdown {
            shutdown_through_receipt_barrier(&runtime, &task, &barrier).await;
            assert!(io_test_timeout(caller).await.unwrap().is_err());
        } else {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            barrier.wait().await;
        }
        wait_cancelled_startup(&runtime, &task).await;
        assert_eq!(
            fs::read_dir(temp.path().join("runtime/tmp"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn background_caller_abort_during_receipt_reaps_owned_startup() {
        cancel_pending_handle_startup(false, false).await;
    }

    #[tokio::test]
    async fn background_shutdown_during_receipt_reaps_owned_startup() {
        cancel_pending_handle_startup(false, true).await;
    }

    #[tokio::test]
    async fn raw_caller_abort_during_receipt_reaps_and_removes_snapshot() {
        cancel_pending_handle_startup(true, false).await;
    }

    #[tokio::test]
    async fn raw_shutdown_during_receipt_reaps_and_removes_snapshot() {
        cancel_pending_handle_startup(true, true).await;
    }

    fn cancel_queued_raw_snapshot(shutdown: bool) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        executor.block_on(async {
            let (temp, runtime) = runtime();
            let source = temp.path().join("source");
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("file"), b"snapshot input").unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = wait.recv();
            });
            ready.await.unwrap();
            let execution_runtime = runtime.clone();
            let mut request = raw_request("sleep 30");
            request.mounts.push(MountSpec {
                host_path: source,
                guest_path: "/workspace/source".into(),
                read_only: true,
                purpose: MountPurpose::External,
            });
            let caller =
                tokio::spawn(async move { execution_runtime.open_raw_stdio(request).await });
            tokio::time::sleep(Duration::from_millis(30)).await;
            let pending = !caller.is_finished();
            let task = runtime.state.tasks.lock().unwrap().values().next().cloned();
            let (aborted, shutdown_result) = if shutdown {
                let result = tokio::time::timeout(Duration::from_secs(8), runtime.shutdown()).await;
                (false, Some(result))
            } else {
                caller.abort();
                (true, None)
            };
            release.send(()).unwrap();
            blocker.await.unwrap();
            if let Some(result) = shutdown_result {
                result
                    .unwrap()
                    .expect("queued copy must not block shutdown");
            }
            if aborted {
                assert!(caller.await.unwrap_err().is_cancelled());
            } else {
                assert!(caller.await.unwrap().is_err());
            }
            assert!(
                pending,
                "caller must be pending while snapshot copy awaits its worker"
            );
            let task = task.expect("startup registered before queued snapshot");
            wait_cancelled_startup(&runtime, &task).await;
            assert_eq!(
                task.pid.load(Ordering::Acquire),
                0,
                "cancelled snapshot must not start a guest"
            );
        });
    }

    #[test]
    fn raw_caller_abort_while_snapshot_is_queued_has_no_late_leak() {
        cancel_queued_raw_snapshot(false);
    }

    #[test]
    fn raw_shutdown_while_snapshot_is_queued_is_bounded() {
        cancel_queued_raw_snapshot(true);
    }

    async fn cancel_unaccepted_handle(raw: bool) {
        let (_temp, runtime) = runtime();
        let mut caller: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), MobileLinuxError>> + Send>,
        > = if raw {
            Box::pin(async {
                runtime
                    .open_raw_stdio(raw_request("sleep 30"))
                    .await
                    .map(|_| ())
            })
        } else {
            let mut request = request();
            request.command = "/bin/sh".into();
            request.args = vec!["-c".into(), "sleep 30".into()];
            Box::pin(async { runtime.spawn_background(request).await.map(|_| ()) })
        };
        std::future::poll_fn(|cx| {
            assert!(caller.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let task = io_test_timeout(async {
            loop {
                let task = runtime.state.tasks.lock().unwrap().values().next().cloned();
                if let Some(task) = task {
                    if task.pid.load(Ordering::Acquire) > 0 {
                        break task;
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        // The owner has created a handle and sent the result. This caller has
        // never polled the response, so dropping it must still dispose the child.
        drop(caller);
        wait_cancelled_startup(&runtime, &task).await;
    }

    #[tokio::test]
    async fn background_unaccepted_handle_is_cancelled_and_reaped() {
        cancel_unaccepted_handle(false).await;
    }

    #[tokio::test]
    async fn raw_unaccepted_handle_is_cancelled_and_reaped() {
        cancel_unaccepted_handle(true).await;
    }

    struct PendingStreamSink {
        entered: tokio::sync::Notify,
        block_stdout: bool,
    }

    #[async_trait]
    impl ProcessStreamSink for PendingStreamSink {
        async fn stdout_line(&self, _: String) -> Result<(), mobile_linux_api::ProcessError> {
            if self.block_stdout {
                self.entered.notify_one();
                std::future::pending().await
            } else {
                Ok(())
            }
        }
        async fn stderr_chunk(&self, _: Vec<u8>) -> Result<(), mobile_linux_api::ProcessError> {
            if !self.block_stdout {
                self.entered.notify_one();
                std::future::pending().await
            } else {
                Ok(())
            }
        }
    }

    async fn interrupt_pending_stream_sink(mode: &str) {
        let (temp, runtime) = runtime();
        let barrier = install_receipt_startup_barrier(&runtime);
        let ready = temp.path().join("stream-fixture-ready");
        let sink = Arc::new(PendingStreamSink {
            entered: tokio::sync::Notify::new(),
            block_stdout: mode != "shutdown",
        });
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec!["-c".into(), format!(
            "printf 'first\\nsecond\\n'; printf 'err\\000\\377' >&2; : > \"$SDK_TEST_STREAM_READY\"; {}",
            if mode == "shutdown" { "exit 0" } else { "sleep 30" }
        )];
        request.network = NetworkPolicy::Disabled;
        request.env.insert(
            "SDK_TEST_STREAM_READY".into(),
            ready.to_string_lossy().into_owned(),
        );
        if mode == "timeout" {
            request.timeout_ms = Some(2000);
        }
        let running = runtime.clone();
        let stream = sink.clone();
        let caller = tokio::spawn(async move { running.run_streaming(request, stream).await });
        // The owner pauses before its real execution deadline. Prepare both
        // pipe payloads first, so this is a callback-timeout test rather than a
        // race against process scheduling under a busy parallel test suite.
        io_test_timeout(async {
            while !ready.is_file() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        barrier.wait().await;
        io_test_timeout(sink.entered.notified()).await;
        // Both writes must have happened before cancellation, so tail retention
        // is tested independently of how quickly the shell gets scheduled.
        io_test_timeout(async {
            loop {
                if runtime
                    .state
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| matches!(event.kind, MobileLinuxEventKind::StderrChunk { .. }))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let task = runtime
            .state
            .tasks
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        let pid = task.pid.load(Ordering::Acquire) as i32;
        let result = match mode {
            "caller" => {
                caller.abort();
                assert!(caller.await.unwrap_err().is_cancelled());
                io_test_timeout(runtime.shutdown())
                    .await
                    .expect("cancelled sink cannot block shutdown");
                None
            }
            "shutdown" => {
                // Also cover cancellation after child.wait completed, while the
                // only thing keeping the owner alive is a pending foreign sink.
                io_test_timeout(async {
                    while kill(Pid::from_raw(pid), None).is_ok() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await;
                io_test_timeout(runtime.shutdown())
                    .await
                    .expect("shutdown interrupts pending sink");
                Some(io_test_timeout(caller).await.unwrap().unwrap())
            }
            "timeout" => Some(io_test_timeout(caller).await.unwrap().unwrap()),
            _ => unreachable!(),
        };
        if let Some(result) = result {
            assert_eq!(result.stdout, "first\nsecond\n");
            assert_eq!(result.stderr, String::from_utf8_lossy(b"err\x00\xff"));
            assert_eq!(result.cancelled, mode == "shutdown");
            assert_eq!(result.timed_out, mode == "timeout");
        }
        assert_eq!(
            task.snapshot.lock().unwrap().status,
            if mode == "timeout" {
                MobileLinuxTaskStatus::TimedOut
            } else {
                MobileLinuxTaskStatus::Cancelled
            }
        );
        assert!(
            kill(Pid::from_raw(pid), None).is_err(),
            "child must be reaped before terminal state"
        );
        let events = runtime.state.events.lock().unwrap();
        let lines: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                MobileLinuxEventKind::StdoutLine { line } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["first", "second"],
            "cancelled delivery must still drain stdout in order"
        );
        let stderr: Vec<_> = events
            .iter()
            .flat_map(|event| match &event.kind {
                MobileLinuxEventKind::StderrChunk { chunk } => chunk.as_slice(),
                _ => &[],
            })
            .copied()
            .collect();
        assert_eq!(stderr, b"err\x00\xff", "stderr events retain exact bytes");
    }

    #[tokio::test]
    async fn streaming_caller_abort_interrupts_pending_sink_and_drains() {
        interrupt_pending_stream_sink("caller").await;
    }

    #[tokio::test]
    async fn streaming_shutdown_after_child_exit_interrupts_pending_sink() {
        interrupt_pending_stream_sink("shutdown").await;
    }

    #[tokio::test]
    async fn streaming_timeout_interrupts_pending_sink_and_retains_capture() {
        interrupt_pending_stream_sink("timeout").await;
    }

    struct SlowStreamSink {
        lines: Mutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait]
    impl ProcessStreamSink for SlowStreamSink {
        async fn stdout_line(&self, line: String) -> Result<(), mobile_linux_api::ProcessError> {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if self.fail {
                return Err(mobile_linux_api::ProcessError::Io(
                    "typed sink failure".into(),
                ));
            }
            self.lines.lock().unwrap().push(line);
            Ok(())
        }
        async fn stderr_chunk(&self, _: Vec<u8>) -> Result<(), mobile_linux_api::ProcessError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn normal_child_exit_preserves_slow_sink_order_and_delivery() {
        let (_temp, runtime) = runtime();
        let sink = Arc::new(SlowStreamSink {
            lines: Mutex::new(Vec::new()),
            fail: false,
        });
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec!["-c".into(), "printf 'first\\nsecond\\n'".into()];
        let result = io_test_timeout(runtime.run_streaming(request, sink.clone()))
            .await
            .unwrap();
        assert_eq!(result.stdout, "first\nsecond\n");
        assert_eq!(*sink.lines.lock().unwrap(), ["first", "second"]);
        assert!(!result.timed_out && !result.cancelled);
    }

    #[tokio::test]
    async fn normal_stream_sink_failure_remains_a_typed_error() {
        let (_temp, runtime) = runtime();
        let sink = Arc::new(SlowStreamSink {
            lines: Mutex::new(Vec::new()),
            fail: true,
        });
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec!["-c".into(), "printf 'first\\n'".into()];
        let error = io_test_timeout(runtime.run_streaming(request, sink))
            .await
            .unwrap_err();
        assert!(
            matches!(error, MobileLinuxError::Io(detail) if detail.contains("typed sink failure"))
        );
        assert_eq!(
            runtime.list_tasks().await.unwrap()[0].status,
            MobileLinuxTaskStatus::Failed
        );
    }

    async fn close_blocked_raw_writer(shutdown: bool) {
        let (_temp, runtime) = runtime();
        let handle = runtime
            .open_raw_stdio(raw_request("trap '' TERM; printf ready; sleep 30"))
            .await
            .expect("open raw");
        let control = runtime.state.raw_stdio.lock().unwrap()[&handle.id].clone();
        let pid = control.task.pid.load(Ordering::Acquire) as u32;
        io_test_timeout(async {
            loop {
                if !runtime
                    .read_raw_stdio(&handle, 32)
                    .await
                    .unwrap()
                    .stdout
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let write_runtime = runtime.clone();
        let write_handle = handle.clone();
        let writer = tokio::spawn(async move {
            write_runtime
                .write_raw_stdio(&write_handle, vec![42; 2 * 1024 * 1024])
                .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !writer.is_finished(),
            "test must exercise pipe backpressure"
        );
        if shutdown {
            io_test_timeout(runtime.shutdown())
                .await
                .expect("shutdown blocked session");
        } else {
            io_test_timeout(runtime.close_raw_stdio(&handle))
                .await
                .expect("close blocked session");
        }
        assert!(io_test_timeout(writer).await.unwrap().is_err());
        assert!(control.worker.lock().unwrap().is_none());
        assert!(control.buffers.lock().unwrap().closed);
        assert!(
            kill(Pid::from_raw(pid as i32), None).is_err(),
            "child must be reaped"
        );
    }

    #[tokio::test]
    async fn raw_close_cancels_blocked_writer_and_reaps_child() {
        close_blocked_raw_writer(false).await;
    }

    #[tokio::test]
    async fn shutdown_cancels_blocked_raw_writer_and_reaps_child() {
        close_blocked_raw_writer(true).await;
    }

    #[tokio::test]
    async fn raw_exit_preserves_both_binary_tails_before_closed() {
        let (temp, runtime) = runtime();
        let input: Vec<u8> = (0..(96 * 1024)).map(|index| (index % 256) as u8).collect();
        let source = temp.path().join("binary-tail");
        fs::write(&source, &input).unwrap();
        let mut request = raw_request("cat \"$1\"; cat \"$1\" >&2");
        request
            .args
            .extend(["tail-test".into(), source.to_string_lossy().into_owned()]);
        let handle = runtime.open_raw_stdio(request).await.unwrap();
        let pid = runtime.state.raw_stdio.lock().unwrap()[&handle.id]
            .task
            .pid
            .load(Ordering::Acquire) as u32;
        let (stdout, stderr) = io_test_timeout(async {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            loop {
                let result = runtime.read_raw_stdio(&handle, 317).await.unwrap();
                stdout.extend(result.stdout);
                stderr.extend(result.stderr);
                if result.closed {
                    assert_eq!(result.exit_code, Some(0));
                    break (stdout, stderr);
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(stdout, input);
        assert_eq!(stderr, input);
        assert!(
            kill(Pid::from_raw(pid as i32), None).is_err(),
            "exit must reap without an explicit close"
        );
        io_test_timeout(runtime.close_raw_stdio(&handle))
            .await
            .expect("close reaped child");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn raw_memory_watchdog_enforces_the_advertised_limit() {
        let (_temp, runtime) = runtime();
        let mut request = raw_request("sleep 30");
        request.resource_limits.max_memory_mb = Some(1);
        let handle = runtime
            .open_raw_stdio(request)
            .await
            .expect("open with watchdog");
        assert!(handle.enforcement.memory_limit_enforced);
        io_test_timeout(async {
            loop {
                match runtime.read_raw_stdio(&handle, 4096).await {
                    Err(MobileLinuxError::ResourceLimitExceeded(message)) => {
                        assert!(message.contains("resident-memory limit"));
                        break;
                    }
                    Ok(result) => assert!(!result.closed, "limited process must fail"),
                    Err(error) => panic!("unexpected raw read error: {error}"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(matches!(
            runtime.close_raw_stdio(&handle).await,
            Err(MobileLinuxError::ResourceLimitExceeded(_))
        ));
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn raw_memory_limit_without_rss_support_never_returns_an_enforced_receipt() {
        let (_temp, runtime) = runtime();
        let mut request = raw_request("sleep 30");
        request.resource_limits.max_memory_mb = Some(1);
        assert!(matches!(
            runtime.open_raw_stdio(request).await,
            Err(MobileLinuxError::ResourceLimitExceeded(_))
        ));
    }

    fn local_app_build_host(temp: &TempDir, app_id: &str, channel: &str) -> PathBuf {
        let path = temp
            .path()
            .join("sandbox")
            .join("apps")
            .join(app_id)
            .join("build")
            .join(channel);
        fs::create_dir_all(&path).expect("local-app build host");
        path
    }

    fn fixed_local_app_build_env(
        app_id: &str,
        channel: &str,
    ) -> (String, BTreeMap<String, String>) {
        let project_guest_path = format!("/var/lingxi/local-app-build/{app_id}/{channel}/project");
        let build_state_root = format!("{project_guest_path}/.lingxi-build-state");
        let mut env = BTreeMap::new();
        env.insert("HOME".into(), format!("{build_state_root}/home"));
        env.insert("TMPDIR".into(), format!("{build_state_root}/tmp"));
        env.insert("TMP".into(), format!("{build_state_root}/tmp"));
        env.insert("TEMP".into(), format!("{build_state_root}/tmp"));
        env.insert(
            "XDG_CACHE_HOME".into(),
            format!("{build_state_root}/xdg-cache"),
        );
        env.insert(
            "XDG_CONFIG_HOME".into(),
            format!("{build_state_root}/xdg-config"),
        );
        env.insert(
            "XDG_DATA_HOME".into(),
            format!("{build_state_root}/xdg-data"),
        );
        (project_guest_path, env)
    }

    #[test]
    fn memory_watchdog_reports_trigger_sample_peak_and_limit() {
        assert_eq!(MEMORY_POLL_INTERVAL, Duration::from_millis(250));
        let mut watchdog = MemoryWatchdog::new(10 * 1024 * 1024);
        assert!(watchdog.observe(4 * 1024 * 1024).is_none());
        assert!(watchdog.observe(9 * 1024 * 1024).is_none());
        assert_eq!(watchdog.peak_rss, 9 * 1024 * 1024);

        let exceeded = watchdog
            .observe(11 * 1024 * 1024)
            .expect("triggering sample exceeds the limit");
        assert_eq!(exceeded.observed_rss, 11 * 1024 * 1024);
        assert_eq!(exceeded.peak_rss, 11 * 1024 * 1024);
        assert_eq!(exceeded.limit, 10 * 1024 * 1024);
        let detail = exceeded.detail();
        assert!(detail.starts_with("resource_limit_exceeded:"));
        assert!(detail.contains("observed_rss_bytes=11534336"));
        assert!(detail.contains("peak_rss_bytes=11534336"));
        assert!(detail.contains("limit_bytes=10485760"));
        let error = MobileLinuxError::ResourceLimitExceeded(exceeded.summary()).to_string();
        assert_eq!(error.matches("resource_limit_exceeded:").count(), 1);
        assert!(error.contains("observed_rss_bytes=11534336"));
        assert!(error.contains("peak_rss_bytes=11534336"));
        assert!(error.contains("limit_bytes=10485760"));
    }

    #[test]
    fn every_positive_max_memory_mb_value_is_supported() {
        let mut request = request();
        for megabytes in [1_u32, 17, u32::MAX] {
            request.resource_limits.max_memory_mb = Some(megabytes);
            assert_eq!(
                requested_memory_limit_bytes(&request).expect("positive memory limit"),
                Some(u64::from(megabytes) * 1024 * 1024),
            );
        }
    }

    #[tokio::test]
    async fn missing_payload_fails_closed_without_legacy_fallback() {
        let temp = tempfile::tempdir().expect("temp");
        let runtime = AndroidProotRuntime::new(AndroidProotRuntimeConfig {
            native_library_dir: None,
            isolated_build_profile: Some(test_build_profile()),
            managed_root: temp.path().join("missing"),
            app_sandbox_root: temp.path().join("sandbox"),
            abi: "x86_64".to_string(),
            rootfs_version: "test".to_string(),
            archive_sha256: None,
        });
        let capability = runtime.probe_capability().await;
        assert!(!capability.available);
        assert_eq!(capability.backend, SandboxBackend::AndroidProot);
        assert!(matches!(
            runtime.run(request()).await,
            Err(MobileLinuxError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn mount_validation_rejects_managed_root_and_traversal() {
        let (_temp, runtime) = runtime();
        let result = runtime
            .configure_mounts(vec![MountSpec {
                host_path: runtime.state.config.managed_root.clone(),
                guest_path: "/workspace/../root".to_string(),
                read_only: false,
                purpose: MountPurpose::Workspace,
            }])
            .await;
        assert!(matches!(result, Err(MobileLinuxError::InvalidRequest(_))));
    }

    #[test]
    fn merged_execution_mounts_keep_configured_binds() {
        let (temp, runtime) = runtime();
        let workspace_host = temp.path().join("workspace");
        let build_host = local_app_build_host(&temp, "app", "store");
        fs::create_dir_all(&workspace_host).expect("workspace host");
        runtime
            .state
            .mounts
            .write()
            .expect("mounts rwlock")
            .push(MountSpec {
                host_path: workspace_host,
                guest_path: "/workspace/default".to_string(),
                read_only: false,
                purpose: MountPurpose::Workspace,
            });
        let mounts = runtime
            .execution_mounts(
                &[MountSpec {
                    host_path: build_host,
                    guest_path: "/var/lingxi/local-app-build/app/store/project".to_string(),
                    read_only: false,
                    purpose: MountPurpose::LocalAppBuild,
                }],
                ForegroundMountMode::Merged,
            )
            .expect("merged mounts");
        assert_eq!(mounts.len(), 2);
        assert!(mounts
            .iter()
            .any(|mount| mount.guest_path == "/workspace/default"));
        assert!(mounts.iter().any(|mount| {
            mount.guest_path == "/var/lingxi/local-app-build/app/store/project"
                && matches!(mount.purpose, MountPurpose::LocalAppBuild)
        }));
    }

    #[test]
    fn isolated_execution_mounts_drop_configured_binds_but_keep_request_mounts() {
        let (temp, runtime) = runtime();
        let workspace_host = temp.path().join("workspace");
        let build_host = local_app_build_host(&temp, "app", "store");
        fs::create_dir_all(&workspace_host).expect("workspace host");
        runtime
            .state
            .mounts
            .write()
            .expect("mounts rwlock")
            .push(MountSpec {
                host_path: workspace_host,
                guest_path: "/workspace/default".to_string(),
                read_only: false,
                purpose: MountPurpose::Workspace,
            });
        let mounts = runtime
            .execution_mounts(
                &[MountSpec {
                    host_path: build_host,
                    guest_path: "/var/lingxi/local-app-build/app/store/project".to_string(),
                    read_only: false,
                    purpose: MountPurpose::LocalAppBuild,
                }],
                ForegroundMountMode::RequestOnly,
            )
            .expect("isolated mounts");
        assert_eq!(mounts.len(), 1);
        assert_eq!(
            mounts[0].guest_path,
            "/var/lingxi/local-app-build/app/store/project"
        );
        assert!(matches!(mounts[0].purpose, MountPurpose::LocalAppBuild));
    }

    #[test]
    fn isolated_execution_mounts_require_exactly_one_local_app_build_mount() {
        let (temp, runtime) = runtime();
        let build_host = local_app_build_host(&temp, "app", "store");
        let extra_host = temp.path().join("workspace");
        fs::create_dir_all(&extra_host).expect("workspace host");

        let error = runtime
            .execution_mounts(
                &[
                    MountSpec {
                        host_path: build_host.clone(),
                        guest_path: "/var/lingxi/local-app-build/app/store/project".to_string(),
                        read_only: false,
                        purpose: MountPurpose::LocalAppBuild,
                    },
                    MountSpec {
                        host_path: extra_host,
                        guest_path: "/workspace/default".to_string(),
                        read_only: false,
                        purpose: MountPurpose::Workspace,
                    },
                ],
                ForegroundMountMode::RequestOnly,
            )
            .expect_err("extra mounts must be rejected");
        assert!(error
            .to_string()
            .contains("exactly one LocalAppBuild mount"));
    }

    #[test]
    fn isolated_execution_mounts_reject_wrong_host_or_guest_shape() {
        let (temp, runtime) = runtime();
        let wrong_guest_host = local_app_build_host(&temp, "app", "store");
        let error = runtime
            .execution_mounts(
                &[MountSpec {
                    host_path: wrong_guest_host,
                    guest_path: "/workspace/default".to_string(),
                    read_only: false,
                    purpose: MountPurpose::LocalAppBuild,
                }],
                ForegroundMountMode::RequestOnly,
            )
            .expect_err("wrong guest path must be rejected");
        assert!(error.to_string().contains("guest_path must be"));

        let wrong_host = temp
            .path()
            .join("sandbox")
            .join("apps")
            .join("other")
            .join("build")
            .join("store");
        fs::create_dir_all(&wrong_host).expect("wrong host");
        let error = runtime
            .execution_mounts(
                &[MountSpec {
                    host_path: wrong_host,
                    guest_path: "/var/lingxi/local-app-build/app/store/project".to_string(),
                    read_only: false,
                    purpose: MountPurpose::LocalAppBuild,
                }],
                ForegroundMountMode::RequestOnly,
            )
            .expect_err("wrong host app id must be rejected");
        assert!(error.to_string().contains(&format!(
            "{}/apps/app/build/store",
            runtime.state.config.app_sandbox_root.display()
        )));
    }

    #[test]
    fn isolated_execution_mounts_reject_same_suffix_outside_app_sandbox_root() {
        let (temp, runtime) = runtime();
        let wrong_host = temp
            .path()
            .join("other-root")
            .join("apps")
            .join("app")
            .join("build")
            .join("store");
        fs::create_dir_all(&wrong_host).expect("wrong host");

        let error = runtime
            .execution_mounts(
                &[MountSpec {
                    host_path: wrong_host,
                    guest_path: "/var/lingxi/local-app-build/app/store/project".to_string(),
                    read_only: false,
                    purpose: MountPurpose::LocalAppBuild,
                }],
                ForegroundMountMode::RequestOnly,
            )
            .expect_err("same suffix outside sandbox root must be rejected");
        assert!(error.to_string().contains(&format!(
            "{}/apps/app/build/store",
            runtime.state.config.app_sandbox_root.display()
        )));
    }

    #[test]
    fn raw_stdio_read_only_mounts_use_isolated_snapshots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("workspace");
        let snapshot = temp.path().join("snapshots/raw-1");
        fs::create_dir_all(source.join("src")).expect("source");
        fs::create_dir_all(source.join("node_modules/react")).expect("node_modules");
        fs::write(
            source.join("src/app.jsx"),
            b"// @ts-check\nconst value = 1;\n",
        )
        .expect("source file");
        fs::write(source.join("node_modules/react/package.json"), b"{}").expect("dependency file");
        let (mounts, roots) = snapshot_read_only_mounts(
            vec![MountSpec {
                host_path: source.clone(),
                guest_path: "/workspace/lingxi-lsp-test".to_string(),
                read_only: true,
                purpose: MountPurpose::External,
            }],
            &snapshot,
            &AtomicBool::new(false),
        )
        .expect("snapshot mount");

        assert!(!mounts[0].read_only);
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[1].host_path, source.join("node_modules"));
        assert_eq!(
            mounts[1].guest_path,
            "/workspace/lingxi-lsp-test/node_modules"
        );
        assert_eq!(roots, vec![snapshot.clone()]);
        fs::write(mounts[0].host_path.join("src/app.jsx"), b"changed in guest")
            .expect("mutate snapshot");
        assert_eq!(
            fs::read(source.join("src/app.jsx")).expect("read source"),
            b"// @ts-check\nconst value = 1;\n"
        );
    }

    #[tokio::test]
    async fn event_reads_are_ordered_and_exclusive() {
        let (_temp, runtime) = runtime();
        runtime.emit(
            None,
            MobileLinuxEventKind::RuntimeError { detail: "a".into() },
        );
        runtime.emit(
            None,
            MobileLinuxEventKind::RuntimeError { detail: "b".into() },
        );
        let first = runtime.read_events(None, 1).await.expect("events");
        let rest = runtime
            .read_events(Some(first[0].sequence), 10)
            .await
            .expect("events");
        assert_eq!(first.len(), 1);
        assert_eq!(rest.len(), 1);
    }

    #[tokio::test]
    async fn run_keeps_stdout_and_stderr_separate_and_emits_one_terminal_state() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.command = "/bin/sh".to_string();
        request.args = vec![
            "-c".to_string(),
            "printf out; printf err >&2; exit 7".to_string(),
        ];
        let result = runtime.run(request).await.expect("run");
        assert_eq!(result.stdout, "out");
        assert_eq!(result.stderr, "err");
        assert_eq!(result.exit_code, 7);
        assert_eq!(result.enforcement, LinuxEnforcementReceipt::default());
        let tasks = runtime.list_tasks().await.expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, MobileLinuxTaskStatus::Failed);
        let terminal_events = runtime
            .read_events(None, 100)
            .await
            .expect("events")
            .into_iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    MobileLinuxEventKind::TaskStatusChanged {
                        status: MobileLinuxTaskStatus::Completed
                            | MobileLinuxTaskStatus::Failed
                            | MobileLinuxTaskStatus::Cancelled
                            | MobileLinuxTaskStatus::TimedOut,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(terminal_events, 1);
    }

    #[tokio::test]
    async fn disabled_network_fails_before_guest_spawn_without_policy_launcher() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.network = NetworkPolicy::Disabled;
        let error = runtime
            .run(request)
            .await
            .expect_err("launcher is required");
        assert!(matches!(
            error,
            MobileLinuxError::NetworkPolicyUnavailable(_)
        ));
        let tasks = runtime.list_tasks().await.expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, MobileLinuxTaskStatus::Failed);
    }

    #[tokio::test]
    async fn loopback_network_fails_before_guest_spawn_without_policy_launcher() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.network = NetworkPolicy::LoopbackOnly;
        let error = runtime
            .run(request)
            .await
            .expect_err("launcher and PRoot extension are required");
        assert!(matches!(
            error,
            MobileLinuxError::NetworkPolicyUnavailable(_)
        ));
        let tasks = runtime.list_tasks().await.expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, MobileLinuxTaskStatus::Failed);
    }

    #[test]
    fn loopback_only_is_admitted_for_sockaddr_aware_proot_enforcement() {
        let mut request = request();
        request.network = NetworkPolicy::LoopbackOnly;
        validate_request(&request, None, None).expect("LoopbackOnly is supported");
        assert_eq!(
            enforced_network_policy_name(request.network),
            Some("loopback_only")
        );
    }

    #[test]
    fn ordinary_requests_reject_build_state_env_overrides() {
        let mut request = request();
        request.env.insert(
            "HOME".into(),
            "/var/lingxi/local-app-build/app/store/project/.lingxi-build-state/home".into(),
        );
        let error =
            validate_request(&request, None, None).expect_err("ordinary requests must reject HOME");
        assert!(error
            .to_string()
            .contains("host-reserved environment variable"));
    }

    #[tokio::test]
    async fn isolated_local_app_build_accepts_fixed_build_env() {
        let (temp, runtime) = runtime();
        let build_host = local_app_build_host(&temp, "app", "store");
        let (project_guest_path, env) = fixed_local_app_build_env("app", "store");
        let mut request = request();
        request.command = "/bin/sh".into();
        request.args = vec![
            "-c".into(),
            "printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s' \
$HOME \"$TMPDIR\" \"$TMP\" \"$TEMP\" \"$XDG_CACHE_HOME\" \"$XDG_CONFIG_HOME\" \
\"$XDG_DATA_HOME\""
                .into(),
        ];
        request.cwd = Some(project_guest_path.clone());
        request.env = env.clone();
        request.mounts = vec![MountSpec {
            host_path: build_host,
            guest_path: project_guest_path,
            read_only: false,
            purpose: MountPurpose::LocalAppBuild,
        }];

        let result = runtime.run_isolated(request).await.expect("isolated run");
        let stdout_lines: Vec<_> = result.stdout.lines().collect();
        assert_eq!(stdout_lines.len(), env.len());
        let expected = [
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
        ]
        .into_iter()
        .map(|key| {
            env.get(key)
                .expect("fixed local-app build env key")
                .as_str()
        })
        .collect::<Vec<_>>();
        assert_eq!(stdout_lines, expected);
    }

    #[test]
    fn isolated_local_app_build_rejects_incomplete_fixed_build_env() {
        let (project_guest_path, fixed_env) = fixed_local_app_build_env("app", "store");
        for missing_key in fixed_env.keys() {
            let mut request = request();
            request.cwd = Some(project_guest_path.clone());
            request.env = fixed_env.clone();
            request.env.remove(missing_key);
            request.mounts = vec![MountSpec {
                host_path: PathBuf::from("/tmp/lingxi-local-app-build"),
                guest_path: project_guest_path.clone(),
                read_only: false,
                purpose: MountPurpose::LocalAppBuild,
            }];

            let error =
                validate_request(&request, Some(&request.mounts), Some(&test_build_profile()))
                    .expect_err("isolated builds require the complete fixed environment");
            assert!(error.to_string().contains(&format!(
                "isolated local-app build requires environment variable {missing_key}"
            )));
        }
    }

    #[tokio::test]
    async fn timeout_reaps_group_and_next_command_runs() {
        let (_temp, runtime) = runtime();
        let mut slow = request();
        slow.command = "/bin/sh".to_string();
        slow.args = vec!["-c".to_string(), "sleep 30".to_string()];
        slow.timeout_ms = Some(50);
        let timed_out = runtime.run(slow).await.expect("timeout result");
        assert!(timed_out.timed_out);

        let mut next = request();
        next.command = "/bin/sh".to_string();
        next.args = vec!["-c".to_string(), "printf ready".to_string()];
        let next = runtime.run(next).await.expect("next run");
        assert_eq!(next.stdout, "ready");
        assert_eq!(next.exit_code, 0);
    }

    #[tokio::test]
    async fn background_kill_is_cancelled_and_idempotent() {
        let (_temp, runtime) = runtime();
        let mut request = request();
        request.command = "/bin/sh".to_string();
        request.args = vec!["-c".to_string(), "sleep 30".to_string()];
        let handle = runtime.spawn_background(request).await.expect("background");
        runtime.kill(&handle).await.expect("kill");
        runtime.kill(&handle).await.expect("idempotent kill");
        let task = runtime
            .task_status(&handle.id)
            .await
            .expect("status")
            .expect("task");
        assert_eq!(task.status, MobileLinuxTaskStatus::Cancelled);
    }

    #[test]
    fn terminal_transition_is_emitted_once() {
        let (_temp, runtime) = runtime();
        let (id, task) = runtime.create_task("task", "true".into(), MobileLinuxTaskStatus::Running);
        runtime.finish_task(&id, &task, MobileLinuxTaskStatus::Completed, Some(0), None);
        runtime.finish_task(&id, &task, MobileLinuxTaskStatus::Cancelled, None, None);
        let terminal = runtime
            .state
            .events
            .lock()
            .expect("events")
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    MobileLinuxEventKind::TaskStatusChanged {
                        status: MobileLinuxTaskStatus::Completed
                            | MobileLinuxTaskStatus::Failed
                            | MobileLinuxTaskStatus::Cancelled
                            | MobileLinuxTaskStatus::TimedOut,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(terminal, 1);
    }
}
