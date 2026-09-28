//! Host-independent contracts for portable mobile Linux runtimes.
#![forbid(unsafe_code)]

mod execution;
mod mobile_linux;

pub use execution::{
    NetworkPolicy, ProcessError, ProcessOutput, ProcessStreamSink, ResourceLimits, SandboxBackend,
};
pub use mobile_linux::{
    find_guest_mount, guest_paths, map_guest_path_to_host, map_host_path_to_guest,
    LinuxCommandRequest, LinuxCommandResult, LinuxEnforcementReceipt, LinuxProcessHandle,
    MobileLinuxCapability, MobileLinuxError, MobileLinuxEvent, MobileLinuxEventKind,
    MobileLinuxRuntime, MobileLinuxRuntimeMode, MobileLinuxSandboxPlan, MobileLinuxTaskSnapshot,
    MobileLinuxTaskStatus, MountPurpose, MountSpec, PtyOpenRequest, PtySessionHandle, PtySize,
    RawStdioOpenRequest, RawStdioReadResult, RawStdioSessionHandle, RootfsState, RootfsStatus,
    UnavailableMobileLinuxRuntime, MAX_MOBILE_LINUX_EVENT_BATCH,
};
