use super::bridge::{
    background_terminal_state, decode_base64, parse_background_events,
    parse_background_kill_response, parse_loopback_probe, parse_native_ok, parse_poll_events,
    parse_process_id_response, parse_raw_stdio_open_response, parse_raw_stdio_read_response,
    parse_run_response, raw_stdio_close_progress, NativeBackgroundEventPayload, NativeErrorPayload,
    NativeProcessStart, RawStdioNativeOperation, RunRequestPayload,
};
use super::config::IosIshRuntimeConfig;
use super::rootfs::filesystem_rootfs_state;
use super::streaming::{append_capped_bytes, append_capped_stdout, cap_capture};
use super::validation::{
    normalize_host_path, validate_mount, validate_pty_request, validate_request,
};
use async_trait::async_trait;
use mobile_linux_api::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, LinuxProcessHandle, MobileLinuxError, MobileLinuxEventKind,
    MobileLinuxRuntime, MobileLinuxTaskSnapshot, MobileLinuxTaskStatus, MountPurpose, MountSpec,
    ProcessStreamSink, PtyOpenRequest, PtySize, RawStdioOpenRequest, RawStdioSessionHandle,
    RootfsState,
};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use mobile_linux_api::NetworkPolicy;

/// Test-only native process transport. This exercises Rust ownership and
/// codec behavior; it does not make the host report an available iSH kernel.
#[derive(Debug, Default)]
pub(super) struct TestProcessTransport {
    spawn_entered: AtomicBool,
    block_spawn: AtomicBool,
    release_spawn: tokio::sync::Notify,
    alive: AtomicBool,
    kills: AtomicU64,
    already_stopped: AtomicBool,
    events: Mutex<VecDeque<NativeBackgroundEventPayload>>,
    request: Mutex<Option<serde_json::Value>>,
    raw_calls: Mutex<Vec<&'static str>>,
}

impl TestProcessTransport {
    pub(super) async fn spawn(
        &self,
        request: &LinuxCommandRequest,
        mounts: &[MountSpec],
        defaults: bool,
    ) -> Result<NativeProcessStart, MobileLinuxError> {
        *self.request.lock().unwrap() = Some(
            serde_json::to_value(RunRequestPayload::from_request(request, mounts, defaults))
                .unwrap(),
        );
        self.spawn_entered.store(true, Ordering::Release);
        if self.block_spawn.load(Ordering::Acquire) {
            self.release_spawn.notified().await;
        }
        self.alive.store(true, Ordering::Release);
        Ok(NativeProcessStart {
            process_id: "test-process".into(),
            enforcement: Default::default(),
        })
    }

    pub(super) fn kill(&self, process_id: &str) -> Result<bool, MobileLinuxError> {
        assert_eq!(process_id, "test-process");
        if self.already_stopped.load(Ordering::Acquire) {
            self.alive.store(false, Ordering::Release);
            return Ok(false);
        }
        if self.kills.fetch_add(1, Ordering::AcqRel) == 0 {
            self.events.lock().unwrap().extend(Self::completion(true));
        }
        Ok(self.alive.load(Ordering::Acquire))
    }

    pub(super) fn poll(
        &self,
        process_id: &str,
        _: Option<u64>,
    ) -> Result<Vec<NativeBackgroundEventPayload>, MobileLinuxError> {
        assert_eq!(process_id, "test-process");
        // Deliberately expose the terminal event only after a separate
        // output poll, proving that kill alone is not considered reaping.
        let result: Vec<_> = self
            .events
            .lock()
            .unwrap()
            .pop_front()
            .into_iter()
            .collect();
        if result.iter().any(|event| event.kind == "process_exited") {
            self.alive.store(false, Ordering::Release);
        }
        Ok(result)
    }

    fn completion(cancelled: bool) -> Vec<NativeBackgroundEventPayload> {
        parse_background_events(
            &serde_json::json!({
                "ok": true,
                "events": [
                    {"sequence":1,"kind":"stdout_line","line":"final tail"},
                    {"sequence":2,"kind":"stderr_chunk","data_base64":"d2FybgA="},
                    {"sequence":3,"kind":"process_exited","exit_code":0,"cancelled":cancelled,
                     "result":{"stdout":"exact\u{0000}tail","stderr":"warn\u{0000}","exit_code":0,
                       "timed_out":false,"cancelled":cancelled,"network_policy_enforced":false,
                       "memory_limit_enforced":false}}
                ]
            })
            .to_string(),
        )
        .unwrap()
    }

