use mobile_linux_api::mobile_linux::LinuxEnforcementReceipt;
#[cfg(test)]
use mobile_linux_api::LinuxCommandResult;
use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MobileLinuxTaskStatus, MountPurpose, MountSpec,
    PtyOpenRequest, RawStdioOpenRequest, RawStdioReadResult,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub(super) struct NativeConfigPayload {
    pub(super) managed_root: String,
    pub(super) workspace_host_path: String,
    pub(super) stable_workspace_id: String,
    pub(super) abi: String,
    pub(super) rootfs_version: String,
    pub(super) archive_sha256: Option<String>,
    pub(super) authorization_file: Option<String>,
    pub(super) rootfs_archive_path: Option<String>,
    pub(super) default_mount_path: Option<String>,
    pub(super) rootfs_patch_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct MountConfigPayload {
    pub(super) mounts: Vec<MountPayload>,
}

impl MountConfigPayload {
    pub(super) fn from_mounts(mounts: &[MountSpec]) -> Self {
        Self {
            mounts: mounts.iter().map(MountPayload::from_mount).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct RunRequestPayload {
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: Option<String>,
    pub(super) env: BTreeMap<String, String>,
    pub(super) stdin: Option<String>,
    pub(super) timeout_ms: Option<u64>,
    pub(super) network: &'static str,
    pub(super) resource_limits: mobile_linux_api::ResourceLimits,
    pub(super) mounts: Vec<MountPayload>,
    pub(super) include_default_mounts: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct RawStdioOpenPayload {
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: Option<String>,
    pub(super) env: BTreeMap<String, String>,
    pub(super) network: &'static str,
    pub(super) resource_limits: mobile_linux_api::ResourceLimits,
    pub(super) mounts: Vec<MountPayload>,
}

impl RawStdioOpenPayload {
    pub(super) fn from_request(request: &RawStdioOpenRequest, mounts: &[MountSpec]) -> Self {
        Self {
            command: request.command.clone(),
            args: request.args.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            network: match request.network {
                mobile_linux_api::NetworkPolicy::Disabled => "disabled",
                mobile_linux_api::NetworkPolicy::LoopbackOnly => "loopback-only",
                mobile_linux_api::NetworkPolicy::Allowed => "allowed",
            },
            resource_limits: request.resource_limits,
            mounts: mounts.iter().map(MountPayload::from_mount).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct RawStdioRequestPayload {
    pub(super) session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) data_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_bytes: Option<u32>,
}

#[derive(Clone, Copy)]
pub(super) enum RawStdioNativeOperation {
    Write,
    Read,
    Close,
    Dispose,
}

impl RunRequestPayload {
    pub(super) fn from_request(
        request: &LinuxCommandRequest,
        mounts: &[MountSpec],
        include_default_mounts: bool,
    ) -> Self {
        Self {
            command: request.command.clone(),
            args: request.args.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            stdin: request.stdin.clone(),
            timeout_ms: request.timeout_ms,
            network: match request.network {
                mobile_linux_api::NetworkPolicy::Disabled => "disabled",
                mobile_linux_api::NetworkPolicy::LoopbackOnly => "loopback-only",
                mobile_linux_api::NetworkPolicy::Allowed => "allowed",
            },
            resource_limits: request.resource_limits,
            mounts: mounts.iter().map(MountPayload::from_mount).collect(),
            include_default_mounts,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct PtyOpenPayload {
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: Option<String>,
    pub(super) env: BTreeMap<String, String>,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) mounts: Vec<MountPayload>,
}

impl PtyOpenPayload {
    pub(super) fn from_request(request: &PtyOpenRequest, mounts: &[MountSpec]) -> Self {
        Self {
            command: request.command.clone(),
            args: request.args.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            cols: request.size.cols,
            rows: request.size.rows,
            mounts: mounts.iter().map(MountPayload::from_mount).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct PtyWritePayload {
    pub(super) session_id: String,
    pub(super) data_base64: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PtyResizePayload {
    pub(super) session_id: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Serialize)]
pub(super) struct PtyClosePayload {
    pub(super) session_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PtyPollPayload {
    pub(super) after_sequence: Option<u64>,
    pub(super) limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(super) struct BackgroundProcessPayload {
    pub(super) process_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct BackgroundPollPayload {
    pub(super) process_id: String,
    pub(super) after_sequence: Option<u64>,
    pub(super) limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(super) struct LoopbackProbePayload {
    pub(super) port: u16,
    pub(super) timeout_ms: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct MountPayload {
    pub(super) host_path: String,
    pub(super) guest_path: String,
    pub(super) read_only: bool,
    pub(super) purpose: &'static str,
}

impl MountPayload {
    pub(super) fn from_mount(mount: &MountSpec) -> Self {
        Self {
            host_path: mount.host_path.display().to_string(),
            guest_path: mount.guest_path.clone(),
            read_only: mount.read_only,
            purpose: match mount.purpose {
                MountPurpose::Workspace => "workspace",
                MountPurpose::LocalAppBuild => "local_app_build",
                MountPurpose::Memory => "memory",
                MountPurpose::Skills => "skills",
                MountPurpose::Shared => "shared",
                MountPurpose::External => "external",
                MountPurpose::Temp => "temp",
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct RunResponsePayload {
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) exit_code: i32,
    #[serde(default)]
    pub(super) timed_out: bool,
    #[serde(default)]
    pub(super) cancelled: bool,
    #[serde(default)]
    pub(super) network_policy_enforced: bool,
    #[serde(default)]
    pub(super) memory_limit_enforced: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NativeErrorPayload {
    pub(super) code: String,
    pub(super) message: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
pub(super) struct NativeRunEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) result: Option<RunResponsePayload>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeAvailabilityEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) available: Option<bool>,
    pub(super) kernel_reason: Option<String>,
    pub(super) shell_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeOkEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeSessionEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeRawStdioOpenEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) session_id: Option<String>,
    #[serde(default)]
    pub(super) network_policy_enforced: bool,
    #[serde(default)]
    pub(super) memory_limit_enforced: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeRawStdioReadEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) stdout_base64: Option<String>,
    pub(super) stderr_base64: Option<String>,
    #[serde(default)]
    pub(super) closed: bool,
    pub(super) exit_code: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeProcessEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) process_id: Option<String>,
    #[serde(default)]
    pub(super) network_policy_enforced: bool,
    #[serde(default)]
    pub(super) memory_limit_enforced: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct NativeProcessStart {
    pub(super) process_id: String,
    pub(super) enforcement: LinuxEnforcementReceipt,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeBackgroundKillEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) termination_requested: Option<bool>,
    pub(super) already_stopped: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NativePtyEventPayload {
    pub(super) sequence: u64,
    pub(super) session_id: String,
    pub(super) kind: String,
    pub(super) data_base64: Option<String>,
    pub(super) detail: Option<String>,
    /// Real guest exit code on a shell self-exit (`pty_closed` emitted by the
    /// bridge's ISHProcessExited observer). Absent for bridge-initiated
    /// closes, whose contract stays "closed cleanly" (code 0).
    #[serde(default)]
    pub(super) exit_code: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativePollEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    #[serde(default)]
    pub(super) events: Vec<NativePtyEventPayload>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NativeBackgroundEventPayload {
    pub(super) sequence: u64,
    pub(super) kind: String,
    pub(super) line: Option<String>,
    pub(super) data_base64: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) cancelled: Option<bool>,
    pub(super) detail: Option<String>,
    #[serde(default)]
    pub(super) result: Option<RunResponsePayload>,
    #[serde(default)]
    pub(super) error: Option<NativeErrorPayload>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeBackgroundPollEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    #[serde(default)]
    pub(super) events: Vec<NativeBackgroundEventPayload>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeLoopbackProbeEnvelope {
    pub(super) ok: bool,
    pub(super) error: Option<NativeErrorPayload>,
    pub(super) reachable: Option<bool>,
}

pub(super) fn background_terminal_state(
    cancel_requested: bool,
    event: &NativeBackgroundEventPayload,
) -> (MobileLinuxTaskStatus, Option<i32>, Option<String>) {
    let cancelled = cancel_requested || event.cancelled.unwrap_or(false);
    let status = if event.error.is_some() {
        MobileLinuxTaskStatus::Failed
    } else if event.result.as_ref().is_some_and(|result| result.timed_out) {
        MobileLinuxTaskStatus::TimedOut
    } else if cancelled {
        MobileLinuxTaskStatus::Cancelled
    } else if event.exit_code == Some(0) {
        MobileLinuxTaskStatus::Completed
    } else {
        MobileLinuxTaskStatus::Failed
    };
    let detail = event
        .detail
        .clone()
        .or_else(|| cancelled.then(|| "task cancelled".to_string()));
    (status, event.exit_code, detail)
}

#[cfg(test)]
pub(super) fn parse_run_response(json: &str) -> Result<LinuxCommandResult, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeRunEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse run_json response: {error}")))?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native run_json response indicated failure".to_string(),
            },
        )));
    }
    let payload = envelope.result.ok_or_else(|| {
        MobileLinuxError::Io("native run_json response omitted result".to_string())
    })?;
    Ok(LinuxCommandResult {
        stdout: payload.stdout,
        stderr: payload.stderr,
        exit_code: payload.exit_code,
        timed_out: payload.timed_out,
        cancelled: payload.cancelled,
        enforcement: LinuxEnforcementReceipt {
            network_policy_enforced: payload.network_policy_enforced,
            memory_limit_enforced: payload.memory_limit_enforced,
        },
    })
}

pub(super) fn parse_native_ok(json: &str) -> Result<(), MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeOkEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse native response: {error}")))?;
    if envelope.ok {
        Ok(())
    } else {
        Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native response indicated failure".to_string(),
            },
        )))
    }
}

pub(super) fn parse_session_id_response(json: &str) -> Result<String, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeSessionEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse PTY response: {error}")))?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native PTY response indicated failure".to_string(),
            },
        )));
    }
    envelope
        .session_id
        .ok_or_else(|| MobileLinuxError::Io("native PTY response omitted session_id".to_string()))
}

pub(super) fn parse_raw_stdio_open_response(
    json: &str,
) -> Result<(String, LinuxEnforcementReceipt), MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeRawStdioOpenEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse raw stdio open: {error}")))?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native raw stdio open indicated failure".to_string(),
            },
        )));
    }
    let session_id = envelope.session_id.ok_or_else(|| {
        MobileLinuxError::Io("native raw stdio open omitted session_id".to_string())
    })?;
    Ok((
        session_id,
        LinuxEnforcementReceipt {
            network_policy_enforced: envelope.network_policy_enforced,
            memory_limit_enforced: envelope.memory_limit_enforced,
        },
    ))
}

