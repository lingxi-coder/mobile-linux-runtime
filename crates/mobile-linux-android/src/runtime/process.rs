use mobile_linux_api::mobile_linux::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, LinuxCommandResult, MobileLinuxError, MobileLinuxEventKind,
    MobileLinuxTaskStatus, MountSpec, NetworkPolicy, ProcessStreamSink,
};
use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};

use super::{
    display_command, executable_regular_file, join_reader, read_stderr, read_stdout,
    validate_request, wait_raw_stop, AndroidProotRuntime, CallerCancellation, ChildWaitOutcome,
    ForegroundMountMode, MemoryWatchdog, SpawnedChild, StdinWriter, StreamDelivery, TaskControl,
    DEFAULT_TIMEOUT_MS, ENFORCEMENT_RECEIPT_TIMEOUT, MEMORY_POLL_INTERVAL, REAP_BUDGET,
};

impl AndroidProotRuntime {
    pub(super) async fn spawn_child_with_mounts(
        &self,
        request: &LinuxCommandRequest,
        mounts: &[MountSpec],
        isolated_local_app_build_mounts: Option<&[MountSpec]>,
    ) -> Result<SpawnedChild, MobileLinuxError> {
        validate_request(
            request,
            isolated_local_app_build_mounts,
            self.state.config.isolated_build_profile.as_ref(),
        )?;
        let memory_limit_bytes = requested_memory_limit_bytes(request)?;
        if memory_limit_bytes.is_some() {
            ensure_process_group_rss_available()?;
        }
        let receipt_policy = enforced_network_policy_name(request.network);
        let receipt_path = if receipt_policy.is_some() {
            let path = self.state.config.managed_root.join("tmp").join(format!(
                "network-policy-receipt-{}",
                self.state.next_id.fetch_add(1, Ordering::Relaxed)
            ));
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| {
                    MobileLinuxError::Io(format!(
                        "create network enforcement receipt {}: {error}",
                        path.display()
                    ))
                })?;
            Some(path)
        } else {
            None
        };
        let mut command = match self.build_command(
            request,
            request.cwd.as_deref(),
            &request.env,
            mounts,
            receipt_path.as_deref(),
        ) {
            Ok(command) => command,
            Err(error) => {
                if let Some(path) = receipt_path.as_deref() {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = receipt_path.as_deref() {
                    let _ = fs::remove_file(path);
                }
                return Err(MobileLinuxError::Io(format!("spawn PRoot: {error}")));
            }
        };
        let network_policy_enforced = if let Some(path) = receipt_path.as_deref() {
            #[cfg(test)]
            {
                let barrier = self.state.receipt_start_barrier.lock().unwrap().clone();
                if let Some(barrier) = barrier {
                    barrier.wait().await;
                }
            }
            match wait_for_network_policy_receipt(
                &mut child,
                path,
                receipt_policy.expect("restricted policy has receipt name"),
            )
            .await
            {
                Ok(()) => true,
                Err(error) => {
                    if let Some(pid) = child.id() {
                        terminate_group(pid, Signal::SIGKILL);
                    }
                    let _ = child.wait().await;
                    let _ = fs::remove_file(path);
                    return Err(error);
                }
            }
        } else {
            false
        };
        if let Some(path) = receipt_path {
            let _ = fs::remove_file(path);
        }
        Ok(SpawnedChild {
            child,
            enforcement: LinuxEnforcementReceipt {
                network_policy_enforced,
                memory_limit_enforced: memory_limit_bytes.is_some(),
            },
            memory_limit_bytes,
        })
    }
}