    pub(super) fn raw_call(
        &self,
        operation: RawStdioNativeOperation,
    ) -> Result<String, MobileLinuxError> {
        let mut calls = self.raw_calls.lock().unwrap();
        let name = match operation {
            RawStdioNativeOperation::Write => "write",
            RawStdioNativeOperation::Read => "read",
            RawStdioNativeOperation::Close => "close",
            RawStdioNativeOperation::Dispose => "dispose",
        };
        calls.push(name);
        Ok(if name == "read" {
            // A failure is reported at the final empty read; disposal must
            // still happen before the typed execution error is returned.
            r#"{"ok":true,"closed":true,"exit_code":137,"error":{"code":"resource_limit_exceeded","message":"test resource limit"}}"#.into()
        } else {
            r#"{"ok":true}"#.into()
        })
    }
}

fn process_test_runtime(temp: &tempfile::TempDir) -> (IosIshRuntime, Arc<TestProcessTransport>) {
    let mut runtime = IosIshRuntime::new_state(test_config(temp.path()));
    fs::create_dir_all(&runtime.state.config.workspace_host_path).unwrap();
    let transport = Arc::new(TestProcessTransport::default());
    let state = Arc::get_mut(&mut runtime.state).unwrap();
    state.test_transport = Some(transport.clone());
    state.booted.store(true, Ordering::Release);
    (runtime, transport)
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("bounded process lifecycle")
}

async fn wait_started(transport: &TestProcessTransport) {
    bounded(async {
        while !transport.spawn_entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn wait_terminal(runtime: &IosIshRuntime) -> MobileLinuxTaskSnapshot {
    bounded(async {
        loop {
            if let Some(task) = runtime
                .list_tasks()
                .await
                .unwrap()
                .into_iter()
                .find(|task| task.finished_at_ms.is_some())
            {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
}

#[tokio::test]
async fn idempotent_boot_reuses_status_until_an_explicit_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, _) = process_test_runtime(&temp);
    let initial = runtime.boot().await.unwrap();
    assert_eq!(initial.installed_size_bytes, Some(0));
    let root = runtime.state.config.active_root();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("new-rootfs-file"), b"changed").unwrap();
    // A repeated boot is the hot path before every command/raw/PTY start.
    assert_eq!(runtime.boot().await.unwrap().installed_size_bytes, Some(0));
    assert_eq!(
        runtime.rootfs_status().await.unwrap().installed_size_bytes,
        Some(7)
    );
    assert_eq!(runtime.boot().await.unwrap().installed_size_bytes, Some(7));
    runtime.invalidate_rootfs_snapshot();
    fs::write(root.join("new-rootfs-file"), b"replacement").unwrap();
    assert_eq!(runtime.boot().await.unwrap().installed_size_bytes, Some(11));
}

#[tokio::test]
async fn foreground_drop_during_native_startup_kills_and_drains_owned_process() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    transport.block_spawn.store(true, Ordering::Release);
    let run_runtime = runtime.clone();
    let caller = tokio::spawn(async move { run_runtime.run(command_request()).await });
    wait_started(&transport).await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    transport.release_spawn.notify_one();
    let terminal = wait_terminal(&runtime).await;
    assert_eq!(terminal.status, MobileLinuxTaskStatus::Cancelled);
    assert!(!transport.alive.load(Ordering::Acquire));
    assert!(transport.kills.load(Ordering::Acquire) >= 1);
    let task = runtime.state.tasks.lock().unwrap()[&terminal.task_id].clone();
    assert_eq!(
        task.native_handle.lock().unwrap().as_deref(),
        Some("test-process")
    );
    let events = runtime.read_events(None, 100).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.kind,
                MobileLinuxEventKind::TaskStatusChanged {
                    status: MobileLinuxTaskStatus::Cancelled,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(&event.kind, MobileLinuxEventKind::StdoutLine {line} if line == "final tail")));
}

#[tokio::test]
async fn shutdown_waits_for_native_handle_then_reaps_foreground() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    transport.block_spawn.store(true, Ordering::Release);
    let run_runtime = runtime.clone();
    let caller = tokio::spawn(async move { run_runtime.run(command_request()).await });
    wait_started(&transport).await;
    let close_runtime = runtime.clone();
    let shutdown = tokio::spawn(async move { close_runtime.shutdown().await });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must not overlook pending native startup"
    );
    transport.release_spawn.notify_one();
    bounded(shutdown).await.unwrap().unwrap();
    assert!(bounded(caller).await.unwrap().unwrap().cancelled);
    assert!(!transport.alive.load(Ordering::Acquire));
    assert!(runtime.ensure_session_open().is_err());
}