pub(super) fn raw_stdio_close_progress(
    json: &str,
) -> Result<(bool, Option<MobileLinuxError>), MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeRawStdioReadEnvelope>(json).map_err(|error| {
        MobileLinuxError::Io(format!("parse raw stdio close progress: {error}"))
    })?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".into(),
                message: "native raw stdio read indicated failure".into(),
            },
        )));
    }
    Ok((envelope.closed, envelope.error.map(native_error_to_mobile)))
}

pub(super) fn parse_raw_stdio_read_response(
    json: &str,
) -> Result<RawStdioReadResult, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeRawStdioReadEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse raw stdio read: {error}")))?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native raw stdio read indicated failure".to_string(),
            },
        )));
    }
    if let Some(error) = envelope.error {
        return Err(native_error_to_mobile(error));
    }
    Ok(RawStdioReadResult {
        stdout: envelope
            .stdout_base64
            .as_deref()
            .map(decode_base64)
            .transpose()?
            .unwrap_or_default(),
        stderr: envelope
            .stderr_base64
            .as_deref()
            .map(decode_base64)
            .transpose()?
            .unwrap_or_default(),
        closed: envelope.closed,
        exit_code: envelope.exit_code,
    })
}

pub(super) fn parse_process_id_response(
    json: &str,
) -> Result<NativeProcessStart, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeProcessEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse background response: {error}")))?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native background response indicated failure".to_string(),
            },
        )));
    }
    let process_id = envelope.process_id.ok_or_else(|| {
        MobileLinuxError::Io("native background response omitted process_id".to_string())
    })?;
    Ok(NativeProcessStart {
        process_id,
        enforcement: LinuxEnforcementReceipt {
            network_policy_enforced: envelope.network_policy_enforced,
            memory_limit_enforced: envelope.memory_limit_enforced,
        },
    })
}