impl AndroidProotRuntime {
    pub(super) fn build_command(
        &self,
        request: &LinuxCommandRequest,
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
        mounts: &[MountSpec],
        receipt_path: Option<&Path>,
    ) -> Result<Command, MobileLinuxError> {
        let (proot, rootfs) = self.readiness()?;
        let native_lib_dir = proot.parent().map(Path::to_path_buf);
        let mut proot_args = vec![
            "-0".to_string(),
            "--link2symlink".to_string(),
            "-r".to_string(),
            rootfs.display().to_string(),
            "-b".to_string(),
            "/dev".to_string(),
            "-b".to_string(),
            "/proc".to_string(),
            "-b".to_string(),
            "/sys".to_string(),
            "-w".to_string(),
            cwd.unwrap_or("/root").to_string(),
        ];
        if matches!(request.network, NetworkPolicy::LoopbackOnly) {
            // This enables LingXi's sockaddr-aware extension in the pinned
            // PRoot build. The extension, not the outer launcher, publishes
            // the LoopbackOnly enforcement receipt after initialization.
            proot_args.push("-p".to_string());
        }
        for mount in mounts {
            proot_args.push("-b".to_string());
            proot_args.push(format!(
                "{}:{}",
                mount.host_path.display(),
                mount.guest_path
            ));
        }
        proot_args.push(request.command.clone());
        proot_args.extend(request.args.iter().cloned());

        let mut command = match enforced_network_policy_name(request.network) {
            None => Command::new(&proot),
            Some(policy) => {
                let launcher = self.policy_launcher()?;
                let mut command = Command::new(launcher);
                command.arg(policy).arg(&proot);
                command
            }
        };
        command
            .args(proot_args)
            .env_clear()
            .env("PROOT_TMP_DIR", self.state.config.managed_root.join("tmp"))
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/bin",
            )
            .env("HOME", "/root")
            .envs(env);
        if let Some(receipt_path) = receipt_path {
            command.env("LINGXI_ENFORCEMENT_RECEIPT_PATH", receipt_path);
        }
        if let Some(native_lib_dir) = native_lib_dir {
            command.env("LD_LIBRARY_PATH", &native_lib_dir);
            let loader = native_lib_dir.join("libproot-loader.so");
            if executable_regular_file(&loader) {
                command.env("PROOT_LOADER", loader);
            }
            let loader32 = native_lib_dir.join("libproot-loader32.so");
            if executable_regular_file(&loader32) {
                command.env("PROOT_LOADER_32", loader32);
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        Ok(command)
    }
}

impl AndroidProotRuntime {
    pub(super) async fn spawn_child(
        &self,
        request: &LinuxCommandRequest,
    ) -> Result<SpawnedChild, MobileLinuxError> {
        let mounts = self.execution_mounts(&request.mounts, ForegroundMountMode::Merged)?;
        self.spawn_child_with_mounts(request, &mounts, None).await
    }
}

impl AndroidProotRuntime {
    pub(super) async fn run_inner(
        &self,
        request: LinuxCommandRequest,
        sink: Option<Arc<dyn ProcessStreamSink>>,
        mount_mode: ForegroundMountMode,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.prepare_execution()?;
        let mounts = self.execution_mounts(&request.mounts, mount_mode)?;
        let (id, task) = self.create_task(
            "task",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let mut cancellation = CallerCancellation {
            task: task.clone(),
            stop,
            armed: true,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        // Foreign callers may cancel the future while input is blocked or while
        // the policy launcher is starting. The owner must survive to reap every
        // child and publish a terminal task instead of detaching its drains.
        tokio::spawn(async move {
            let result = runtime
                .run_owned(request, sink, mount_mode, mounts, (&id, &task), stopped)
                .await;
            if let Err(error) = &result {
                runtime.finish_task(
                    &id,
                    &task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
            }
            let _ = reply.send(result);
        });
        let result = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("foreground process owner failed: {error}"))
        })?;
        cancellation.armed = false;
        result
    }
}

impl AndroidProotRuntime {
    pub(super) async fn run_owned(
        &self,
        request: LinuxCommandRequest,
        sink: Option<Arc<dyn ProcessStreamSink>>,
        mount_mode: ForegroundMountMode,
        mounts: Vec<MountSpec>,
        task: (&str, &Arc<TaskControl>),
        mut stopped: tokio::sync::watch::Receiver<bool>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        let (id, task) = task;
        let isolated_local_app_build_mounts =
            matches!(mount_mode, ForegroundMountMode::RequestOnly).then_some(mounts.as_slice());
        let spawned = match self
            .spawn_child_with_mounts(&request, &mounts, isolated_local_app_build_mounts)
            .await
        {
            Ok(spawned) => spawned,
            Err(error) => {
                self.finish_task(
                    id,
                    task,
                    if task.cancel_requested.load(Ordering::Acquire) {
                        MobileLinuxTaskStatus::Cancelled
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let SpawnedChild {
            mut child,
            enforcement,
            memory_limit_bytes,
        } = spawned;
        let pid = child
            .id()
            .ok_or_else(|| MobileLinuxError::Io("PRoot child has no pid".to_string()))?;
        task.pid.store(u64::from(pid), Ordering::Release);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stdout unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stderr unavailable".to_string()))?;
        let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let deadline = tokio::time::Instant::now() + timeout;
        let stream_timed_out = Arc::new(AtomicBool::new(false));
        let (stop_delivery, delivery_stopped) = tokio::sync::watch::channel(false);
        let delivery = StreamDelivery {
            task: task.clone(),
            stopped: delivery_stopped,
            deadline,
            timed_out: stream_timed_out.clone(),
        };
        let stdout_delivery = delivery.clone();
        let stdout_runtime = self.clone();
        let stdout_id = id.to_owned();
        let stdout_sink = sink.clone();
        let stdout_task = tokio::spawn(async move {
            read_stdout(stdout, move |line| {
                stdout_runtime.emit(
                    Some(stdout_id.clone()),
                    MobileLinuxEventKind::StdoutLine { line: line.clone() },
                );
                let sink = stdout_sink.clone();
                let mut delivery = stdout_delivery.clone();
                async move {
                    if let Some(sink) = sink {
                        delivery.deliver(sink.stdout_line(line)).await?;
                    }
                    Ok(())
                }
            })
            .await
        });
        let stderr_runtime = self.clone();
        let stderr_id = id.to_owned();
        let stderr_task = tokio::spawn(async move {
            read_stderr(stderr, move |chunk| {
                stderr_runtime.emit(
                    Some(stderr_id.clone()),
                    MobileLinuxEventKind::StderrChunk {
                        chunk: chunk.clone(),
                    },
                );
                let sink = sink.clone();
                let mut delivery = delivery.clone();
                async move {
                    if let Some(sink) = sink {
                        delivery.deliver(sink.stderr_chunk(chunk)).await?;
                    }
                    Ok(())
                }
            })
            .await
        });
        // Both output drains are live before input can fill either pipe. The
        // writer runs concurrently with timeout and resident-memory enforcement.
        let stdin_writer = StdinWriter::start(child.stdin.take(), request.stdin);
        let waited = if task.cancel_requested.load(Ordering::Acquire) {
            terminate_and_reap(&mut child, pid)
                .await
                .map(ChildWaitOutcome::Exited)
        } else {
            tokio::select! {
                biased;
                _ = wait_raw_stop(&mut stopped) => {
                    terminate_and_reap(&mut child, pid).await.map(ChildWaitOutcome::Exited)
                }
                result = wait_for_child(&mut child, pid, Some(deadline.saturating_duration_since(tokio::time::Instant::now())), memory_limit_bytes) => result,
            }
        };
        // Interrupted execution must not wait for foreign callbacks to return.
        // Readers continue capturing/draining bytes after delivery is cancelled.
        // A normal child exit does not truncate a slow, still-valid sink.
        if task.cancel_requested.load(Ordering::Acquire)
            || !matches!(&waited, Ok(ChildWaitOutcome::Exited(_)))
        {
            stop_delivery.send_replace(true);
        }
        let stdin_result = stdin_writer.finish().await;
        let (exit_code, mut timed_out, memory_limit_exceeded) = match waited {
            Ok(ChildWaitOutcome::Exited(status)) => (status.code().unwrap_or(-1), false, None),
            Ok(ChildWaitOutcome::TimedOut) => (-1, true, None),
            Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic)) => (-1, false, Some(diagnostic)),
            Err(error) => {
                terminate_group(pid, Signal::SIGKILL);
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                let _ = tokio::join!(stdout_task, stderr_task);
                self.finish_task(
                    id,
                    task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let stdout = join_reader(stdout_task, "stdout").await?;
        let stderr = join_reader(stderr_task, "stderr").await?;
        timed_out |= stream_timed_out.load(Ordering::Acquire);
        let cancelled = task.cancel_requested.load(Ordering::Acquire);
        if exit_code == 0 && !cancelled && !timed_out {
            if let Err(error) = stdin_result {
                self.finish_task(
                    id,
                    task,
                    MobileLinuxTaskStatus::Failed,
                    Some(exit_code),
                    Some(error.to_string()),
                );
                return Err(error);
            }
        }
        let status = if memory_limit_exceeded.is_some() {
            MobileLinuxTaskStatus::Failed
        } else if cancelled {
            MobileLinuxTaskStatus::Cancelled
        } else if timed_out {
            MobileLinuxTaskStatus::TimedOut
        } else if exit_code == 0 {
            MobileLinuxTaskStatus::Completed
        } else {
            MobileLinuxTaskStatus::Failed
        };
        self.finish_task(
            id,
            task,
            status,
            (!timed_out && !cancelled).then_some(exit_code),
            if let Some(diagnostic) = &memory_limit_exceeded {
                Some(diagnostic.detail())
            } else if cancelled {
                Some("command cancelled".to_string())
            } else {
                timed_out.then(|| "command timed out".to_string())
            },
        );
        if let Some(diagnostic) = memory_limit_exceeded {
            return Err(MobileLinuxError::ResourceLimitExceeded(
                diagnostic.summary(),
            ));
        }
        Ok(LinuxCommandResult {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
            timed_out,
            cancelled,
            enforcement,
        })
    }
}

pub(super) fn terminate_group(pid: u32, signal: Signal) {
    if let Ok(raw) = i32::try_from(pid) {
        let pid = Pid::from_raw(raw);
        if killpg(pid, signal).is_err() {
            let _ = kill(pid, signal);
        }
    }
}

pub(super) async fn wait_for_network_policy_receipt(
    child: &mut Child,
    path: &Path,
    expected_policy: &str,
) -> Result<(), MobileLinuxError> {
    let expected = format!("{expected_policy}\n");
    let deadline = tokio::time::Instant::now() + ENFORCEMENT_RECEIPT_TIMEOUT;
    loop {
        match fs::read_to_string(path) {
            Ok(value) if value == expected => return Ok(()),
            Ok(value) if !value.is_empty() => {
                return Err(MobileLinuxError::NetworkPolicyUnavailable(format!(
                    "Android policy launcher returned an invalid enforcement receipt: {value:?}"
                )));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(MobileLinuxError::NetworkPolicyUnavailable(format!(
                    "Android policy launcher receipt became unreadable: {error}"
                )));
            }
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| MobileLinuxError::Io(format!("poll policy launcher: {error}")))?
        {
            return Err(MobileLinuxError::NetworkPolicyUnavailable(format!(
                "Android policy launcher exited before enforcement (status {status})"
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(MobileLinuxError::NetworkPolicyUnavailable(
                "Android policy launcher did not prove enforcement before spawn timeout"
                    .to_string(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) fn enforced_network_policy_name(policy: NetworkPolicy) -> Option<&'static str> {
    match policy {
        NetworkPolicy::Disabled => Some("disabled"),
        NetworkPolicy::LoopbackOnly => Some("loopback_only"),
        NetworkPolicy::Allowed => None,
    }
}

pub(super) async fn terminate_and_reap(
    child: &mut Child,
    pid: u32,
) -> Result<std::process::ExitStatus, MobileLinuxError> {
    terminate_group(pid, Signal::SIGTERM);
    if let Ok(result) = tokio::time::timeout(REAP_BUDGET / 2, child.wait()).await {
        // Reaping the launcher does not prove its descendants exited. A child
        // can ignore TERM and retain the output pipes after the group leader
        // has gone; kill the remaining group before waiting for either drain.
        terminate_group(pid, Signal::SIGKILL);
        return result
            .map_err(|error| MobileLinuxError::Io(format!("reap raw stdio child: {error}")));
    }
    terminate_group(pid, Signal::SIGKILL);
    tokio::time::timeout(REAP_BUDGET / 2, child.wait())
        .await
        .map_err(|_| MobileLinuxError::Io("raw stdio process tree survived SIGKILL".to_string()))?
        .map_err(|error| MobileLinuxError::Io(format!("reap raw stdio child: {error}")))
}

pub(super) async fn wait_for_child(
    child: &mut Child,
    pid: u32,
    timeout: Option<Duration>,
    memory_limit_bytes: Option<u64>,
) -> Result<ChildWaitOutcome, MobileLinuxError> {
    let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
    let mut memory_watchdog = memory_limit_bytes.map(MemoryWatchdog::new);
    let mut memory_poll = tokio::time::interval(MEMORY_POLL_INTERVAL);
    memory_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = child.wait() => {
                return result
                    .map(ChildWaitOutcome::Exited)
                    .map_err(|error| MobileLinuxError::Io(format!("wait for PRoot: {error}")));
            }
            _ = memory_poll.tick(), if memory_watchdog.is_some() => {
                let resident = match process_group_rss_bytes(pid) {
                    Ok(resident) => resident,
                    Err(error) => {
                        terminate_group(pid, Signal::SIGKILL);
                        let _ = child.wait().await;
                        return Err(error);
                    }
                };
                let watchdog = memory_watchdog.as_mut().expect("guarded memory watchdog");
                if let Some(diagnostic) = watchdog.observe(resident) {
                    terminate_group(pid, Signal::SIGKILL);
                    let _ = child.wait().await;
                    return Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic));
                }
            }
            () = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                terminate_group(pid, Signal::SIGKILL);
                let _ = child.wait().await;
                return Ok(ChildWaitOutcome::TimedOut);
            }
        }
    }
}

pub(super) fn ensure_process_group_rss_available() -> Result<(), MobileLinuxError> {
    if Path::new("/proc/self/stat").is_file() && Path::new("/proc/self/status").is_file() {
        Ok(())
    } else {
        Err(MobileLinuxError::ResourceLimitExceeded(
            "Android process-group RSS accounting is unavailable".to_string(),
        ))
    }
}

pub(super) fn process_group_rss_bytes(process_group: u32) -> Result<u64, MobileLinuxError> {
    let entries = fs::read_dir("/proc").map_err(|error| {
        MobileLinuxError::ResourceLimitExceeded(format!(
            "read Android process table for memory watchdog: {error}"
        ))
    })?;
    let mut resident_bytes = 0_u64;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let process_dir = entry.path();
        let Ok(stat) = fs::read_to_string(process_dir.join("stat")) else {
            continue;
        };
        let Some(after_name) = stat.rsplit_once(')').map(|(_, tail)| tail.trim()) else {
            continue;
        };
        let mut fields = after_name.split_whitespace();
        let _state = fields.next();
        let _parent_pid = fields.next();
        let Some(group) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if group != process_group {
            continue;
        }
        let Ok(status) = fs::read_to_string(process_dir.join("status")) else {
            continue;
        };
        let resident_kib = status.lines().find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        });
        if let Some(resident_kib) = resident_kib {
            resident_bytes = resident_bytes.saturating_add(resident_kib.saturating_mul(1024));
        } else if pid == process_group {
            return Err(MobileLinuxError::ResourceLimitExceeded(
                "Android memory watchdog could not read root process RSS".to_string(),
            ));
        }
    }
    Ok(resident_bytes)
}

pub(super) fn requested_memory_limit_bytes(
    request: &LinuxCommandRequest,
) -> Result<Option<u64>, MobileLinuxError> {
    let limits = request.resource_limits;
    if limits.max_cpu_seconds.is_some()
        || limits.max_processes.is_some()
        || limits.max_open_files.is_some()
    {
        return Err(MobileLinuxError::ResourceLimitExceeded(
            "Android PRoot currently enforces only max_memory_mb for local-app commands"
                .to_string(),
        ));
    }
    match limits.max_memory_mb {
        Some(0) => Err(MobileLinuxError::InvalidRequest(
            "max_memory_mb must be greater than zero".to_string(),
        )),
        Some(megabytes) => Ok(Some(u64::from(megabytes).saturating_mul(1024 * 1024))),
        None => Ok(None),
    }
}
