use super::mounts::{
    expected_local_app_build_env, parse_local_app_build_guest_path, snapshot_read_only_mounts,
    validate_request,
};
use super::process::{enforced_network_policy_name, requested_memory_limit_bytes};
use async_trait::async_trait;
use mobile_linux_api::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MobileLinuxEventKind, MobileLinuxRuntime,
    MobileLinuxTaskStatus, MountPurpose, MountSpec, NetworkPolicy, ProcessStreamSink,
    RawStdioOpenRequest, SandboxBackend,
};
use nix::sys::signal::kill;
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        let caller = tokio::spawn(async move { execution_runtime.open_raw_stdio(request).await });
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
    assert!(matches!(error, MobileLinuxError::Io(detail) if detail.contains("typed sink failure")));
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

fn fixed_local_app_build_env(app_id: &str, channel: &str) -> (String, BTreeMap<String, String>) {
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

        let error = validate_request(&request, Some(&request.mounts), Some(&test_build_profile()))
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