#[tokio::test]
async fn foreground_timeout_kills_then_drains_before_reporting_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    let mut request = command_request();
    request.timeout_ms = Some(5);
    let result = bounded(runtime.run(request)).await.unwrap();
    assert!(result.timed_out);
    assert!(!result.cancelled);
    assert_eq!(result.stdout, "exact\0tail");
    assert_eq!(result.stderr, "warn\0");
    assert!(!transport.alive.load(Ordering::Acquire));
    assert_eq!(
        wait_terminal(&runtime).await.status,
        MobileLinuxTaskStatus::TimedOut
    );
}

#[tokio::test]
async fn isolated_foreground_preserves_mount_scope_and_exact_terminal_output() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    transport
        .events
        .lock()
        .unwrap()
        .extend(TestProcessTransport::completion(false));
    let mut request = command_request();
    request.mounts = vec![runtime.state.config.default_workspace_mount()];
    let result = bounded(runtime.run_isolated(request)).await.unwrap();
    assert_eq!(result.stdout, "exact\0tail");
    assert_eq!(result.stderr, "warn\0");
    assert!(!result.cancelled);
    assert_eq!(result.enforcement, LinuxEnforcementReceipt::default());
    let request = transport.request.lock().unwrap().clone().unwrap();
    assert_eq!(request["include_default_mounts"], false);
    assert_eq!(request["mounts"].as_array().unwrap().len(), 1);
    assert!(!transport.alive.load(Ordering::Acquire));
}

struct BlockingSink(tokio::sync::Notify);

#[async_trait]
impl ProcessStreamSink for BlockingSink {
    async fn stdout_line(&self, _: String) -> Result<(), mobile_linux_api::ProcessError> {
        self.0.notify_one();
        std::future::pending().await
    }
    async fn stderr_chunk(&self, _: Vec<u8>) -> Result<(), mobile_linux_api::ProcessError> {
        Ok(())
    }
}

#[tokio::test]
async fn streaming_drop_cancels_a_blocked_sink_and_drains_native_process() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    transport
        .events
        .lock()
        .unwrap()
        .push_back(TestProcessTransport::completion(false).remove(0));
    let sink = Arc::new(BlockingSink(tokio::sync::Notify::new()));
    let run_runtime = runtime.clone();
    let run_sink = sink.clone();
    let caller =
        tokio::spawn(async move { run_runtime.run_streaming(command_request(), run_sink).await });
    bounded(sink.0.notified()).await;
    caller.abort();
    let _ = caller.await;
    assert_eq!(
        wait_terminal(&runtime).await.status,
        MobileLinuxTaskStatus::Cancelled
    );
    assert!(!transport.alive.load(Ordering::Acquire));
}

struct SlowSink;

#[async_trait]
impl ProcessStreamSink for SlowSink {
    async fn stdout_line(&self, _: String) -> Result<(), mobile_linux_api::ProcessError> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }
    async fn stderr_chunk(&self, _: Vec<u8>) -> Result<(), mobile_linux_api::ProcessError> {
        Ok(())
    }
}

#[tokio::test]
async fn completed_native_process_is_not_mislabeled_timeout_while_draining() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    transport
        .events
        .lock()
        .unwrap()
        .extend(TestProcessTransport::completion(false));
    transport.already_stopped.store(true, Ordering::Release);
    let mut request = command_request();
    request.timeout_ms = Some(5);
    let result = bounded(runtime.run_streaming(request, Arc::new(SlowSink)))
        .await
        .unwrap();
    assert!(!result.timed_out);
    assert!(!result.cancelled);
    assert_eq!(result.stdout, "exact\0tail");
    assert_eq!(
        wait_terminal(&runtime).await.status,
        MobileLinuxTaskStatus::Completed
    );
}

#[tokio::test]
async fn foreground_native_resource_failure_stays_typed_after_terminal_drain() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    let mut events = TestProcessTransport::completion(false);
    events.last_mut().unwrap().error = Some(NativeErrorPayload {
        code: "resource_limit_exceeded".into(),
        message: "guest RSS".into(),
    });
    assert_eq!(
        background_terminal_state(false, events.last().unwrap()).0,
        MobileLinuxTaskStatus::Failed
    );
    transport.events.lock().unwrap().extend(events);
    assert!(matches!(
        bounded(runtime.run(command_request())).await,
        Err(MobileLinuxError::ResourceLimitExceeded(_))
    ));
    assert!(!transport.alive.load(Ordering::Acquire));
    assert_eq!(
        wait_terminal(&runtime).await.status,
        MobileLinuxTaskStatus::Failed
    );
}

