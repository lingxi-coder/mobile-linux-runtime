//! Standalone Mobile Linux FFI. No application, Harness, or client protocol dependency.
use mobile_linux_api as platform_api;
#[cfg(any(target_os = "android", target_os = "ios"))]
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
uniffi::setup_scaffolding!("mobile_linux_runtime");
const MAX_MOBILE_LINUX_EVENT_BATCH: usize = 512;
static MOBILE_LINUX_FFI_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum MobileLinuxRuntimeModeFfi {
    Legacy,
    MobileLinux,
}
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum MobileLinuxRootfsStateFfi {
    Missing,
    Installing,
    Ready,
    Corrupt,
    Repairing,
    Resetting,
    Unsupported,
    BlockedByLicense,
}

/// FFI capability snapshot for the Android mobile-linux runtime.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxCapabilityFfi {
    pub available: bool,
    pub backend: String,
    pub mode: MobileLinuxRuntimeModeFfi,
    pub reason: Option<String>,
    pub streaming_output: bool,
    pub background_processes: bool,
    pub pty: bool,
    pub bind_mounts: bool,
    pub rootfs_integrity: bool,
}

/// FFI rootfs status snapshot for the Android mobile-linux runtime.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxStatusFfi {
    pub state: MobileLinuxRootfsStateFfi,
    pub backend: String,
    pub mode: MobileLinuxRuntimeModeFfi,
    pub platform: String,
    pub abi: String,
    pub version: Option<String>,
    pub managed_root: Option<String>,
    pub active_root: Option<String>,
    pub staged_root: Option<String>,
    pub archive_sha256: Option<String>,
    pub installed_size_bytes: Option<u64>,
    pub writable_guest_paths: Vec<String>,
    pub last_error: Option<String>,
}

/// FFI mount purpose for the mobile-linux runtime.
#[derive(uniffi::Enum, Debug, Clone, Copy)]
pub enum MobileLinuxMountPurposeFfi {
    Workspace,
    LocalAppBuild,
    Memory,
    Skills,
    Shared,
    External,
    Temp,
}

/// FFI mount descriptor for guest-visible paths.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxMountSpecFfi {
    pub host_path: String,
    pub guest_path: String,
    pub read_only: bool,
    pub purpose: MobileLinuxMountPurposeFfi,
}

#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxEnvEntryFfi {
    pub key: String,
    pub value: String,
}

/// FFI request for a command executed inside the mobile-linux runtime.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxCommandRequestFfi {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<MobileLinuxEnvEntryFfi>,
    pub stdin: Option<String>,
    pub timeout_ms: Option<u64>,
    pub network: NetworkPolicyFfi,
    pub resource_limits: ResourceLimitsFfi,
    pub mounts: Vec<MobileLinuxMountSpecFfi>,
}

/// FFI command result returned by the mobile-linux runtime.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxCommandResultFfi {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub cancelled: bool,
    pub enforcement: EnforcementReceiptFfi,
}

/// FFI background-process handle.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxProcessHandleFfi {
    pub id: String,
    pub enforcement: EnforcementReceiptFfi,
}

/// FFI PTY size.
#[derive(uniffi::Record, Debug, Clone, Copy)]
pub struct MobileLinuxPtySizeFfi {
    pub cols: u16,
    pub rows: u16,
}

/// FFI PTY open request.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxPtyOpenRequestFfi {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<MobileLinuxEnvEntryFfi>,
    pub size: MobileLinuxPtySizeFfi,
    pub mounts: Vec<MobileLinuxMountSpecFfi>,
}

/// FFI PTY-session handle.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxPtySessionHandleFfi {
    pub id: String,
}

/// FFI stream sink for live command output.
#[uniffi::export(callback_interface)]
#[async_trait::async_trait]
pub trait RuntimeStreamSink: Send + Sync {
    async fn stdout_line(&self, line: String) -> Result<(), MobileLinuxApiErrorFfi>;
    async fn stderr_chunk(&self, chunk: Vec<u8>) -> Result<(), MobileLinuxApiErrorFfi>;
}

/// FFI error surface for mobile-linux operations.
#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum MobileLinuxApiErrorFfi {
    #[error("rootfs integrity failure: {detail}")]
    Integrity { detail: String },
    #[error("network policy unavailable: {detail}")]
    NetworkPolicyUnavailable { detail: String },
    #[error("resource limit exceeded: {detail}")]
    ResourceLimitExceeded { detail: String },
    #[error("I/O failure: {detail}")]
    Io { detail: String },
    #[error("operation timed out")]
    Timeout,
    #[error("application process restart required: {detail}")]
    RestartRequired { detail: String },
    #[error("legacy backend selected")]
    LegacySelected,
    #[error("mobile-linux runtime unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("mobile-linux runtime license blocked: {detail}")]
    LicenseBlocked { detail: String },
    #[error("invalid mobile-linux request: {detail}")]
    InvalidRequest { detail: String },
    #[error("mobile-linux operation failed: {detail}")]
    OperationFailed { detail: String },
}

