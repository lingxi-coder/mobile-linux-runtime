use mobile_linux_api::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, LinuxCommandResult, MobileLinuxError, MobileLinuxEventKind,
    MobileLinuxRuntime, MobileLinuxTaskStatus, ProcessStreamSink,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::spawn_blocking;

use super::{
    cap_capture, decode_base64, display_command, native, native_error_to_mobile,
    parse_loopback_probe, validate_request, wait_process_deadline, wait_task_cancel,
    ForegroundCancellation, IosIshRuntime, LoopbackProbePayload, NativeProcessStart, TaskControl,
    BACKGROUND_IDLE_POLL, BACKGROUND_REAP_BUDGET,
};

impl IosIshRuntime {
    /// Probe whether an app-owned loopback server is accepting TCP connections.
    ///
    /// This is deliberately exposed on the concrete iSH runtime rather than the
    /// shared trait because only the iOS native bridge needs host-side probing.
    pub async fn probe_loopback_port(
        &self,
        port: u16,
        timeout: Duration,
    ) -> Result<bool, MobileLinuxError> {
        self.ensure_native_available()?;
        if port == 0 {
            return Err(MobileLinuxError::InvalidRequest(
                "loopback port must be greater than zero".to_string(),
            ));
        }
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&LoopbackProbePayload {
            port,
            timeout_ms: timeout.as_millis().clamp(1, u128::from(u32::MAX)) as u32,
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize loopback probe: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::probe_loopback_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join loopback probe: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_loopback_probe(&response)
    }
}

impl IosIshRuntime {
    pub(super) async fn run_inner(
        &self,
        request: LinuxCommandRequest,
        sink: Option<Arc<dyn ProcessStreamSink>>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.ensure_session_open()?;
        validate_request(&request)?;
        self.boot().await?;
        // Shutdown cannot snapshot tasks until native startup has published its
        // handle. Move this guard into the owner, so dropping the caller while
        // spawn_blocking is pending cannot release the lifecycle too early.
        let startup = self.state.lifecycle.clone().lock_owned().await;
        self.ensure_session_open()?;
        let mounts = self.merged_mounts(&request.mounts)?;
        self.apply_mounts(&mounts).await?;
        let (task_id, task) = self.create_task(
            "task",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        let mut cancellation = ForegroundCancellation {
            task: task.clone(),
            armed: true,
        };
        let runtime = self.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let started = runtime
                .native_spawn_background(&request, &mounts, true)
                .await;
            if let Ok(start) = &started {
                *task
                    .native_handle
                    .lock()
                    .expect("ios-ish native handle mutex") = Some(start.process_id.clone());
            }
            drop(startup);
            let result = match started {
                Ok(start) if !start.process_id.trim().is_empty() => {
                    runtime
                        .drain_foreground_process(&task_id, &task, start, request.timeout_ms, sink)
                        .await
                }
                Ok(_) => Err(MobileLinuxError::Io(
                    "native foreground spawn returned an empty process id".into(),
                )),
                Err(error) => Err(error),
            };
            match &result {
                Ok(result) => {
                    let status = if result.timed_out {
                        MobileLinuxTaskStatus::TimedOut
                    } else if result.cancelled {
                        MobileLinuxTaskStatus::Cancelled
                    } else if result.exit_code == 0 {
                        MobileLinuxTaskStatus::Completed
                    } else {
                        MobileLinuxTaskStatus::Failed
                    };
                    runtime.finish_task(
                        &task_id,
                        &task,
                        status,
                        (!result.timed_out && !result.cancelled).then_some(result.exit_code),
                        if result.timed_out {
                            Some("command timed out".into())
                        } else {
                            result.cancelled.then(|| "command cancelled".into())
                        },
                    );
                }
                Err(error) => {
                    runtime.emit_runtime_error(Some(task_id.clone()), error.to_string());
                    runtime.finish_task(
                        &task_id,
                        &task,
                        MobileLinuxTaskStatus::Failed,
                        None,
                        Some(error.to_string()),
                    );
                }
            }
            let _ = sender.send(result);
        });
        let result = receiver
            .await
            .map_err(|error| MobileLinuxError::Io(format!("foreground owner failed: {error}")))?;
        cancellation.armed = false;
        result
    }
}