#[test]
fn exact_streaming_capture_stays_bounded_on_utf8_boundary() {
    let captured = cap_capture("🙂".repeat(MAX_STREAM_CAPTURE_BYTES));
    assert_eq!(captured.len(), MAX_STREAM_CAPTURE_BYTES);
    assert!(!captured.ends_with('\n'));
}

#[tokio::test]
async fn raw_resource_failure_disposes_native_session_before_returning_typed_error() {
    let temp = tempfile::tempdir().unwrap();
    let (runtime, transport) = process_test_runtime(&temp);
    let handle = RawStdioSessionHandle {
        id: "test-raw".into(),
        enforcement: Default::default(),
    };
    runtime
        .state
        .raw_stdio
        .lock()
        .unwrap()
        .insert(handle.id.clone(), handle.clone());
    assert!(matches!(
        bounded(runtime.close_raw_stdio(&handle)).await,
        Err(MobileLinuxError::ResourceLimitExceeded(_))
    ));
    assert!(!runtime
        .state
        .raw_stdio
        .lock()
        .unwrap()
        .contains_key(&handle.id));
    assert_eq!(
        *transport.raw_calls.lock().unwrap(),
        vec!["close", "read", "dispose"]
    );
}

fn test_config(root: &Path) -> IosIshRuntimeConfig {
    IosIshRuntimeConfig {
        managed_root: root.join("mobile-linux"),
        app_sandbox_root: root.to_path_buf(),
        workspace_host_path: root.join("workspaces/default"),
        stable_workspace_id: "default".to_string(),
        abi: "arm64".to_string(),
        rootfs_version: "v1".to_string(),
        archive_sha256: None,
        authorization_file: None,
        rootfs_archive_path: None,
        default_mount_path: None,
        rootfs_patch_path: None,
        protected_host_roots: vec![root.join(".lingxi")],
        allowed_mount_roots: vec![root.join("exports")],
        allowed_guest_roots: vec!["/exports".into()],
    }
}

fn command_request() -> LinuxCommandRequest {
    LinuxCommandRequest {
        command: "/bin/sh".to_string(),
        args: vec!["-lc".to_string(), "echo hi".to_string()],
        cwd: Some("/workspace/default".to_string()),
        env: BTreeMap::from([(String::from("TERM"), String::from("xterm-256color"))]),
        stdin: None,
        timeout_ms: Some(1000),
        network: NetworkPolicy::Allowed,
        resource_limits: Default::default(),
        mounts: Vec::new(),
    }
}

#[test]
fn streaming_capture_is_bounded_without_truncating_utf8() {
    let mut stdout = String::new();
    append_capped_stdout(&mut stdout, &"🙂".repeat(MAX_STREAM_CAPTURE_BYTES));
    assert!(stdout.len() <= MAX_STREAM_CAPTURE_BYTES);
    assert!(std::str::from_utf8(stdout.as_bytes()).is_ok());

    let mut stderr = Vec::new();
    append_capped_bytes(&mut stderr, &vec![b'x'; MAX_STREAM_CAPTURE_BYTES * 2]);
    assert_eq!(stderr.len(), MAX_STREAM_CAPTURE_BYTES);
}

#[test]
fn run_request_payload_marks_isolated_runs_as_request_only() {
    let request = command_request();
    let payload = RunRequestPayload::from_request(&request, &[], false);
    let json = serde_json::to_value(&payload).expect("serialize payload");

    assert_eq!(
        json.get("include_default_mounts")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
}

#[test]
fn workspace_mount_accepts_the_managed_workspace_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    fs::create_dir_all(config.workspace_host_path.clone()).expect("create workspace");
    fs::create_dir_all(config.managed_root.clone()).expect("create managed root");
    fs::create_dir_all(config.app_sandbox_root.join(".lingxi")).expect("create .lingxi");

    let mount = validate_mount(&config.default_workspace_mount(), &config).expect("mount");
    assert_eq!(
        mount.host_path,
        normalize_host_path(&config.workspace_host_path, "workspace_host_path")
            .expect("normalized workspace path")
    );
    assert_eq!(mount.guest_path, "/workspace/default");
}

#[test]
fn runtime_keeps_root_on_a_protected_persistent_host_mount() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    let runtime = IosIshRuntime::new_state(config.clone());
    let mounts = runtime.state.mounts.read().expect("mounts");
    let home = mounts
        .iter()
        .find(|mount| mount.guest_path == "/root")
        .expect("persistent home mount");
    assert_eq!(home.host_path, config.managed_root.join("persistent/root"));
    assert!(!home.read_only);
}

