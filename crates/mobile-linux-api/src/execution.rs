//! Portable execution vocabulary shared by runtime backends and host adapters.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Consumer for a command whose output must be observed while it is running.
///
/// The process runner invokes stdout one complete logical line at a time and
/// stderr in bounded byte chunks. Implementations should apply bounded
/// backpressure; returning an error aborts the command and lets the platform
/// runner tear down its process tree.
#[async_trait]
pub trait ProcessStreamSink: Send + Sync {
    /// Consume one stdout line, without its trailing line ending.
    async fn stdout_line(&self, line: String) -> Result<(), ProcessError>;

    /// Consume a bounded stderr chunk. Stderr is not an event stream, but must
    /// be drained concurrently so a noisy child cannot deadlock on a full pipe.
    async fn stderr_chunk(&self, chunk: Vec<u8>) -> Result<(), ProcessError>;
}

/// Collected output of a completed process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessOutput {
    /// Captured stdout (UTF-8 lossy decoded).
    pub stdout: String,
    /// Captured stderr (UTF-8 lossy decoded).
    pub stderr: String,
    /// Exit code (use `-1` to indicate "signalled" if the platform cannot
    /// report a real status).
    pub exit_code: i32,
    /// True when the runner had to kill the process for exceeding its
    /// timeout.
    pub timed_out: bool,
}

/// Failure modes shared by every `ProcessRunner` method.
#[derive(Debug, Clone, Error)]
pub enum ProcessError {
    /// Platform does not implement process execution.
    #[error("unsupported on this platform")]
    Unsupported,
    /// A requested policy guarantee cannot be enforced on this platform.
    /// The payload names the guarantee (spec: "errors must name the
    /// unenforceable guarantee").
    #[error("policy unsupported: {0}")]
    PolicyUnsupported(String),
    /// The runner received a `SandboxedCommand` whose backend plan is
    /// missing, malformed, or minted for a different backend.
    #[error("malformed sandbox plan: {0}")]
    MalformedSandboxPlan(String),
    /// Jail setup failed at runtime (after prepare admitted the command).
    #[error("sandbox enforcement failed: {0}")]
    SandboxEnforcementFailed(String),
    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(String),
    /// Timeout fired before the command completed.
    #[error("timeout")]
    Timeout,
}

/// Concrete sandbox implementation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxBackend {
    /// Linux user / mount / network namespaces.
    LinuxNamespaces,
    /// Firejail-based wrapper on Linux.
    LinuxFirejail,
    /// macOS `sandbox-exec` profile.
    MacOsSandboxExec,
    /// Windows Job Object + restricted token.
    WindowsJobObject,
    /// Android in-engine Minijail (`no_new_privs` / rlimits / seccomp via
    /// libminijail linked into the engine .so). Spec r3 D6.
    AndroidMinijail,
    /// Android Linux userspace runtime backed by PRoot, still wrapped by the
    /// outer Minijail boundary.
    AndroidProot,
    /// iOS Linux userspace runtime backed by iSH userspace emulation.
    IosIsh,
    /// No sandbox enforcement (used for explicit bypass).
    None,
}

/// Coarse network isolation level for a `SandboxPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPolicy {
    /// No outbound network at all.
    Disabled,
    /// Loopback (127.0.0.0/8, `::1`) only.
    LoopbackOnly,
    /// Full outbound network access.
    Allowed,
}

/// Resource ceilings for a sandboxed process.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum CPU time in seconds.
    pub max_cpu_seconds: Option<u32>,
    /// Maximum resident memory in megabytes.
    pub max_memory_mb: Option<u32>,
    /// Maximum number of child processes / threads.
    pub max_processes: Option<u32>,
    /// Maximum number of open file descriptors.
    pub max_open_files: Option<u32>,
}