impl IosIshRuntime {
    pub(super) async fn drain_foreground_process(
        &self,
        task_id: &str,
        task: &TaskControl,
        start: NativeProcessStart,
        timeout_ms: Option<u64>,
        sink: Option<Arc<dyn ProcessStreamSink>>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        let mut deadline =
            timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
        let mut cursor = None;
        let mut timed_out = false;
        let mut failure = None;
        let mut reap_deadline = None;
        loop {
            timed_out |= deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline);
            if (task.cancel_requested.load(Ordering::Acquire) || timed_out || failure.is_some())
                && reap_deadline.is_none()
            {
                let terminated = self.native_kill_background(&start.process_id).await?;
                // The process can finish before a slow output consumer observes
                // its terminal event. A refused kill of an already-stopped child
                // must not turn that successful execution into a timeout.
                if timed_out && !terminated {
                    timed_out = false;
                    deadline = None;
                }
                reap_deadline = Some(tokio::time::Instant::now() + BACKGROUND_REAP_BUDGET);
            }
            let events = match self
                .native_poll_background(
                    &start.process_id,
                    cursor,
                    mobile_linux_api::MAX_MOBILE_LINUX_EVENT_BATCH as u32,
                )
                .await
            {
                Ok(events) => events,
                Err(error) => {
                    failure.get_or_insert(error);
                    Vec::new()
                }
            };
            let had_events = !events.is_empty();
            for event in events {
                cursor = Some(
                    cursor.map_or(event.sequence, |previous: u64| previous.max(event.sequence)),
                );
                if event.kind == "process_exited" {
                    if let Some(error) = failure.or_else(|| event.error.map(native_error_to_mobile))
                    {
                        return Err(error);
                    }
                    let payload = event.result.ok_or_else(|| {
                        MobileLinuxError::Io(
                            "native foreground terminal event omitted its exact result".into(),
                        )
                    })?;
                    return Ok(LinuxCommandResult {
                        stdout: if sink.is_some() {
                            cap_capture(payload.stdout)
                        } else {
                            payload.stdout
                        },
                        stderr: if sink.is_some() {
                            cap_capture(payload.stderr)
                        } else {
                            payload.stderr
                        },
                        exit_code: if timed_out { -1 } else { payload.exit_code },
                        timed_out: timed_out || payload.timed_out,
                        cancelled: task.cancel_requested.load(Ordering::Acquire)
                            || (payload.cancelled && !timed_out),
                        enforcement: LinuxEnforcementReceipt {
                            network_policy_enforced: payload.network_policy_enforced,
                            memory_limit_enforced: payload.memory_limit_enforced,
                        },
                    });
                }
                let output = match event.kind.as_str() {
                    "stdout_line" => MobileLinuxEventKind::StdoutLine {
                        line: event.line.unwrap_or_default(),
                    },
                    "stderr_chunk" => {
                        match event.data_base64.as_deref().map(decode_base64).transpose() {
                            Ok(Some(chunk)) => MobileLinuxEventKind::StderrChunk { chunk },
                            Ok(None) => {
                                failure.get_or_insert_with(|| {
                                    MobileLinuxError::Io(
                                        "native stderr event omitted data_base64".into(),
                                    )
                                });
                                continue;
                            }
                            Err(error) => {
                                failure.get_or_insert(error);
                                continue;
                            }
                        }
                    }
                    _ => continue,
                };
                self.emit(Some(task_id.to_string()), output.clone());
                if let Some(sink) = &sink {
                    if failure.is_some()
                        || timed_out
                        || task.cancel_requested.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    let callback = async {
                        match output {
                            MobileLinuxEventKind::StdoutLine { line } => {
                                sink.stdout_line(line).await
                            }
                            MobileLinuxEventKind::StderrChunk { chunk } => {
                                sink.stderr_chunk(chunk).await
                            }
                            _ => Ok(()),
                        }
                    };
                    tokio::select! {
                        _ = wait_task_cancel(task) => {},
                        _ = wait_process_deadline(deadline) => { timed_out = true; },
                        result = callback => if let Err(error) = result {
                            failure = Some(MobileLinuxError::from(error));
                        },
                    }
                }
            }
            if reap_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                return Err(failure.unwrap_or_else(|| {
                    MobileLinuxError::Io(format!(
                        "foreground process {} did not drain within 3 seconds after termination",
                        start.process_id
                    ))
                }));
            }
            if had_events {
                tokio::task::yield_now().await;
                continue;
            }
            if reap_deadline.is_some() {
                tokio::time::sleep(BACKGROUND_IDLE_POLL).await;
            } else {
                tokio::select! {
                    _ = wait_task_cancel(task) => {},
                    _ = wait_process_deadline(deadline) => { timed_out = true; },
                    _ = tokio::time::sleep(BACKGROUND_IDLE_POLL) => {},
                }
            }
        }
    }
}
