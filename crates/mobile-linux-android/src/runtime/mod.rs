use mobile_linux_api::LinuxEnforcementReceipt;
use mobile_linux_api::{
    MobileLinuxError, MobileLinuxEvent, MobileLinuxEventKind, MobileLinuxTaskSnapshot,
    MobileLinuxTaskStatus, MountSpec,
};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin};

pub use config::AndroidProotRuntimeConfig;
use mounts::{
    check_snapshot_cancelled, snapshot_read_only_mounts, validate_mount, validate_pty_request,
    validate_request,
};
use process::{requested_memory_limit_bytes, terminate_and_reap, terminate_group, wait_for_child};
use raw_stdio::{raw_stdin_closed, wait_raw_stop};
use rootfs::{executable_regular_file, rootfs_store_error};
use streaming::{join_reader, read_stderr, read_stdout};
mod background;
mod config;
mod implementation;
mod mounts;
mod process;
mod pty;
mod raw_stdio;
mod rootfs;
mod streaming;

const MAX_EVENTS: usize = 4096;

const MAX_CAPTURE_BYTES: usize = 256 * 1024;

const MAX_STDOUT_FRAGMENT_BYTES: usize = 16 * 1024;

const MAX_RAW_STDIO_BUFFER_BYTES: usize = 512 * 1024;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

const REAP_BUDGET: Duration = Duration::from_secs(2);

const ENFORCEMENT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(3);

const MEMORY_POLL_INTERVAL: Duration = Duration::from_millis(250);

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

#[derive(Clone, Copy)]
enum ForegroundMountMode {
    Merged,
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

#[cfg(test)]
mod tests;