/// Event kind emitted by run/PTY streaming APIs.
#[derive(uniffi::Enum, Debug, Clone, Copy)]
pub enum MobileLinuxEventKindFfi {
    TaskStatusChanged,
    StdoutLine,
    StderrChunk,
    PtyOutput,
    PtyClosed,
    RuntimeError,
}

/// Streaming/runtime event delivered to the Android host.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxEventFfi {
    pub sequence: u64,
    pub task_id: Option<String>,
    pub kind: MobileLinuxEventKindFfi,
    pub data: Option<Vec<u8>>,
    pub text: Option<String>,
    pub session_id: Option<String>,
    pub status: Option<MobileLinuxTaskStateFfi>,
    pub exit_code: Option<i32>,
    pub timed_out: Option<bool>,
    pub cancelled: Option<bool>,
    pub detail: Option<String>,
}

/// Crate-local callback interface for mobile-linux events.
#[uniffi::export(callback_interface)]
#[async_trait::async_trait]
pub trait RuntimeEventSink: Send + Sync {
    async fn on_event(&self, event: MobileLinuxEventFfi) -> Result<(), MobileLinuxApiErrorFfi>;
}

/// FFI task kind for host-visible runtime work.
#[derive(uniffi::Enum, Debug, Clone, Copy)]
pub enum MobileLinuxTaskKindFfi {
    Command,
    PtySession,
}

/// FFI task status for host-visible runtime work.
#[derive(uniffi::Enum, Debug, Clone, Copy)]
pub enum MobileLinuxTaskStateFfi {
    Queued,
    Running,
    Backgrounded,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// FFI task snapshot. Phase-1 keeps this fail-closed unless a real runtime is linked.
#[derive(uniffi::Record, Debug, Clone)]
pub struct MobileLinuxTaskSnapshotFfi {
    pub task_id: String,
    pub status: MobileLinuxTaskStateFfi,
    pub command: String,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub detail: Option<String>,
}

fn mobile_linux_backend_name(backend: platform_api::SandboxBackend) -> String {
    match backend {
        platform_api::SandboxBackend::LinuxNamespaces => "linux-namespaces",
        platform_api::SandboxBackend::LinuxFirejail => "linux-firejail",
        platform_api::SandboxBackend::MacOsSandboxExec => "macos-sandbox-exec",
        platform_api::SandboxBackend::WindowsJobObject => "windows-job-object",
        platform_api::SandboxBackend::AndroidMinijail => "android-minijail",
        platform_api::SandboxBackend::AndroidProot => "android-proot",
        platform_api::SandboxBackend::IosIsh => "ios-ish",
        platform_api::SandboxBackend::None => "none",
    }
    .to_string()
}

fn rootfs_state_to_ffi(state: platform_api::RootfsState) -> MobileLinuxRootfsStateFfi {
    match state {
        platform_api::RootfsState::Missing => MobileLinuxRootfsStateFfi::Missing,
        platform_api::RootfsState::Installing => MobileLinuxRootfsStateFfi::Installing,
        platform_api::RootfsState::Ready => MobileLinuxRootfsStateFfi::Ready,
        platform_api::RootfsState::Corrupt => MobileLinuxRootfsStateFfi::Corrupt,
        platform_api::RootfsState::Repairing => MobileLinuxRootfsStateFfi::Repairing,
        platform_api::RootfsState::Resetting => MobileLinuxRootfsStateFfi::Resetting,
        platform_api::RootfsState::Unsupported => MobileLinuxRootfsStateFfi::Unsupported,
        platform_api::RootfsState::BlockedByLicense => MobileLinuxRootfsStateFfi::BlockedByLicense,
    }
}

fn capability_to_ffi(capability: platform_api::MobileLinuxCapability) -> MobileLinuxCapabilityFfi {
    MobileLinuxCapabilityFfi {
        available: capability.available,
        backend: mobile_linux_backend_name(capability.backend),
        mode: match capability.mode {
            platform_api::MobileLinuxRuntimeMode::Legacy => MobileLinuxRuntimeModeFfi::Legacy,
            platform_api::MobileLinuxRuntimeMode::MobileLinux => {
                MobileLinuxRuntimeModeFfi::MobileLinux
            }
        },
        reason: capability.reason,
        streaming_output: capability.streaming_output,
        background_processes: capability.background_processes,
        pty: capability.pty,
        bind_mounts: capability.bind_mounts,
        rootfs_integrity: capability.rootfs_integrity,
    }
}

fn status_to_ffi(status: platform_api::RootfsStatus) -> MobileLinuxStatusFfi {
    MobileLinuxStatusFfi {
        state: rootfs_state_to_ffi(status.state),
        backend: mobile_linux_backend_name(status.backend),
        mode: match status.mode {
            platform_api::MobileLinuxRuntimeMode::Legacy => MobileLinuxRuntimeModeFfi::Legacy,
            platform_api::MobileLinuxRuntimeMode::MobileLinux => {
                MobileLinuxRuntimeModeFfi::MobileLinux
            }
        },
        platform: status.platform,
        abi: status.abi,
        version: status.version,
        managed_root: status
            .managed_root
            .map(|p| p.to_string_lossy().into_owned()),
        active_root: status.active_root.map(|p| p.to_string_lossy().into_owned()),
        staged_root: status.staged_root.map(|p| p.to_string_lossy().into_owned()),
        archive_sha256: status.archive_sha256,
        installed_size_bytes: status.installed_size_bytes,
        writable_guest_paths: status.writable_guest_paths,
        last_error: status.last_error,
    }
}

fn mount_purpose_to_traits(value: MobileLinuxMountPurposeFfi) -> platform_api::MountPurpose {
    match value {
        MobileLinuxMountPurposeFfi::Workspace => platform_api::MountPurpose::Workspace,
        MobileLinuxMountPurposeFfi::LocalAppBuild => platform_api::MountPurpose::LocalAppBuild,
        MobileLinuxMountPurposeFfi::Memory => platform_api::MountPurpose::Memory,
        MobileLinuxMountPurposeFfi::Skills => platform_api::MountPurpose::Skills,
        MobileLinuxMountPurposeFfi::Shared => platform_api::MountPurpose::Shared,
        MobileLinuxMountPurposeFfi::External => platform_api::MountPurpose::External,
        MobileLinuxMountPurposeFfi::Temp => platform_api::MountPurpose::Temp,
    }
}

fn command_request_to_traits(
    request: MobileLinuxCommandRequestFfi,
) -> Result<platform_api::LinuxCommandRequest, MobileLinuxApiErrorFfi> {
    let command = request.command.trim();
    if command.is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "command must not be empty".to_string(),
        });
    }
    let mounts = request
        .mounts
        .into_iter()
        .map(mount_spec_to_traits)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(platform_api::LinuxCommandRequest {
        command: command.to_string(),
        args: request.args,
        cwd: request.cwd,
        env: request
            .env
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect(),
        stdin: request.stdin,
        timeout_ms: request.timeout_ms,
        network: request.network.into(),
        resource_limits: request.resource_limits.into(),
        mounts,
    })
}

