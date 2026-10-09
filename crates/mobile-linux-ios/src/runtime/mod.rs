use mobile_linux_api::{
    MobileLinuxError, MobileLinuxEvent, MobileLinuxEventKind, MobileLinuxRuntime,
    MobileLinuxTaskSnapshot, MobileLinuxTaskStatus, MountSpec, RawStdioSessionHandle, RootfsStatus,
};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bridge::{
    background_terminal_state, decode_base64, encode_base64, native_error_to_mobile,
    parse_availability_reason, parse_background_events, parse_background_kill_response,
    parse_loopback_probe, parse_native_ok, parse_poll_events, parse_process_id_response,
    parse_raw_stdio_open_response, parse_raw_stdio_read_response, parse_session_id_response,
    raw_stdio_close_progress, BackgroundPollPayload, BackgroundProcessPayload,
    LoopbackProbePayload, MountConfigPayload, NativeBackgroundEventPayload, NativeConfigPayload,
    NativeProcessStart, NativePtyEventPayload, PtyClosePayload, PtyOpenPayload, PtyPollPayload,
    PtyResizePayload, PtyWritePayload, RawStdioNativeOperation, RawStdioOpenPayload,
    RawStdioRequestPayload, RunRequestPayload,
};
pub use config::IosIshRuntimeConfig;
use rootfs::filesystem_rootfs_state;
use streaming::cap_capture;
use validation::{
    normalize_host_path, validate_guest_path, validate_mount, validate_pty_request,
    validate_request,
};
mod background;
mod bridge;
mod config;
mod execution;
mod implementation;
mod lifecycle;
mod mounts;
mod native;
mod pty;
mod raw_stdio;
mod rootfs;
mod streaming;
mod validation;

const MAX_EVENTS: usize = 4096;

const MAX_STREAM_CAPTURE_BYTES: usize = 256 * 1024;

const PTY_IDLE_POLL: Duration = Duration::from_millis(25);

const BACKGROUND_IDLE_POLL: Duration = Duration::from_millis(25);

const BACKGROUND_REAP_BUDGET: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct TaskControl {
    snapshot: Mutex<MobileLinuxTaskSnapshot>,
    native_handle: Mutex<Option<String>>,
    cancel_requested: AtomicBool,
    cancel_notify: tokio::sync::Notify,
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
            native_handle: Mutex::new(None),
            cancel_requested: AtomicBool::new(false),
            cancel_notify: tokio::sync::Notify::new(),
            terminal_emitted: AtomicBool::new(false),
        }
    }
}

/// Cancellation follows the Rust/UniFFI future even while native startup is
/// pending. The independent owner survives Drop to publish, kill and drain it.
struct ForegroundCancellation {
    task: Arc<TaskControl>,
    armed: bool,
}

impl Drop for ForegroundCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.task.cancel_requested.store(true, Ordering::Release);
            self.task.cancel_notify.notify_one();
        }
    }
}

async fn wait_task_cancel(task: &TaskControl) {
    while !task.cancel_requested.load(Ordering::Acquire) {
        task.cancel_notify.notified().await;
    }
}

async fn wait_process_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

#[derive(Debug)]
struct PtyControl {
    task: Arc<TaskControl>,
    open: AtomicBool,
    close_emitted: AtomicBool,
}

#[derive(Debug)]
struct RuntimeState {
    config: IosIshRuntimeConfig,
    mounts: RwLock<Vec<MountSpec>>,
    tasks: Mutex<HashMap<String, Arc<TaskControl>>>,
    events: Mutex<VecDeque<MobileLinuxEvent>>,
    next_id: AtomicU64,
    next_sequence: AtomicU64,
    booted: AtomicBool,
    rootfs_status_cache: Mutex<Option<RootfsStatus>>,
    kernel_started: Arc<AtomicBool>,
    closed: AtomicBool,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    pty: Mutex<Option<(String, Arc<PtyControl>)>>,
    raw_stdio: Mutex<HashMap<String, RawStdioSessionHandle>>,
    native_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    test_transport: Option<Arc<tests::TestProcessTransport>>,
}

/// Concrete mobile-linux runtime backed by the native iSH bridge on device and
/// by a safe unavailable stub elsewhere.
#[derive(Clone, Debug)]
pub struct IosIshRuntime {
    state: Arc<RuntimeState>,
}

#[derive(Default)]
struct ProcessRegistry {
    identity: Option<IosIshRuntimeConfig>,
    sessions: Vec<Weak<RuntimeState>>,
    native_lock: Arc<Mutex<()>>,
    kernel_started: Arc<AtomicBool>,
}

impl IosIshRuntime {
    fn next_id(&self, prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            self.state.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn emit(&self, task_id: Option<String>, kind: MobileLinuxEventKind) {
        let mut events = self.state.events.lock().expect("ios-ish events mutex");
        events.push_back(MobileLinuxEvent {
            sequence: self.state.next_sequence.fetch_add(1, Ordering::Relaxed),
            task_id,
            kind,
        });
        while events.len() > MAX_EVENTS {
            events.pop_front();
        }
    }

    fn emit_runtime_error(&self, task_id: Option<String>, detail: impl Into<String>) {
        self.emit(
            task_id,
            MobileLinuxEventKind::RuntimeError {
                detail: detail.into(),
            },
        );
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
            .expect("ios-ish tasks mutex")
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

    fn create_task_with_id(
        &self,
        id: String,
        command: String,
        status: MobileLinuxTaskStatus,
    ) -> Arc<TaskControl> {
        let task = Arc::new(TaskControl::new(id.clone(), command, status));
        self.state
            .tasks
            .lock()
            .expect("ios-ish tasks mutex")
            .insert(id.clone(), task.clone());
        self.emit(
            Some(id),
            MobileLinuxEventKind::TaskStatusChanged {
                status,
                exit_code: None,
                detail: None,
            },
        );
        task
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
        let mut snapshot = task.snapshot.lock().expect("ios-ish task snapshot mutex");
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

/// Build the shared trait object wired to this crate's iSH runtime backend.
#[must_use]
pub fn linked_runtime(
    config: IosIshRuntimeConfig,
) -> Result<Arc<dyn MobileLinuxRuntime>, MobileLinuxError> {
    Ok(Arc::new(IosIshRuntime::new(config)?) as Arc<dyn MobileLinuxRuntime>)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn display_command(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

#[cfg(test)]
mod tests;