pub(super) fn parse_background_kill_response(json: &str) -> Result<bool, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeBackgroundKillEnvelope>(json).map_err(|error| {
        MobileLinuxError::Io(format!("parse background kill response: {error}"))
    })?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native background kill response indicated failure".to_string(),
            },
        )));
    }
    match (envelope.termination_requested, envelope.already_stopped) {
        (Some(requested), _) => Ok(requested),
        (None, Some(true)) => Ok(false),
        _ => Err(MobileLinuxError::Io(
            "native background kill response omitted termination state".to_string(),
        )),
    }
}

pub(super) fn parse_poll_events(
    json: &str,
) -> Result<Vec<NativePtyEventPayload>, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativePollEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse PTY poll response: {error}")))?;
    if envelope.ok {
        Ok(envelope.events)
    } else {
        Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native PTY poll indicated failure".to_string(),
            },
        )))
    }
}

pub(super) fn parse_background_events(
    json: &str,
) -> Result<Vec<NativeBackgroundEventPayload>, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeBackgroundPollEnvelope>(json).map_err(|error| {
        MobileLinuxError::Io(format!("parse background poll response: {error}"))
    })?;
    if envelope.ok {
        Ok(envelope.events)
    } else {
        Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native background poll indicated failure".to_string(),
            },
        )))
    }
}

