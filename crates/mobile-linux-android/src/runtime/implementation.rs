use async_trait::async_trait;
use mobile_linux_api::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, LinuxCommandResult, LinuxProcessHandle, MobileLinuxCapability,
    MobileLinuxError, MobileLinuxEvent, MobileLinuxEventKind, MobileLinuxRuntime,
    MobileLinuxRuntimeMode, MobileLinuxTaskSnapshot, MobileLinuxTaskStatus, MountSpec,
    ProcessStreamSink, PtyOpenRequest, PtySessionHandle, PtySize, RawStdioOpenRequest,
    RawStdioReadResult, RawStdioSessionHandle, RawStdioStopResult, RootfsState, RootfsStatus,
    SandboxBackend,
};
use nix::sys::signal::Signal;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::{
    display_command, raw_stdin_closed, rootfs_store_error, terminate_group, validate_mount,
    validate_pty_request, wait_raw_stop, AndroidProotRuntime, CallerCancellation,
    ForegroundMountMode, PtyControl, RawStdioWrite, ENFORCEMENT_RECEIPT_TIMEOUT, REAP_BUDGET,
};

#[async_trait]
impl MobileLinuxRuntime for AndroidProotRuntime {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::AndroidProot
    }

    fn mode(&self) -> MobileLinuxRuntimeMode {
        MobileLinuxRuntimeMode::MobileLinux
    }

    async fn probe_capability(&self) -> MobileLinuxCapability {
        let readiness = self.readiness();
        MobileLinuxCapability {
            available: readiness.is_ok(),
            backend: SandboxBackend::AndroidProot,
            mode: MobileLinuxRuntimeMode::MobileLinux,
            reason: readiness.err().map(|error| error.to_string()),
            streaming_output: true,
            background_processes: true,
            pty: true,
            bind_mounts: true,
            rootfs_integrity: self.load_rootfs_manifest().is_ok(),
        }
    }

    async fn boot(&self) -> Result<RootfsStatus, MobileLinuxError> {
        self.prepare_execution()?;
        Ok(self.rootfs_snapshot())
    }

    async fn shutdown(&self) -> Result<(), MobileLinuxError> {
        let pty_ids: HashSet<_> = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .keys()
            .cloned()
            .collect();
        let raw_stdio_ids: HashSet<_> = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .keys()
            .cloned()
            .collect();
        let task_ids: Vec<_> = self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .keys()
            .filter(|id| !pty_ids.contains(*id) && !raw_stdio_ids.contains(*id))
            .cloned()
            .collect();
        let mut errors = Vec::new();
        for id in pty_ids {
            if let Err(error) = self.close_pty(&PtySessionHandle { id }).await {
                errors.push(error.to_string());
            }
        }
        for id in raw_stdio_ids {
            if let Err(error) = self
                .close_raw_stdio(&RawStdioSessionHandle {
                    id,
                    enforcement: LinuxEnforcementReceipt::default(),
                })
                .await
            {
                errors.push(error.to_string());
            }
        }
        for id in task_ids {
            if let Err(error) = self
                .kill(&LinuxProcessHandle {
                    id,
                    enforcement: LinuxEnforcementReceipt::default(),
                })
                .await
            {
                errors.push(error.to_string());
            }
        }
        self.state.booted.store(false, Ordering::Release);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(MobileLinuxError::Io(format!(
                "shutdown reaping failed: {}",
                errors.join("; ")
            )))
        }
    }

    async fn run(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, None).await
    }

    async fn run_streaming(
        &self,
        request: LinuxCommandRequest,
        sink: Arc<dyn ProcessStreamSink>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, Some(sink)).await
    }

    async fn spawn_background(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxProcessHandle, MobileLinuxError> {
        self.prepare_execution()?;
        let (id, task) = self.create_task(
            "bg",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Backgrounded,
        );
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let mut cancellation = CallerCancellation {
            task: task.clone(),
            stop,
            armed: true,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        tokio::spawn(async move {
            let result = runtime
                .spawn_background_owned(request, id.clone(), task.clone())
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
            let handle = result.as_ref().ok().cloned();
            let delivered = reply.send(result).is_ok();
            if let Some(handle) = handle {
                // Sending the result is not acceptance: the caller can disappear
                // before polling it. Its guard closes the channel on acceptance
                // or publishes cancellation, leaving this owner to dispose it.
                let abandoned = task.cancel_requested.load(Ordering::Acquire)
                    || !delivered
                    || stopped.wait_for(|cancelled| *cancelled).await.is_ok();
                if abandoned || task.cancel_requested.load(Ordering::Acquire) {
                    task.cancel_requested.store(true, Ordering::Release);
                    if let Err(error) = runtime.kill(&handle).await {
                        runtime.emit(
                            Some(id),
                            MobileLinuxEventKind::RuntimeError {
                                detail: error.to_string(),
                            },
                        );
                    }
                }
            }
        });
        let result = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("process startup owner failed: {error}"))
        })?;
        cancellation.armed = false;
        result
    }

    async fn kill(&self, handle: &LinuxProcessHandle) -> Result<(), MobileLinuxError> {
        let task = self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("unknown task handle".to_string()))?;
        if task.terminal_emitted.load(Ordering::Acquire) {
            return self.close_raw_stdio_if_present(&handle.id).await;
        }
        task.cancel_requested.store(true, Ordering::Release);
        let starting = task.pid.load(Ordering::Acquire) == 0;
        // A foreground owner may still be waiting for its enforcement receipt.
        // Keep the cancellation request pending until it publishes the child;
        // never abandon an owned startup solely because its PID is not ready.
        let budget = REAP_BUDGET
            + if starting {
                ENFORCEMENT_RECEIPT_TIMEOUT
            } else {
                Duration::ZERO
            };
        let deadline = tokio::time::Instant::now() + budget;
        let mut hard_kill_at = None;
        let mut sent_sigkill = false;
        while !task.terminal_emitted.load(Ordering::Acquire) {
            let pid = u32::try_from(task.pid.load(Ordering::Acquire))
                .map_err(|_| MobileLinuxError::Io("invalid task pid".to_string()))?;
            if pid != 0 && hard_kill_at.is_none() {
                terminate_group(pid, Signal::SIGTERM);
                hard_kill_at = Some(tokio::time::Instant::now() + REAP_BUDGET / 2);
            }
            if !sent_sigkill && hard_kill_at.is_some_and(|at| tokio::time::Instant::now() >= at) {
                terminate_group(pid, Signal::SIGKILL);
                sent_sigkill = true;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MobileLinuxError::Io(format!(
                    "task {} did not reap within {} seconds",
                    handle.id,
                    budget.as_secs()
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.close_raw_stdio_if_present(&handle.id).await
    }

    async fn open_pty(
        &self,
        request: PtyOpenRequest,
    ) -> Result<PtySessionHandle, MobileLinuxError> {
        self.prepare_execution()?;
        validate_pty_request(&request, &self.state.config.brand_profile)?;
        let mounts = self.execution_mounts(&request.mounts, ForegroundMountMode::Merged)?;
        let (program, args, env) = self.build_pty_invocation(
            &request.command,
            &request.args,
            request.cwd.as_deref(),
            &request.env,
            &mounts,
        )?;
        let (id, task) = self.create_task(
            "pty",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        let spawned = platform_pty::spawn_pty_process(
            &program,
            &args,
            &self.state.config.managed_root,
            &env,
            &None,
            platform_pty::TerminalSize {
                cols: request.size.cols,
                rows: request.size.rows,
            },
            &[],
        )
        .await;
        let spawned = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                let error = MobileLinuxError::Io(format!("spawn PRoot PTY: {error}"));
                self.finish_task(
                    &id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        let process = Arc::new(spawned.session);
        if let Some(pid) = process.process_id() {
            task.pid.store(u64::from(pid), Ordering::Release);
        }
        let control = Arc::new(PtyControl {
            task: task.clone(),
            process: process.clone(),
        });
        self.state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .insert(id.clone(), control);
        let mut stdout = spawned.stdout_rx;
        let exit = spawned.exit_rx;
        let runtime = self.clone();
        let reaper_id = id.clone();
        tokio::spawn(async move {
            let output_runtime = runtime.clone();
            let output_id = reaper_id.clone();
            let output_task = tokio::spawn(async move {
                while let Some(data) = stdout.recv().await {
                    output_runtime.emit(
                        Some(output_id.clone()),
                        MobileLinuxEventKind::PtyOutput {
                            session_id: output_id.clone(),
                            data,
                        },
                    );
                }
            });
            let result = exit.await;
            let _ = output_task.await;
            let cancelled = task.cancel_requested.load(Ordering::Acquire);
            let (status, code, detail) = match result {
                Ok(code) => (
                    if cancelled {
                        MobileLinuxTaskStatus::Cancelled
                    } else if code == 0 {
                        MobileLinuxTaskStatus::Completed
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    Some(code),
                    cancelled.then(|| "PTY closed".to_string()),
                ),
                Err(error) => (
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(format!("PTY exit channel closed: {error}")),
                ),
            };
            runtime.finish_task(&reaper_id, &task, status, code, detail.clone());
            runtime.emit(
                Some(reaper_id.clone()),
                MobileLinuxEventKind::PtyClosed {
                    session_id: reaper_id.clone(),
                    exit_code: code,
                    detail,
                },
            );
            runtime
                .state
                .ptys
                .lock()
                .expect("mobile-linux PTY mutex")
                .remove(&reaper_id);
        });
        Ok(PtySessionHandle { id })
    }

    async fn open_raw_stdio(
        &self,
        request: RawStdioOpenRequest,
    ) -> Result<RawStdioSessionHandle, MobileLinuxError> {
        self.prepare_execution()?;
        let executor = super::raw_stdio::raw_stdio_stop_executor()?;
        // Reserve cleanup capacity before a process can be created. An
        // admitted session must never lose its ability to request stop.
        let owner_permit = super::raw_stdio::raw_stdio_stop_capacity()
            .try_acquire_owned()
            .map_err(|_| {
                MobileLinuxError::ResourceLimitExceeded("raw stdio owner capacity exhausted".into())
            })?;
        let (id, task) =
            self.create_raw_stdio_task(display_command(&request.command, &request.args))?;
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let mut cancellation = CallerCancellation {
            task: task.clone(),
            stop,
            armed: true,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        executor.spawn(async move {
            let result = runtime
                .open_raw_stdio_owned(request, id.clone(), task.clone(), owner_permit)
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
            let handle = result.as_ref().ok().cloned();
            let delivered = reply.send(result).is_ok();
            if let Some(handle) = handle {
                // Sending the result is not acceptance: the caller can disappear
                // before polling it. Its guard closes the channel on acceptance
                // or publishes cancellation, leaving this owner to dispose it.
                let abandoned = task.cancel_requested.load(Ordering::Acquire)
                    || !delivered
                    || stopped.wait_for(|cancelled| *cancelled).await.is_ok();
                if abandoned || task.cancel_requested.load(Ordering::Acquire) {
                    task.cancel_requested.store(true, Ordering::Release);
                    if let Err(error) = runtime.close_raw_stdio_if_present(&handle.id).await {
                        runtime.emit(
                            Some(id),
                            MobileLinuxEventKind::RuntimeError {
                                detail: error.to_string(),
                            },
                        );
                    }
                }
            }
        });
        let result = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("process startup owner failed: {error}"))
        })?;
        cancellation.armed = false;
        result
    }

    async fn write_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest("unknown raw stdio session".to_string())
            })?;
        let mut stopped = control.stop.subscribe();
        let (reply, response) = tokio::sync::oneshot::channel();
        tokio::select! {
            biased;
            _ = wait_raw_stop(&mut stopped) => Err(raw_stdin_closed()),
            result = async {
                control.writes.send(RawStdioWrite { input, reply }).await
                    .map_err(|_| raw_stdin_closed())?;
                response.await.map_err(|_| raw_stdin_closed())?
            } => result,
        }
    }

    async fn read_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        max_bytes: usize,
    ) -> Result<RawStdioReadResult, MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest("unknown raw stdio session".to_string())
            })?;
        let _gate = control.stop_gate.lock().await;
        if control
            .stop_result
            .lock()
            .expect("raw stop result")
            .is_some()
            || !self
                .state
                .raw_stdio
                .lock()
                .expect("raw stdio mutex")
                .get(&handle.id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &control))
        {
            return Err(MobileLinuxError::InvalidRequest(
                "raw stdio session is stopping or closed".into(),
            ));
        }
        let mut guard = control.buffers.lock().expect("raw stdio buffers mutex");
        if let Some(error) = &guard.error {
            return Err(error.clone());
        }
        if guard.stdout_overflowed || guard.stderr_overflowed {
            return Err(MobileLinuxError::Io(
                "raw stdio unread output exceeded 512 KiB".to_string(),
            ));
        }
        let (stdout, stderr) = Self::drain_raw_stdio_fair(&mut guard, max_bytes);
        Ok(RawStdioReadResult {
            stdout,
            stderr,
            closed: guard.closed && guard.stdout.is_empty() && guard.stderr.is_empty(),
            exit_code: guard.exit_code.filter(|code| *code >= 0),
        })
    }

    async fn close_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
    ) -> Result<(), MobileLinuxError> {
        let runtime = self.clone();
        let id = handle.id.clone();
        super::raw_stdio::raw_stdio_stop_executor()?
            .spawn(async move { runtime.close_raw_stdio_owned(&id, false).await })
            .await
            .map_err(|error| {
                MobileLinuxError::Io(format!("raw stdio close owner failed: {error}"))
            })?
    }

    async fn stop_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        max_bytes_per_stream: usize,
    ) -> Result<RawStdioStopResult, MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest(
                    "unknown raw stdio session; cleanup is unverified".into(),
                )
            })?;
        let executor = super::raw_stdio::raw_stdio_stop_executor()?;
        let mut completion = {
            let mut slot = control.stop_completion.lock().expect("raw stop completion");
            if let Some(existing) = slot.as_ref() {
                existing.clone()
            } else {
                let (finished, receiver) = tokio::sync::watch::channel(None);
                let runtime = self.clone();
                let handle = handle.clone();
                executor.spawn(async move {
                    let result = runtime
                        .stop_raw_stdio_owned(&handle, usize::MAX)
                        .await
                        .map(|_| ());
                    finished.send_replace(Some(result));
                });
                *slot = Some(receiver.clone());
                receiver
            }
        };
        loop {
            let result = completion.borrow().clone();
            if let Some(result) = result {
                result?;
                let output = control
                    .stop_result
                    .lock()
                    .expect("raw stop result")
                    .clone()
                    .ok_or_else(|| MobileLinuxError::Io("raw stop result missing".into()))?;
                return Ok(super::raw_stdio::bounded_stop_result(
                    output,
                    max_bytes_per_stream,
                ));
            }
            completion.changed().await.map_err(|_| {
                MobileLinuxError::Io("raw stdio stop owner ended without a result".into())
            })?;
        }
    }

    async fn write_pty(
        &self,
        handle: &PtySessionHandle,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("stale PTY handle".to_string()))?;
        if input == [3] {
            return control
                .process
                .signal(platform_pty::ProcessSignal::Interrupt)
                .map_err(|error| MobileLinuxError::Io(format!("interrupt PTY: {error}")));
        }
        control
            .process
            .write(input)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("write PTY: {error}")))
    }

    async fn resize_pty(
        &self,
        handle: &PtySessionHandle,
        size: PtySize,
    ) -> Result<(), MobileLinuxError> {
        if size.cols == 0 || size.rows == 0 {
            return Err(MobileLinuxError::InvalidRequest(
                "PTY dimensions must be non-zero".to_string(),
            ));
        }
        let control = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("stale PTY handle".to_string()))?;
        control
            .process
            .resize(platform_pty::TerminalSize {
                cols: size.cols,
                rows: size.rows,
            })
            .map_err(|error| MobileLinuxError::Io(format!("resize PTY: {error}")))
    }

    async fn close_pty(&self, handle: &PtySessionHandle) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .ptys
            .lock()
            .expect("mobile-linux PTY mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("stale PTY handle".to_string()))?;
        control.task.cancel_requested.store(true, Ordering::Release);
        control.process.close_stdin();
        let _ = control
            .process
            .signal(platform_pty::ProcessSignal::Terminate);
        let deadline = tokio::time::Instant::now() + REAP_BUDGET;
        let hard_kill_at = tokio::time::Instant::now() + REAP_BUDGET / 2;
        let mut forced = false;
        while !control.task.terminal_emitted.load(Ordering::Acquire) {
            if !forced && tokio::time::Instant::now() >= hard_kill_at {
                control.process.terminate();
                forced = true;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MobileLinuxError::Io(format!(
                    "PTY {} did not reap within 2 seconds",
                    handle.id
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    async fn rootfs_status(&self) -> Result<RootfsStatus, MobileLinuxError> {
        Ok(self.rootfs_snapshot())
    }

    async fn verify_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let manifest = self.load_rootfs_manifest()?;
        Ok(self.rootfs_store().status(&manifest))
    }

    async fn repair_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let manifest = self.load_rootfs_manifest()?;
        let store = self.rootfs_store();
        let status = store
            .recover_interrupted_activation(&manifest)
            .map_err(rootfs_store_error)?;
        if matches!(status.state, RootfsState::Ready) || status.staged_root.is_none() {
            return Ok(status);
        }
        store
            .activate_staged_rootfs(&manifest)
            .map_err(rootfs_store_error)
    }

    async fn reset_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let manifest = self.load_rootfs_manifest()?;
        self.shutdown().await?;
        let store = self.rootfs_store();
        store
            .reset_writable_state(&manifest)
            .map_err(rootfs_store_error)?;
        Ok(store.status(&manifest))
    }

    async fn configure_mounts(&self, mounts: Vec<MountSpec>) -> Result<(), MobileLinuxError> {
        for mount in &mounts {
            validate_mount(
                mount,
                &self.state.config.managed_root,
                &self.state.config.brand_profile,
            )?;
        }
        *self
            .state
            .mounts
            .write()
            .expect("mobile-linux mounts rwlock") = mounts;
        Ok(())
    }

    fn current_mounts(&self) -> Vec<MountSpec> {
        let mut mounts = self.home_mounts();
        mounts.extend(
            self.state
                .mounts
                .read()
                .expect("mobile-linux mounts rwlock")
                .clone(),
        );
        mounts
    }

    async fn read_events(
        &self,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Vec<MobileLinuxEvent>, MobileLinuxError> {
        let after = after_sequence.unwrap_or(0);
        Ok(self
            .state
            .events
            .lock()
            .expect("mobile-linux events mutex")
            .iter()
            .filter(|event| event.sequence > after)
            .take(limit.min(mobile_linux_api::MAX_MOBILE_LINUX_EVENT_BATCH))
            .cloned()
            .collect())
    }

    async fn list_tasks(&self) -> Result<Vec<MobileLinuxTaskSnapshot>, MobileLinuxError> {
        let mut tasks: Vec<_> = self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .values()
            .map(|task| task.snapshot.lock().expect("task snapshot mutex").clone())
            .collect();
        tasks.sort_by_key(|task| task.started_at_ms);
        Ok(tasks)
    }

    async fn task_status(
        &self,
        task_id: &str,
    ) -> Result<Option<MobileLinuxTaskSnapshot>, MobileLinuxError> {
        Ok(self
            .state
            .tasks
            .lock()
            .expect("mobile-linux tasks mutex")
            .get(task_id)
            .map(|task| task.snapshot.lock().expect("task snapshot mutex").clone()))
    }
}
