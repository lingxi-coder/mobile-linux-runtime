use mobile_linux_api as platform_api;
#[cfg(any(target_os = "android", target_os = "ios"))]
use std::path::PathBuf;

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