fn command_result_to_ffi(result: platform_api::LinuxCommandResult) -> MobileLinuxCommandResultFfi {
    MobileLinuxCommandResultFfi {
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        cancelled: result.cancelled,
        enforcement: result.enforcement.into(),
    }
}

fn task_status_to_ffi(status: platform_api::MobileLinuxTaskStatus) -> MobileLinuxTaskStateFfi {
    match status {
        platform_api::MobileLinuxTaskStatus::Queued => MobileLinuxTaskStateFfi::Queued,
        platform_api::MobileLinuxTaskStatus::Running => MobileLinuxTaskStateFfi::Running,
        platform_api::MobileLinuxTaskStatus::Backgrounded => MobileLinuxTaskStateFfi::Backgrounded,
        platform_api::MobileLinuxTaskStatus::Completed => MobileLinuxTaskStateFfi::Completed,
        platform_api::MobileLinuxTaskStatus::Failed => MobileLinuxTaskStateFfi::Failed,
        platform_api::MobileLinuxTaskStatus::Cancelled => MobileLinuxTaskStateFfi::Cancelled,
        platform_api::MobileLinuxTaskStatus::TimedOut => MobileLinuxTaskStateFfi::TimedOut,
    }
}

fn task_snapshot_to_ffi(task: platform_api::MobileLinuxTaskSnapshot) -> MobileLinuxTaskSnapshotFfi {
    MobileLinuxTaskSnapshotFfi {
        task_id: task.task_id,
        status: task_status_to_ffi(task.status),
        command: task.command,
        started_at_ms: task.started_at_ms,
        finished_at_ms: task.finished_at_ms,
        exit_code: task.exit_code,
        detail: task.detail,
    }
}