pub(super) fn parse_loopback_probe(json: &str) -> Result<bool, MobileLinuxError> {
    let envelope = serde_json::from_str::<NativeLoopbackProbeEnvelope>(json)
        .map_err(|error| MobileLinuxError::Io(format!("parse loopback probe response: {error}")))?;
    if !envelope.ok {
        return Err(native_error_to_mobile(envelope.error.unwrap_or(
            NativeErrorPayload {
                code: "io".to_string(),
                message: "native loopback probe indicated failure".to_string(),
            },
        )));
    }
    envelope
        .reachable
        .ok_or_else(|| MobileLinuxError::Io("native loopback probe omitted reachable".to_string()))
}

pub(super) fn parse_availability_reason(json: &str) -> Option<String> {
    let envelope = serde_json::from_str::<NativeAvailabilityEnvelope>(json).ok()?;
    if envelope.ok && envelope.available.unwrap_or(false) {
        return None;
    }
    if let Some(error) = envelope.error {
        return Some(error.message);
    }
    let mut reasons = Vec::new();
    if let Some(reason) = envelope.kernel_reason.filter(|reason| !reason.is_empty()) {
        reasons.push(format!("kernel: {reason}"));
    }
    if let Some(reason) = envelope.shell_reason.filter(|reason| !reason.is_empty()) {
        reasons.push(format!("shell: {reason}"));
    }
    if reasons.is_empty() {
        Some("native iSH runtime reports unavailable".to_string())
    } else {
        Some(reasons.join("; "))
    }
}

pub(super) fn native_error_to_mobile(error: NativeErrorPayload) -> MobileLinuxError {
    match error.code.as_str() {
        "invalid_request" => MobileLinuxError::InvalidRequest(error.message),
        "restart_required" => MobileLinuxError::RestartRequired(error.message),
        "unavailable" => MobileLinuxError::Unavailable(error.message),
        "network_policy_unavailable" => MobileLinuxError::NetworkPolicyUnavailable(error.message),
        "resource_limit_exceeded" => MobileLinuxError::ResourceLimitExceeded(error.message),
        "io" => MobileLinuxError::Io(error.message),
        _ => MobileLinuxError::Io(error.message),
    }
}

pub(super) fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(b0 >> 2) as usize] as char);
        encoded.push(TABLE[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[(((b1 & 0b1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

pub(super) fn decode_base64(value: &str) -> Result<Vec<u8>, MobileLinuxError> {
    fn sextet(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(MobileLinuxError::Io(
            "native PTY payload is not valid base64".to_string(),
        ));
    }
    let mut decoded = Vec::with_capacity((bytes.len() / 4) * 3);
    for chunk in bytes.chunks(4) {
        let s0 = sextet(chunk[0]).ok_or_else(|| {
            MobileLinuxError::Io("native PTY payload is not valid base64".to_string())
        })?;
        let s1 = sextet(chunk[1]).ok_or_else(|| {
            MobileLinuxError::Io("native PTY payload is not valid base64".to_string())
        })?;
        let s2 = if chunk[2] == b'=' {
            None
        } else {
            Some(sextet(chunk[2]).ok_or_else(|| {
                MobileLinuxError::Io("native PTY payload is not valid base64".to_string())
            })?)
        };
        let s3 = if chunk[3] == b'=' {
            None
        } else {
            Some(sextet(chunk[3]).ok_or_else(|| {
                MobileLinuxError::Io("native PTY payload is not valid base64".to_string())
            })?)
        };
        decoded.push((s0 << 2) | (s1 >> 4));
        if let Some(s2) = s2 {
            decoded.push(((s1 & 0b1111) << 4) | (s2 >> 2));
            if let Some(s3) = s3 {
                decoded.push(((s2 & 0b11) << 6) | s3);
            }
        }
    }
    Ok(decoded)
}