/// `current_mounts` is the table `GuestPathFileSystem` translates against;
/// it must expose the same live state the runtime actually mounts —
/// including the persistent `/root` bind the runtime adds on its own.
#[test]
fn current_mounts_exposes_the_live_workspace_and_home_table() {
    use mobile_linux_api::MobileLinuxRuntime as _;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    let runtime = IosIshRuntime::new_state(config.clone());
    let mounts = runtime.current_mounts();
    assert_eq!(mounts.len(), 2);
    assert_eq!(
        mounts[0].guest_path,
        mobile_linux_api::guest_paths::workspace(&config.stable_workspace_id)
    );
    assert_eq!(mounts[1].guest_path, mobile_linux_api::guest_paths::HOME);
    assert_eq!(
        mounts[1].host_path,
        config.managed_root.join("persistent/root")
    );
}

#[test]
fn merged_mounts_keep_default_workspace_and_home_binds() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    let runtime = IosIshRuntime::new_state(config.clone());
    let host_path = root.join("exports").join("data");
    fs::create_dir_all(config.workspace_host_path.clone()).expect("create workspace");
    fs::create_dir_all(config.managed_root.clone()).expect("create managed root");
    fs::create_dir_all(config.app_sandbox_root.join(".lingxi")).expect("create .lingxi");
    fs::create_dir_all(&host_path).expect("create build root");

    let mounts = runtime
        .merged_mounts(&[MountSpec {
            host_path,
            guest_path: "/exports/data".to_string(),
            read_only: false,
            purpose: MountPurpose::Shared,
        }])
        .expect("merged mounts");
    assert_eq!(mounts.len(), 3);
    assert!(mounts
        .iter()
        .any(|mount| mount.guest_path == "/workspace/default"));
    assert!(mounts.iter().any(|mount| mount.guest_path == "/root"));
    assert!(mounts.iter().any(|mount| {
        mount.guest_path == "/exports/data" && matches!(mount.purpose, MountPurpose::Shared)
    }));
}