fn event_to_ffi(event: platform_api::MobileLinuxEvent) -> MobileLinuxEventFfi {
    match event.kind {
        platform_api::MobileLinuxEventKind::TaskStatusChanged {
            status,
            exit_code,
            detail,
        } => MobileLinuxEventFfi {
            sequence: event.sequence,
            task_id: event.task_id,
            kind: MobileLinuxEventKindFfi::TaskStatusChanged,
            data: None,
            text: None,
            session_id: None,
            status: Some(task_status_to_ffi(status)),
            exit_code,
            timed_out: Some(matches!(
                status,
                platform_api::MobileLinuxTaskStatus::TimedOut
            )),
            cancelled: Some(matches!(
                status,
                platform_api::MobileLinuxTaskStatus::Cancelled
            )),
            detail,
        },
        platform_api::MobileLinuxEventKind::StdoutLine { line } => MobileLinuxEventFfi {
            sequence: event.sequence,
            task_id: event.task_id,
            kind: MobileLinuxEventKindFfi::StdoutLine,
            data: None,
            text: Some(line),
            session_id: None,
            status: None,
            exit_code: None,
            timed_out: None,
            cancelled: None,
            detail: None,
        },
        platform_api::MobileLinuxEventKind::StderrChunk { chunk } => MobileLinuxEventFfi {
            sequence: event.sequence,
            task_id: event.task_id,
            kind: MobileLinuxEventKindFfi::StderrChunk,
            data: Some(chunk),
            text: None,
            session_id: None,
            status: None,
            exit_code: None,
            timed_out: None,
            cancelled: None,
            detail: None,
        },
        platform_api::MobileLinuxEventKind::PtyOutput { session_id, data } => MobileLinuxEventFfi {
            sequence: event.sequence,
            task_id: event.task_id,
            kind: MobileLinuxEventKindFfi::PtyOutput,
            data: Some(data),
            text: None,
            session_id: Some(session_id),
            status: None,
            exit_code: None,
            timed_out: None,
            cancelled: None,
            detail: None,
        },
        platform_api::MobileLinuxEventKind::PtyClosed {
            session_id,
            exit_code,
            detail,
        } => MobileLinuxEventFfi {
            sequence: event.sequence,
            task_id: event.task_id,
            kind: MobileLinuxEventKindFfi::PtyClosed,
            data: None,
            text: None,
            session_id: Some(session_id),
            status: None,
            exit_code,
            timed_out: None,
            cancelled: None,
            detail,
        },
        platform_api::MobileLinuxEventKind::RuntimeError { detail } => MobileLinuxEventFfi {
            sequence: event.sequence,
            task_id: event.task_id,
            kind: MobileLinuxEventKindFfi::RuntimeError,
            data: None,
            text: None,
            session_id: None,
            status: None,
            exit_code: None,
            timed_out: None,
            cancelled: None,
            detail: Some(detail),
        },
    }
}

fn mount_spec_to_traits(
    mount: MobileLinuxMountSpecFfi,
) -> Result<platform_api::MountSpec, MobileLinuxApiErrorFfi> {
    if mount.guest_path.trim().is_empty() || !mount.guest_path.starts_with('/') {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: format!("invalid guest mount path: {}", mount.guest_path),
        });
    }
    if mount.host_path.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "host mount path must not be empty".to_string(),
        });
    }
    Ok(platform_api::MountSpec {
        host_path: std::path::PathBuf::from(mount.host_path),
        guest_path: mount.guest_path,
        read_only: mount.read_only,
        purpose: mount_purpose_to_traits(mount.purpose),
    })
}

fn pty_open_request_to_traits(
    request: MobileLinuxPtyOpenRequestFfi,
) -> Result<platform_api::PtyOpenRequest, MobileLinuxApiErrorFfi> {
    let command = request.command.trim();
    if command.is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "pty command must not be empty".to_string(),
        });
    }
    let mounts = request
        .mounts
        .into_iter()
        .map(mount_spec_to_traits)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(platform_api::PtyOpenRequest {
        command: command.to_string(),
        args: request.args,
        cwd: request.cwd,
        env: request
            .env
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect(),
        size: platform_api::PtySize {
            cols: request.size.cols,
            rows: request.size.rows,
        },
        mounts,
    })
}

fn process_handle_to_ffi(handle: platform_api::LinuxProcessHandle) -> MobileLinuxProcessHandleFfi {
    MobileLinuxProcessHandleFfi {
        id: handle.id,
        enforcement: handle.enforcement.into(),
    }
}

fn process_handle_to_traits(
    handle: MobileLinuxProcessHandleFfi,
) -> Result<platform_api::LinuxProcessHandle, MobileLinuxApiErrorFfi> {
    if handle.id.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "process handle id must not be empty".to_string(),
        });
    }
    Ok(platform_api::LinuxProcessHandle {
        id: handle.id,
        enforcement: platform_api::LinuxEnforcementReceipt {
            network_policy_enforced: handle.enforcement.network_policy_enforced,
            memory_limit_enforced: handle.enforcement.memory_limit_enforced,
        },
    })
}

