use mobile_linux_api as platform_api;
#[cfg(any(target_os = "android", target_os = "ios"))]
use std::path::PathBuf;

use crate::types::{
    MobileLinuxApiErrorFfi, MobileLinuxCapabilityFfi, MobileLinuxCommandRequestFfi,
    MobileLinuxCommandResultFfi, MobileLinuxEventFfi, MobileLinuxEventKindFfi,
    MobileLinuxMountPurposeFfi, MobileLinuxMountSpecFfi, MobileLinuxProcessHandleFfi,
    MobileLinuxPtyOpenRequestFfi, MobileLinuxPtySessionHandleFfi, MobileLinuxRootfsStateFfi,
    MobileLinuxRuntimeModeFfi, MobileLinuxStatusFfi, MobileLinuxTaskSnapshotFfi,
    MobileLinuxTaskStateFfi,
};

pub(crate) fn mobile_linux_backend_name(backend: platform_api::SandboxBackend) -> String {
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

pub(crate) fn rootfs_state_to_ffi(state: platform_api::RootfsState) -> MobileLinuxRootfsStateFfi {
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

pub(crate) fn capability_to_ffi(
    capability: platform_api::MobileLinuxCapability,
) -> MobileLinuxCapabilityFfi {
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

pub(crate) fn status_to_ffi(status: platform_api::RootfsStatus) -> MobileLinuxStatusFfi {
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

pub(crate) fn mount_purpose_to_traits(
    value: MobileLinuxMountPurposeFfi,
) -> platform_api::MountPurpose {
    match value {
        MobileLinuxMountPurposeFfi::Workspace => platform_api::MountPurpose::Workspace,
        MobileLinuxMountPurposeFfi::Memory => platform_api::MountPurpose::Memory,
        MobileLinuxMountPurposeFfi::Skills => platform_api::MountPurpose::Skills,
        MobileLinuxMountPurposeFfi::Shared => platform_api::MountPurpose::Shared,
        MobileLinuxMountPurposeFfi::External => platform_api::MountPurpose::External,
        MobileLinuxMountPurposeFfi::Temp => platform_api::MountPurpose::Temp,
    }
}

pub(crate) fn command_request_to_traits(
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

pub(crate) fn command_result_to_ffi(
    result: platform_api::LinuxCommandResult,
) -> MobileLinuxCommandResultFfi {
    MobileLinuxCommandResultFfi {
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        cancelled: result.cancelled,
        enforcement: result.enforcement.into(),
    }
}

pub(crate) fn task_status_to_ffi(
    status: platform_api::MobileLinuxTaskStatus,
) -> MobileLinuxTaskStateFfi {
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

pub(crate) fn task_snapshot_to_ffi(
    task: platform_api::MobileLinuxTaskSnapshot,
) -> MobileLinuxTaskSnapshotFfi {
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

pub(crate) fn event_to_ffi(event: platform_api::MobileLinuxEvent) -> MobileLinuxEventFfi {
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

pub(crate) fn mount_spec_to_traits(
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

pub(crate) fn pty_open_request_to_traits(
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

pub(crate) fn process_handle_to_ffi(
    handle: platform_api::LinuxProcessHandle,
) -> MobileLinuxProcessHandleFfi {
    MobileLinuxProcessHandleFfi {
        id: handle.id,
        enforcement: handle.enforcement.into(),
    }
}

pub(crate) fn process_handle_to_traits(
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

pub(crate) fn pty_handle_to_ffi(
    handle: platform_api::PtySessionHandle,
) -> MobileLinuxPtySessionHandleFfi {
    MobileLinuxPtySessionHandleFfi { id: handle.id }
}

pub(crate) fn pty_handle_to_traits(
    handle: MobileLinuxPtySessionHandleFfi,
) -> Result<platform_api::PtySessionHandle, MobileLinuxApiErrorFfi> {
    if handle.id.trim().is_empty() {
        return Err(MobileLinuxApiErrorFfi::InvalidRequest {
            detail: "pty handle id must not be empty".to_string(),
        });
    }
    Ok(platform_api::PtySessionHandle { id: handle.id })
}

pub(crate) fn mobile_linux_error_to_ffi(
    error: platform_api::MobileLinuxError,
) -> MobileLinuxApiErrorFfi {
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

pub(crate) fn raw_handle(
    id: String,
) -> Result<platform_api::RawStdioSessionHandle, MobileLinuxApiErrorFfi> {
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