#[test]
fn workspace_mount_rejects_guest_path_escape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    fs::create_dir_all(config.workspace_host_path.clone()).expect("create workspace");
    fs::create_dir_all(config.managed_root.clone()).expect("create managed root");
    fs::create_dir_all(config.app_sandbox_root.join(".lingxi")).expect("create .lingxi");

    let error = validate_mount(
        &MountSpec {
            host_path: config.workspace_host_path.clone(),
            guest_path: "/workspace/../etc".to_string(),
            read_only: false,
            purpose: MountPurpose::Workspace,
        },
        &config,
    )
    .expect_err("mount should fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn request_mount_cannot_replace_persistent_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    let host = root.join("external");
    fs::create_dir_all(config.workspace_host_path.clone()).expect("create workspace");
    fs::create_dir_all(config.managed_root.clone()).expect("create managed root");
    fs::create_dir_all(config.app_sandbox_root.join(".lingxi")).expect("create .lingxi");
    fs::create_dir_all(&host).expect("create external");

    let error = validate_mount(
        &MountSpec {
            host_path: host,
            guest_path: "/root".to_string(),
            read_only: false,
            purpose: MountPurpose::External,
        },
        &config,
    )
    .expect_err("persistent root override must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn validate_request_accepts_guest_group_memory_limit() {
    let mut request = command_request();
    request.resource_limits.max_memory_mb = Some(800);
    validate_request(&request).expect("memory watchdog is supported");

    request.resource_limits.max_memory_mb = Some(0);
    assert!(matches!(
        validate_request(&request),
        Err(MobileLinuxError::InvalidRequest(_))
    ));
}

#[cfg(unix)]
#[test]
fn mount_rejects_symlink_alias_into_protected_roots() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    let alias_root = root.join("aliases");
    fs::create_dir_all(config.workspace_host_path.clone()).expect("create workspace");
    fs::create_dir_all(config.managed_root.clone()).expect("create managed root");
    fs::create_dir_all(config.app_sandbox_root.join(".lingxi")).expect("create .lingxi");
    fs::create_dir_all(&alias_root).expect("create aliases");

    let alias = alias_root.join("managed-link");
    std::os::unix::fs::symlink(config.managed_root.clone(), &alias).expect("create symlink");

    let error = validate_mount(
        &MountSpec {
            host_path: alias.join("rootfs"),
            guest_path: "/tmp/managed".to_string(),
            read_only: true,
            purpose: MountPurpose::Temp,
        },
        &config,
    )
    .expect_err("mount should fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn filesystem_rootfs_state_marks_invalid_active_root_as_corrupt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let config = test_config(&root);
    fs::create_dir_all(config.managed_root.clone()).expect("create managed root");
    fs::write(config.active_root(), b"not-a-directory").expect("write active root file");

    assert_eq!(
        filesystem_rootfs_state(&config.active_root(), &config.managed_root),
        RootfsState::Corrupt
    );
}

#[test]
fn native_run_envelope_maps_to_command_result() {
    let result = parse_run_response(
        r#"{"ok":true,"result":{"stdout":"Linux\n","stderr":"","exit_code":0,"timed_out":false,"cancelled":false,"network_policy_enforced":true,"memory_limit_enforced":true}}"#,
    )
    .expect("parse native result");

    assert_eq!(result.stdout, "Linux\n");
    assert_eq!(result.exit_code, 0);
    assert!(!result.timed_out);
    assert_eq!(
        result.enforcement,
        LinuxEnforcementReceipt {
            network_policy_enforced: true,
            memory_limit_enforced: true,
        }
    );
}

#[test]
fn pty_closed_event_carries_and_defaults_the_guest_exit_code() {
    // Shell SELF-exit (bridge's ISHProcessExited observer): the real
    // wait-status-decoded code rides in `exit_code`.
    let events = parse_poll_events(
        r#"{"ok":true,"events":[{"sequence":7,"session_id":"s1","kind":"pty_closed","data_base64":null,"detail":null,"exit_code":3}]}"#,
    )
    .expect("parse pty_closed with exit_code");
    assert_eq!(events[0].kind, "pty_closed");
    assert_eq!(events[0].exit_code, Some(3));

    // Bridge-initiated close omits the field entirely — must default to
    // None (the reader then reports the historical code 0), not fail.
    let events = parse_poll_events(
        r#"{"ok":true,"events":[{"sequence":8,"session_id":"s1","kind":"pty_closed","data_base64":null,"detail":null}]}"#,
    )
    .expect("parse pty_closed without exit_code");
    assert_eq!(events[0].exit_code, None);
}

#[test]
fn native_error_envelope_is_not_treated_as_success() {
    let error =
        parse_native_ok(r#"{"ok":false,"error":{"code":"unavailable","message":"device only"}}"#)
            .expect_err("native error must propagate");

    assert!(matches!(error, MobileLinuxError::Unavailable(_)));
}

#[test]
fn validate_request_accepts_enforceable_network_policies() {
    let mut request = command_request();
    request.network = NetworkPolicy::Disabled;
    validate_request(&request).expect("Disabled is supported");

    request.network = NetworkPolicy::LoopbackOnly;
    validate_request(&request).expect("LoopbackOnly is supported");
}

#[test]
fn validate_request_rejects_zero_timeout_and_invalid_env_keys() {
    let mut request = command_request();
    request.timeout_ms = Some(0);
    let error = validate_request(&request).expect_err("zero timeout must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));

    request = command_request();
    request.env.insert(String::new(), String::from("value"));
    let error = validate_request(&request).expect_err("empty env key must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));

    request = command_request();
    request
        .env
        .insert(String::from("BAD=KEY"), String::from("value"));
    let error = validate_request(&request).expect_err("env key with '=' must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn validate_request_rejects_nul_bytes() {
    let mut request = command_request();
    request.command = "/bin/\0sh".to_string();
    let error = validate_request(&request).expect_err("NUL command must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));

    request = command_request();
    request.args.push("bad\0arg".to_string());
    let error = validate_request(&request).expect_err("NUL arg must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));

    request = command_request();
    request
        .env
        .insert(String::from("TERM"), String::from("x\0term"));
    let error = validate_request(&request).expect_err("NUL env value must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn validate_pty_request_rejects_invalid_env_and_nul() {
    let mut request = PtyOpenRequest {
        command: "/bin/sh".to_string(),
        args: vec!["-i".to_string()],
        cwd: Some("/workspace/default".to_string()),
        env: BTreeMap::from([(String::from("TERM"), String::from("xterm"))]),
        size: PtySize { cols: 80, rows: 24 },
        mounts: Vec::new(),
    };
    request.env.insert(String::new(), String::from("value"));
    let error = validate_pty_request(&request).expect_err("empty env key must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));

    request = PtyOpenRequest {
        command: "/bin/\0sh".to_string(),
        args: vec!["-i".to_string()],
        cwd: Some("/workspace/default".to_string()),
        env: BTreeMap::new(),
        size: PtySize { cols: 80, rows: 24 },
        mounts: Vec::new(),
    };
    let error = validate_pty_request(&request).expect_err("NUL command must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn capability_reports_background_contract_even_when_native_bridge_is_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let runtime = IosIshRuntime::new_state(test_config(&root));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let capability = rt.block_on(runtime.probe_capability());
    assert!(capability.background_processes);
    assert!(capability.streaming_output);
    assert!(!capability.available);

    let error = rt
        .block_on(runtime.spawn_background(command_request()))
        .expect_err("background spawn requires the device bridge");
    assert!(matches!(error, MobileLinuxError::Unavailable(_)));

    let error = rt
        .block_on(runtime.kill(&LinuxProcessHandle {
            id: String::from("bg-1"),
            enforcement: LinuxEnforcementReceipt::default(),
        }))
        .expect_err("unknown background handle must fail");
    assert!(matches!(error, MobileLinuxError::InvalidRequest(_)));
}

#[test]
fn native_background_envelopes_preserve_stream_and_terminal_state() {
    let process = parse_process_id_response(
        r#"{"ok":true,"process_id":"process-42","guest_pid":42,"network_policy_enforced":true,"memory_limit_enforced":true}"#,
    )
    .expect("parse process id");
    assert_eq!(process.process_id, "process-42");
    assert_eq!(
        process.enforcement,
        LinuxEnforcementReceipt {
            network_policy_enforced: true,
            memory_limit_enforced: true,
        }
    );
    assert!(parse_background_kill_response(
        r#"{"ok":true,"process_id":"process-42","termination_requested":true}"#
    )
    .expect("parse requested termination"));
    assert!(!parse_background_kill_response(
        r#"{"ok":true,"process_id":"process-42","already_stopped":true}"#
    )
    .expect("parse idempotent termination"));

    let events = parse_background_events(
        r#"{"ok":true,"events":[{"sequence":7,"kind":"stdout_line","line":"ready","data_base64":null,"exit_code":null,"cancelled":null,"detail":null},{"sequence":8,"kind":"stderr_chunk","line":null,"data_base64":"d2Fybg==","exit_code":null,"cancelled":null,"detail":null},{"sequence":9,"kind":"process_exited","line":null,"data_base64":null,"exit_code":143,"cancelled":true,"detail":"terminated"}]}"#,
    )
    .expect("parse background events");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].line.as_deref(), Some("ready"));
    assert_eq!(
        decode_base64(events[1].data_base64.as_deref().unwrap()).unwrap(),
        b"warn"
    );
    assert_eq!(events[2].exit_code, Some(143));
    assert_eq!(events[2].cancelled, Some(true));

    let (status, exit_code, detail) = background_terminal_state(false, &events[2]);
    assert_eq!(status, MobileLinuxTaskStatus::Cancelled);
    assert_eq!(exit_code, Some(143));
    assert_eq!(detail.as_deref(), Some("terminated"));

    let completed = NativeBackgroundEventPayload {
        sequence: 10,
        kind: "process_exited".to_string(),
        line: None,
        data_base64: None,
        exit_code: Some(0),
        cancelled: Some(false),
        detail: None,
        result: None,
        error: None,
    };
    assert_eq!(
        background_terminal_state(false, &completed).0,
        MobileLinuxTaskStatus::Completed
    );
    assert_eq!(
        background_terminal_state(true, &completed).0,
        MobileLinuxTaskStatus::Cancelled
    );
}

#[test]
fn native_raw_stdio_envelopes_preserve_bytes_and_enforcement() {
    let (session, enforcement) = parse_raw_stdio_open_response(
        r#"{"ok":true,"session_id":"raw-1","network_policy_enforced":true,"memory_limit_enforced":true}"#,
    )
    .expect("parse raw stdio open");
    assert_eq!(session, "raw-1");
    assert_eq!(
        enforcement,
        LinuxEnforcementReceipt {
            network_policy_enforced: true,
            memory_limit_enforced: true,
        }
    );

    let chunk = parse_raw_stdio_read_response(
        r#"{"ok":true,"stdout_base64":"Q29udGVudC1MZW5ndGg6IDINCg0Ke30=","stderr_base64":"d2Fybg==","closed":true,"exit_code":0,"error":null}"#,
    )
    .expect("parse raw stdio read");
    assert_eq!(chunk.stdout, b"Content-Length: 2\r\n\r\n{}");
    assert_eq!(chunk.stderr, b"warn");
    assert!(chunk.closed);
    assert_eq!(chunk.exit_code, Some(0));
}

#[test]
fn raw_stdio_resource_failure_keeps_reap_progress_and_typed_error() {
    let json = r#"{"ok":true,"stdout_base64":"","stderr_base64":"","closed":true,"exit_code":137,"error":{"code":"resource_limit_exceeded","message":"raw output limit"}}"#;
    assert!(matches!(
        parse_raw_stdio_read_response(json),
        Err(MobileLinuxError::ResourceLimitExceeded(_))
    ));
    let (closed, failure) = raw_stdio_close_progress(json).unwrap();
    assert!(closed);
    assert!(matches!(
        failure,
        Some(MobileLinuxError::ResourceLimitExceeded(_))
    ));
}

#[test]
fn simulator_raw_stdio_fails_closed_without_using_pty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let runtime = IosIshRuntime::new_state(test_config(&root));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let error = rt
        .block_on(runtime.open_raw_stdio(RawStdioOpenRequest {
            command: "/opt/lingxi/toolchains/typescript/7.0.2/tsc".to_string(),
            args: vec!["--lsp".to_string(), "--stdio".to_string()],
            cwd: None,
            env: BTreeMap::new(),
            network: NetworkPolicy::Disabled,
            resource_limits: mobile_linux_api::ResourceLimits {
                max_memory_mb: Some(384),
                ..mobile_linux_api::ResourceLimits::default()
            },
            mounts: Vec::new(),
        }))
        .expect_err("non-device builds must reject raw stdio");
    assert!(matches!(error, MobileLinuxError::Unavailable(_)));
    assert!(runtime.state.pty.lock().expect("pty mutex").is_none());
}

#[test]
fn loopback_probe_requires_reachable_field() {
    assert!(parse_loopback_probe(r#"{"ok":true,"reachable":true}"#).unwrap());
    let error = parse_loopback_probe(r#"{"ok":true}"#).expect_err("reachable is required");
    assert!(matches!(error, MobileLinuxError::Io(_)));
}
#[test]
fn process_registry_reuses_identical_configuration_and_rejects_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let mut slot = ProcessRegistry::default();
    let a = IosIshRuntime::from_registry(&mut slot, config.clone()).unwrap();
    let b = IosIshRuntime::from_registry(&mut slot, config.clone()).unwrap();
    assert!(Arc::ptr_eq(&a.state, &b.state));
    let mut different = config;
    different.rootfs_version = "different".into();
    assert!(IosIshRuntime::from_registry(&mut slot, different).is_err());
}

#[tokio::test]
async fn logical_close_does_not_claim_kernel_teardown() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = IosIshRuntime::new_state(test_config(dir.path()));
    runtime.state.kernel_started.store(true, Ordering::Release);
    runtime.shutdown().await.unwrap();
    assert!(runtime.ensure_session_open().is_err());
    assert!(matches!(
        runtime.repair_rootfs().await,
        Err(MobileLinuxError::RestartRequired(_))
    ));
    assert!(matches!(
        runtime.reset_rootfs().await,
        Err(MobileLinuxError::RestartRequired(_))
    ));
}

#[test]
fn additional_mounts_need_explicit_host_and_guest_policy() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let mut mount = MountSpec {
        host_path: dir.path().join("exports/data"),
        guest_path: "/exports/data".into(),
        read_only: true,
        purpose: MountPurpose::Shared,
    };
    assert!(validate_mount(&mount, &config).is_ok());
    mount.guest_path = "/outside".into();
    assert!(validate_mount(&mount, &config).is_err());
    mount.guest_path = "/exports/data".into();
    mount.host_path = dir.path().join("secrets");
    assert!(validate_mount(&mount, &config).is_err());
}

#[test]
fn native_restart_required_remains_structured() {
    assert!(matches!(
        parse_native_ok(
            r#"{"ok":false,"error":{"code":"restart_required","message":"restart process"}}"#
        ),
        Err(MobileLinuxError::RestartRequired(_))
    ));
}
#[test]
fn different_workspaces_share_one_kernel_but_distinct_session_state() {
    let dir = tempfile::tempdir().unwrap();
    let a = test_config(dir.path());
    let mut b = a.clone();
    b.workspace_host_path = dir.path().join("workspaces/second");
    b.stable_workspace_id = "second".into();
    let mut registry = ProcessRegistry::default();
    let first = IosIshRuntime::from_registry(&mut registry, a.clone()).unwrap();
    let second = IosIshRuntime::from_registry(&mut registry, b).unwrap();
    assert!(!Arc::ptr_eq(&first.state, &second.state));
    assert!(Arc::ptr_eq(
        &first.state.native_lock,
        &second.state.native_lock
    ));
    assert!(Arc::ptr_eq(
        &first.state.kernel_started,
        &second.state.kernel_started
    ));
    let mut other_root = a;
    other_root.managed_root = dir.path().join("other-root");
    assert!(IosIshRuntime::from_registry(&mut registry, other_root).is_err());
}