fn pty_handle_to_ffi(handle: platform_api::PtySessionHandle) -> MobileLinuxPtySessionHandleFfi {
    MobileLinuxPtySessionHandleFfi { id: handle.id }
}

fn pty_handle_to_traits(
    handle: MobileLinuxPtySessionHandleFfi,
) -> Result<platform_api::PtySessionHandle, MobileLinuxApiErrorFfi> {
    if handle.id.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "pty handle id must not be empty".to_string(),
        });
    }
    Ok(platform_api::PtySessionHandle { id: handle.id })
}

fn mobile_linux_error_to_ffi(error: platform_api::MobileLinuxError) -> MobileLinuxApiErrorFfi {
    match error {
        platform_api::MobileLinuxError::RestartRequired(message) => {
            MobileLinuxApiErrorFfi::RestartRequired { detail: message }
        }
        platform_api::MobileLinuxError::Unsupported => MobileLinuxApiErrorFfi::Unavailable {
            detail: "runtime unsupported on this build".to_string(),
        },
        platform_api::MobileLinuxError::Unavailable(message) => {
            MobileLinuxApiErrorFfi::Unavailable { detail: message }
        }
        platform_api::MobileLinuxError::LicenseBlocked(message) => {
            MobileLinuxApiErrorFfi::LicenseBlocked { detail: message }
        }
        platform_api::MobileLinuxError::InvalidRequest(message) => {
            MobileLinuxApiErrorFfi::InvalidRequest { detail: message }
        }
        platform_api::MobileLinuxError::Integrity(message) => {
            MobileLinuxApiErrorFfi::Integrity { detail: message }
        }
        platform_api::MobileLinuxError::Io(message) => {
            MobileLinuxApiErrorFfi::Io { detail: message }
        }
        platform_api::MobileLinuxError::NetworkPolicyUnavailable(message) => {
            MobileLinuxApiErrorFfi::NetworkPolicyUnavailable { detail: message }
        }
        platform_api::MobileLinuxError::ResourceLimitExceeded(message) => {
            MobileLinuxApiErrorFfi::ResourceLimitExceeded { detail: message }
        }
        platform_api::MobileLinuxError::Timeout => MobileLinuxApiErrorFfi::Timeout,
    }
}

struct RuntimeEventSinkBridge {
    task_id: String,
    inner: Box<dyn RuntimeEventSink>,
}

impl RuntimeEventSinkBridge {
    async fn emit_event(&self, event: MobileLinuxEventFfi) -> Result<(), MobileLinuxApiErrorFfi> {
        self.inner.on_event(event).await
    }
}

#[async_trait::async_trait]
impl platform_api::ProcessStreamSink for RuntimeEventSinkBridge {
    async fn stdout_line(&self, line: String) -> Result<(), platform_api::ProcessError> {
        self.inner
            .on_event(MobileLinuxEventFfi {
                sequence: 0,
                task_id: Some(self.task_id.clone()),
                kind: MobileLinuxEventKindFfi::StdoutLine,
                data: None,
                text: Some(line),
                session_id: None,
                status: None,
                exit_code: None,
                timed_out: None,
                cancelled: None,
                detail: None,
            })
            .await
            .map_err(|err| platform_api::ProcessError::Io(err.to_string()))
    }

    async fn stderr_chunk(&self, chunk: Vec<u8>) -> Result<(), platform_api::ProcessError> {
        self.inner
            .on_event(MobileLinuxEventFfi {
                sequence: 0,
                task_id: Some(self.task_id.clone()),
                kind: MobileLinuxEventKindFfi::StderrChunk,
                data: Some(chunk),
                text: None,
                session_id: None,
                status: None,
                exit_code: None,
                timed_out: None,
                cancelled: None,
                detail: None,
            })
            .await
            .map_err(|err| platform_api::ProcessError::Io(err.to_string()))
    }
}

#[derive(uniffi::Object)]
pub struct RuntimeHandle {
    runtime: Arc<dyn platform_api::MobileLinuxRuntime>,
}
#[uniffi::export(async_runtime = "tokio")]
impl RuntimeHandle {
    pub async fn capability(&self) -> MobileLinuxCapabilityFfi {
        capability_to_ffi(self.runtime.probe_capability().await)
    }

    pub async fn status(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .rootfs_status()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn boot(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .boot()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn shutdown(&self) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .shutdown()
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn verify_rootfs(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .verify_rootfs()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn repair_rootfs(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .repair_rootfs()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn reset_rootfs(&self) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .reset_rootfs()
            .await
            .map(status_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn run_command(
        &self,
        request: MobileLinuxCommandRequestFfi,
    ) -> Result<MobileLinuxCommandResultFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .run(command_request_to_traits(request)?)
            .await
            .map(command_result_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn run_command_streaming(
        &self,
        request: MobileLinuxCommandRequestFfi,
        sink: Box<dyn RuntimeEventSink>,
    ) -> Result<MobileLinuxCommandResultFfi, MobileLinuxApiErrorFfi> {
        let task_id = format!(
            "run-{}",
            MOBILE_LINUX_FFI_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let sink = Arc::new(RuntimeEventSinkBridge {
            task_id,
            inner: sink,
        });
        let result = self
            .runtime
            .run_streaming(command_request_to_traits(request)?, sink.clone())
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        sink.emit_event(MobileLinuxEventFfi {
            sequence: 0,
            task_id: Some(sink.task_id.clone()),
            kind: MobileLinuxEventKindFfi::TaskStatusChanged,
            data: None,
            text: None,
            session_id: None,
            status: Some(if result.cancelled {
                MobileLinuxTaskStateFfi::Cancelled
            } else if result.timed_out {
                MobileLinuxTaskStateFfi::TimedOut
            } else if result.exit_code == 0 {
                MobileLinuxTaskStateFfi::Completed
            } else {
                MobileLinuxTaskStateFfi::Failed
            }),
            exit_code: Some(result.exit_code),
            timed_out: Some(result.timed_out),
            cancelled: Some(result.cancelled),
            detail: None,
        })
        .await?;
        Ok(command_result_to_ffi(result))
    }

    pub async fn spawn_background(
        &self,
        request: MobileLinuxCommandRequestFfi,
    ) -> Result<MobileLinuxProcessHandleFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .spawn_background(command_request_to_traits(request)?)
            .await
            .map(process_handle_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn kill_process(
        &self,
        handle: MobileLinuxProcessHandleFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .kill(&process_handle_to_traits(handle)?)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn open_pty(
        &self,
        request: MobileLinuxPtyOpenRequestFfi,
    ) -> Result<MobileLinuxPtySessionHandleFfi, MobileLinuxApiErrorFfi> {
        self.runtime
            .open_pty(pty_open_request_to_traits(request)?)
            .await
            .map(pty_handle_to_ffi)
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn write_pty(
        &self,
        handle: MobileLinuxPtySessionHandleFfi,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .write_pty(&pty_handle_to_traits(handle)?, input)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn resize_pty(
        &self,
        handle: MobileLinuxPtySessionHandleFfi,
        size: MobileLinuxPtySizeFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .resize_pty(
                &pty_handle_to_traits(handle)?,
                platform_api::PtySize {
                    cols: size.cols,
                    rows: size.rows,
                },
            )
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn close_pty(
        &self,
        handle: MobileLinuxPtySessionHandleFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .close_pty(&pty_handle_to_traits(handle)?)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn configure_mounts(
        &self,
        mounts: Vec<MobileLinuxMountSpecFfi>,
    ) -> Result<MobileLinuxStatusFfi, MobileLinuxApiErrorFfi> {
        let mounts = mounts
            .into_iter()
            .map(mount_spec_to_traits)
            .collect::<Result<Vec<_>, _>>()?;
        self.runtime
            .configure_mounts(mounts)
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        self.status().await
    }

    pub async fn read_events(
        &self,
        after_sequence: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<MobileLinuxEventFfi>, MobileLinuxApiErrorFfi> {
        let limit = limit
            .map(|value| value as usize)
            .unwrap_or(MAX_MOBILE_LINUX_EVENT_BATCH)
            .min(MAX_MOBILE_LINUX_EVENT_BATCH);
        self.runtime
            .read_events(after_sequence, limit)
            .await
            .map(|events| events.into_iter().map(event_to_ffi).collect())
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn list_tasks(
        &self,
    ) -> Result<Vec<MobileLinuxTaskSnapshotFfi>, MobileLinuxApiErrorFfi> {
        self.runtime
            .list_tasks()
            .await
            .map(|tasks| tasks.into_iter().map(task_snapshot_to_ffi).collect())
            .map_err(mobile_linux_error_to_ffi)
    }

    pub async fn task_status(
        &self,
        task_id: String,
    ) -> Result<Option<MobileLinuxTaskSnapshotFfi>, MobileLinuxApiErrorFfi> {
        if task_id.trim().is_empty() {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "task_id must not be empty".to_string(),
            });
        }
        self.runtime
            .task_status(&task_id)
            .await
            .map(|task| task.map(task_snapshot_to_ffi))
            .map_err(mobile_linux_error_to_ffi)
    }
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum RuntimePlatform {
    Android,
    Ios,
}
#[derive(Debug, Clone, uniffi::Record)]
pub struct RuntimeConfig {
    pub platform: RuntimePlatform,
    pub managed_root: String,
    pub app_sandbox_root: String,
    pub abi: String,
    pub rootfs_version: String,
    pub archive_sha256: Option<String>,
    pub native_library_dir: Option<String>,
    pub workspace_host_path: Option<String>,
    pub stable_workspace_id: Option<String>,
    pub authorization_file: Option<String>,
    pub rootfs_archive_path: Option<String>,
    #[uniffi(default = None)]
    pub rootfs_patch_path: Option<String>,
    pub default_mount_path: Option<String>,
    pub protected_host_roots: Vec<String>,
    pub allowed_mount_roots: Vec<String>,
    pub allowed_guest_roots: Vec<String>,
}
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum NetworkPolicyFfi {
    Disabled,
    LoopbackOnly,
    Allowed,
}
impl From<NetworkPolicyFfi> for platform_api::NetworkPolicy {
    fn from(value: NetworkPolicyFfi) -> Self {
        match value {
            NetworkPolicyFfi::Disabled => Self::Disabled,
            NetworkPolicyFfi::LoopbackOnly => Self::LoopbackOnly,
            NetworkPolicyFfi::Allowed => Self::Allowed,
        }
    }
}
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct ResourceLimitsFfi {
    pub max_cpu_seconds: Option<u32>,
    pub max_memory_mb: Option<u32>,
    pub max_processes: Option<u32>,
    pub max_open_files: Option<u32>,
}
impl From<ResourceLimitsFfi> for platform_api::ResourceLimits {
    fn from(v: ResourceLimitsFfi) -> Self {
        Self {
            max_cpu_seconds: v.max_cpu_seconds,
            max_memory_mb: v.max_memory_mb,
            max_processes: v.max_processes,
            max_open_files: v.max_open_files,
        }
    }
}
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct EnforcementReceiptFfi {
    pub network_policy_enforced: bool,
    pub memory_limit_enforced: bool,
}
impl From<platform_api::LinuxEnforcementReceipt> for EnforcementReceiptFfi {
    fn from(v: platform_api::LinuxEnforcementReceipt) -> Self {
        Self {
            network_policy_enforced: v.network_policy_enforced,
            memory_limit_enforced: v.memory_limit_enforced,
        }
    }
}
#[derive(Debug, Clone, uniffi::Record)]
pub struct RawStdioHandle {
    pub id: String,
    pub enforcement: EnforcementReceiptFfi,
}
#[derive(Debug, Clone, uniffi::Record)]
pub struct RawStdioOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub closed: bool,
    pub exit_code: Option<i32>,
}
#[uniffi::export(async_runtime = "tokio")]
impl RuntimeHandle {
    pub async fn open_raw_stdio(
        &self,
        request: MobileLinuxCommandRequestFfi,
    ) -> Result<RawStdioHandle, MobileLinuxApiErrorFfi> {
        if request.stdin.is_some() || request.timeout_ms.is_some() {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "Raw stdio writes and lifetime are controlled through the session handle"
                    .into(),
            });
        }
        let r = command_request_to_traits(request)?;
        let h = self
            .runtime
            .open_raw_stdio(platform_api::RawStdioOpenRequest {
                command: r.command,
                args: r.args,
                cwd: r.cwd,
                env: r.env,
                network: r.network,
                resource_limits: r.resource_limits,
                mounts: r.mounts,
            })
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        Ok(RawStdioHandle {
            id: h.id,
            enforcement: h.enforcement.into(),
        })
    }
    pub async fn write_raw_stdio(
        &self,
        id: String,
        data: Vec<u8>,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .write_raw_stdio(&raw_handle(id)?, data)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }
    pub async fn read_raw_stdio(
        &self,
        id: String,
        max_bytes: u32,
    ) -> Result<RawStdioOutput, MobileLinuxApiErrorFfi> {
        if max_bytes == 0 || max_bytes > 1048576 {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "max_bytes must be 1..1048576".into(),
            });
        }
        let v = self
            .runtime
            .read_raw_stdio(&raw_handle(id)?, max_bytes as usize)
            .await
            .map_err(mobile_linux_error_to_ffi)?;
        Ok(RawStdioOutput {
            stdout: v.stdout,
            stderr: v.stderr,
            closed: v.closed,
            exit_code: v.exit_code,
        })
    }
    pub async fn close_raw_stdio(&self, id: String) -> Result<(), MobileLinuxApiErrorFfi> {
        self.runtime
            .close_raw_stdio(&raw_handle(id)?)
            .await
            .map_err(mobile_linux_error_to_ffi)
    }
}
fn raw_handle(id: String) -> Result<platform_api::RawStdioSessionHandle, MobileLinuxApiErrorFfi> {
    if id.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "empty raw stdio handle".into(),
        });
    }
    Ok(platform_api::RawStdioSessionHandle {
        id,
        enforcement: Default::default(),
    })
}
#[uniffi::export]
pub fn create_runtime(config: RuntimeConfig) -> Result<Arc<RuntimeHandle>, MobileLinuxApiErrorFfi> {
    for value in [&config.managed_root, &config.app_sandbox_root] {
        if !std::path::Path::new(value).is_absolute() {
            return Err(MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "runtime roots must be absolute".into(),
            });
        }
    }
    if config.rootfs_version.trim().is_empty() || config.abi.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "rootfs version and ABI are required".into(),
        });
    }
    #[cfg(target_os = "android")]
    if matches!(config.platform, RuntimePlatform::Android) {
        let runtime = mobile_linux_android::AndroidProotRuntime::new(
            mobile_linux_android::AndroidProotRuntimeConfig {
                managed_root: PathBuf::from(&config.managed_root),
                app_sandbox_root: PathBuf::from(&config.app_sandbox_root),
                abi: config.abi.clone(),
                rootfs_version: config.rootfs_version.clone(),
                archive_sha256: config.archive_sha256.clone(),
                native_library_dir: config.native_library_dir.clone().map(PathBuf::from),
                isolated_build_profile: None,
            },
        );
        return Ok(Arc::new(RuntimeHandle {
            runtime: Arc::new(runtime),
        }));
    }
    create_ios_or_unavailable(config)
}
fn create_ios_or_unavailable(
    config: RuntimeConfig,
) -> Result<Arc<RuntimeHandle>, MobileLinuxApiErrorFfi> {
    #[cfg(target_os = "ios")]
    if matches!(config.platform, RuntimePlatform::Ios) {
        let workspace = config
            .workspace_host_path
            .as_ref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "iOS requires an explicit workspace_host_path".into(),
            })?;
        let stable_id = config
            .stable_workspace_id
            .as_ref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MobileLinuxApiErrorFfi::InvalidRequest {
                detail: "iOS requires an explicit stable_workspace_id".into(),
            })?;
        let runtime = mobile_linux_ios::linked_runtime(mobile_linux_ios::IosIshRuntimeConfig {
            managed_root: PathBuf::from(&config.managed_root),
            app_sandbox_root: PathBuf::from(&config.app_sandbox_root),
            workspace_host_path: PathBuf::from(workspace),
            stable_workspace_id: stable_id.clone(),
            abi: config.abi,
            rootfs_version: config.rootfs_version,
            archive_sha256: config.archive_sha256,
            authorization_file: config.authorization_file,
            rootfs_archive_path: config.rootfs_archive_path.map(PathBuf::from),
            rootfs_patch_path: config.rootfs_patch_path.map(PathBuf::from),
            default_mount_path: config.default_mount_path.map(PathBuf::from),
            protected_host_roots: config
                .protected_host_roots
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            allowed_mount_roots: config
                .allowed_mount_roots
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            allowed_guest_roots: config.allowed_guest_roots,
        })
        .map_err(mobile_linux_error_to_ffi)?;
        return Ok(Arc::new(RuntimeHandle { runtime }));
    }
    let (backend, name) = match config.platform {
        RuntimePlatform::Android => (platform_api::SandboxBackend::AndroidProot, "android"),
        RuntimePlatform::Ios => (platform_api::SandboxBackend::IosIsh, "ios"),
    };
    Ok(Arc::new(RuntimeHandle {
        runtime: Arc::new(platform_api::UnavailableMobileLinuxRuntime::unavailable(
            backend,
            platform_api::MobileLinuxRuntimeMode::MobileLinux,
            name,
            config.abi,
            "runtime execution requires a supported physical device",
        )),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_adapter_preserves_non_utf8_and_nul_bytes() {
        let bytes = vec![0, 255, 128, 10];
        let value = event_to_ffi(platform_api::MobileLinuxEvent {
            sequence: 9,
            task_id: Some("task".into()),
            kind: platform_api::MobileLinuxEventKind::PtyOutput {
                session_id: "pty".into(),
                data: bytes.clone(),
            },
        });
        assert_eq!(value.data, Some(bytes));
        assert_eq!(value.sequence, 9);
    }
    #[test]
    fn process_handle_keeps_enforcement_receipt() {
        let original = platform_api::LinuxProcessHandle {
            id: "task".into(),
            enforcement: platform_api::LinuxEnforcementReceipt {
                network_policy_enforced: true,
                memory_limit_enforced: true,
            },
        };
        assert_eq!(
            process_handle_to_traits(process_handle_to_ffi(original.clone())).unwrap(),
            original
        );
    }
    #[test]
    fn restart_required_is_not_a_generic_io_failure() {
        assert!(
            matches!(mobile_linux_error_to_ffi(platform_api::MobileLinuxError::RestartRequired("kernel".into())),MobileLinuxApiErrorFfi::RestartRequired {detail} if detail=="kernel")
        );
    }
}
