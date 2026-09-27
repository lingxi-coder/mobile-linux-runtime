use async_trait::async_trait;
use mobile_linux_api::mobile_linux::LinuxEnforcementReceipt;
use mobile_linux_api::{
    LinuxCommandRequest, LinuxCommandResult, LinuxProcessHandle, MobileLinuxCapability,
    MobileLinuxError, MobileLinuxEvent, MobileLinuxRuntime, MobileLinuxRuntimeMode,
    MobileLinuxTaskSnapshot, MobileLinuxTaskStatus, MountSpec, ProcessStreamSink, PtyOpenRequest,
    PtySessionHandle, PtySize, RawStdioOpenRequest, RawStdioReadResult, RawStdioSessionHandle,
    RootfsState, RootfsStatus, SandboxBackend,
};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::spawn_blocking;

use super::{
    display_command, encode_base64, filesystem_rootfs_state, native, parse_native_ok,
    parse_raw_stdio_read_response, raw_stdio_close_progress, validate_mount, validate_pty_request,
    validate_request, ForegroundMountMode, IosIshRuntime, PtyControl, RawStdioNativeOperation,
    RawStdioRequestPayload, BACKGROUND_REAP_BUDGET,
};

#[async_trait]
impl MobileLinuxRuntime for IosIshRuntime {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::IosIsh
    }

    fn mode(&self) -> MobileLinuxRuntimeMode {
        MobileLinuxRuntimeMode::MobileLinux
    }

    async fn probe_capability(&self) -> MobileLinuxCapability {
        let reason = self.native_unavailable_reason();
        MobileLinuxCapability {
            available: reason.is_none(),
            backend: SandboxBackend::IosIsh,
            mode: MobileLinuxRuntimeMode::MobileLinux,
            reason,
            streaming_output: true,
            background_processes: true,
            pty: true,
            bind_mounts: true,
            rootfs_integrity: true,
        }
    }

    async fn boot(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let _lifecycle = self.state.lifecycle.lock().await;
        if self.state.booted.load(Ordering::Acquire) {
            let cached = self
                .state
                .rootfs_status_cache
                .lock()
                .expect("ios-ish rootfs status cache")
                .clone();
            if let Some(snapshot) = cached {
                return Ok(snapshot);
            }
            return self.refresh_rootfs_snapshot().await;
        }
        self.ensure_native_available()?;
        fs::create_dir_all(&self.state.config.managed_root).map_err(|error| {
            MobileLinuxError::Io(format!(
                "create managed root {}: {error}",
                self.state.config.managed_root.display()
            ))
        })?;
        let active_root = self.state.config.active_root();
        let current_state = filesystem_rootfs_state(&active_root, &self.state.config.managed_root);
        if matches!(current_state, RootfsState::Missing) {
            self.install_rootfs(false).await?;
        } else if matches!(current_state, RootfsState::Corrupt) {
            return Err(MobileLinuxError::Integrity(
                "active iSH rootfs is corrupt; run repair_rootfs".to_string(),
            ));
        }
        self.native_boot().await?;
        self.state.kernel_started.store(true, Ordering::Release);
        self.state.closed.store(false, Ordering::Release);
        self.state.booted.store(true, Ordering::Release);
        self.refresh_rootfs_snapshot().await
    }

    async fn shutdown(&self) -> Result<(), MobileLinuxError> {
        let _lifecycle = self.state.lifecycle.lock().await;
        let mut close_errors = Vec::new();
        let current_pty = self.state.pty.lock().expect("ios-ish pty mutex").clone();
        if let Some((session_id, _)) = current_pty {
            if let Err(error) = self.close_pty(&PtySessionHandle { id: session_id }).await {
                close_errors.push(error.to_string());
            }
        }
        let raw_sessions: Vec<_> = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .values()
            .cloned()
            .collect();
        for session in raw_sessions {
            if let Err(error) = self.close_raw_stdio(&session).await {
                close_errors.push(error.to_string());
            }
        }
        let task_ids: Vec<_> = self
            .state
            .tasks
            .lock()
            .expect("ios-ish tasks mutex")
            .iter()
            .filter(|(_, task)| !task.terminal_emitted.load(Ordering::Acquire))
            .map(|(id, _)| id.clone())
            .collect();
        for id in task_ids {
            if let Err(error) = self
                .kill(&LinuxProcessHandle {
                    id,
                    enforcement: LinuxEnforcementReceipt::default(),
                })
                .await
            {
                close_errors.push(error.to_string());
            }
        }
        if !close_errors.is_empty() {
            return Err(MobileLinuxError::Io(close_errors.join("; ")));
        }
        self.state.closed.store(true, Ordering::Release);
        self.state.booted.store(false, Ordering::Release);
        Ok(())
    }

    async fn run(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, ForegroundMountMode::Merged, None)
            .await
    }

    async fn run_isolated(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, ForegroundMountMode::RequestOnly, None)
            .await
    }

    async fn run_streaming(
        &self,
        request: LinuxCommandRequest,
        sink: Arc<dyn ProcessStreamSink>,
    ) -> Result<LinuxCommandResult, MobileLinuxError> {
        self.run_inner(request, ForegroundMountMode::Merged, Some(sink))
            .await
    }

    async fn spawn_background(
        &self,
        request: LinuxCommandRequest,
    ) -> Result<LinuxProcessHandle, MobileLinuxError> {
        self.ensure_session_open()?;
        validate_request(&request)?;
        self.boot().await?;
        let mounts = self.merged_mounts(&request.mounts)?;
        self.apply_mounts(&mounts).await?;
        let (task_id, task) = self.create_task(
            "bg",
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Backgrounded,
        );
        let native_start = match self.native_spawn_background(&request, &mounts, true).await {
            Ok(start) if !start.process_id.trim().is_empty() => start,
            Ok(_) => {
                let error = MobileLinuxError::Io(
                    "native background spawn returned an empty process id".to_string(),
                );
                self.finish_task(
                    &task_id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
            Err(error) => {
                self.finish_task(
                    &task_id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };
        *task
            .native_handle
            .lock()
            .expect("ios-ish native handle mutex") = Some(native_start.process_id.clone());
        self.spawn_background_reader(task_id.clone(), native_start.process_id, task);
        Ok(LinuxProcessHandle {
            id: task_id,
            enforcement: native_start.enforcement,
        })
    }

    async fn kill(&self, handle: &LinuxProcessHandle) -> Result<(), MobileLinuxError> {
        let task = self
            .state
            .tasks
            .lock()
            .expect("ios-ish tasks mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("unknown task handle".to_string()))?;
        if task.terminal_emitted.load(Ordering::Acquire) {
            return Ok(());
        }
        let native_process_id = task
            .native_handle
            .lock()
            .expect("ios-ish native handle mutex")
            .clone()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest(
                    "task is not a killable background process".to_string(),
                )
            })?;
        if self.native_kill_background(&native_process_id).await? {
            task.cancel_requested.store(true, Ordering::Release);
            task.cancel_notify.notify_one();
        }
        let deadline = tokio::time::Instant::now() + BACKGROUND_REAP_BUDGET;
        while !task.terminal_emitted.load(Ordering::Acquire) {
            if tokio::time::Instant::now() >= deadline {
                return Err(MobileLinuxError::Io(format!(
                    "background task {} did not reap within 3 seconds",
                    handle.id
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    async fn open_pty(
        &self,
        request: PtyOpenRequest,
    ) -> Result<PtySessionHandle, MobileLinuxError> {
        self.ensure_session_open()?;
        validate_pty_request(&request)?;
        self.boot().await?;
        let mounts = self.merged_mounts(&request.mounts)?;
        self.apply_mounts(&mounts).await?;
        {
            let guard = self.state.pty.lock().expect("ios-ish pty mutex");
            if guard.is_some() {
                return Err(MobileLinuxError::InvalidRequest(
                    "only one PTY session is supported by the iSH bridge".to_string(),
                ));
            }
        }
        let session_id = match self.native_open_pty(&request, &mounts).await {
            Ok(session_id) => session_id,
            Err(error) => {
                self.emit_runtime_error(None, error.to_string());
                return Err(error);
            }
        };
        let task = self.create_task_with_id(
            session_id.clone(),
            display_command(&request.command, &request.args),
            MobileLinuxTaskStatus::Running,
        );
        if session_id.trim().is_empty() {
            self.finish_task(
                &session_id,
                &task,
                MobileLinuxTaskStatus::Failed,
                None,
                Some("native PTY open returned an empty session id".to_string()),
            );
            return Err(MobileLinuxError::Io(
                "native PTY open returned an empty session id".to_string(),
            ));
        }
        let control = Arc::new(PtyControl {
            task,
            open: AtomicBool::new(true),
            close_emitted: AtomicBool::new(false),
        });
        *self.state.pty.lock().expect("ios-ish pty mutex") =
            Some((session_id.clone(), control.clone()));
        self.spawn_pty_reader(session_id.clone(), control);
        Ok(PtySessionHandle { id: session_id })
    }

    async fn write_pty(
        &self,
        handle: &PtySessionHandle,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        let control = self.current_pty(&handle.id)?;
        if !control.open.load(Ordering::Acquire) {
            return Err(MobileLinuxError::InvalidRequest(
                "PTY session is already closed".to_string(),
            ));
        }
        self.native_write_pty(&handle.id, input).await
    }

    async fn resize_pty(
        &self,
        handle: &PtySessionHandle,
        size: PtySize,
    ) -> Result<(), MobileLinuxError> {
        let control = self.current_pty(&handle.id)?;
        if !control.open.load(Ordering::Acquire) {
            return Err(MobileLinuxError::InvalidRequest(
                "PTY session is already closed".to_string(),
            ));
        }
        self.native_resize_pty(&handle.id, size).await
    }

    async fn close_pty(&self, handle: &PtySessionHandle) -> Result<(), MobileLinuxError> {
        let control = self.current_pty(&handle.id)?;
        control.open.store(false, Ordering::Release);
        // Do not `?` here. Only one PTY may be open per runtime, so returning
        // early on a failed native close left `state.pty` occupied for the life
        // of the handle and every later `open_pty` was refused with "only one
        // PTY session is supported by the iSH bridge". The slot is our own
        // bookkeeping: release it either way and report the failure afterwards.
        let native_result = self.native_close_pty(&handle.id).await;
        self.finish_task(
            &handle.id,
            &control.task,
            MobileLinuxTaskStatus::Completed,
            Some(0),
            Some("PTY closed".to_string()),
        );
        self.emit_pty_closed_once(
            &handle.id,
            &control,
            Some(0),
            Some("PTY closed".to_string()),
        );
        self.clear_current_pty(&handle.id);
        native_result
    }

    async fn open_raw_stdio(
        &self,
        request: RawStdioOpenRequest,
    ) -> Result<RawStdioSessionHandle, MobileLinuxError> {
        self.ensure_session_open()?;
        let validation_request = LinuxCommandRequest {
            command: request.command.clone(),
            args: request.args.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            stdin: None,
            timeout_ms: None,
            network: request.network,
            resource_limits: request.resource_limits,
            mounts: request.mounts.clone(),
        };
        validate_request(&validation_request)?;
        self.boot().await?;
        let mounts = self.merged_mounts(&request.mounts)?;
        let (id, enforcement) = self.native_open_raw_stdio(&request, &mounts).await?;
        enforcement.ensure_for(request.network, request.resource_limits)?;
        let handle = RawStdioSessionHandle { id, enforcement };
        self.state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .insert(handle.id.clone(), handle.clone());
        Ok(handle)
    }

    async fn write_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        input: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        if !self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .contains_key(&handle.id)
        {
            return Err(MobileLinuxError::InvalidRequest(
                "unknown raw stdio session".to_string(),
            ));
        }
        let response = self
            .native_raw_stdio_call(
                RawStdioNativeOperation::Write,
                RawStdioRequestPayload {
                    session_id: handle.id.clone(),
                    data_base64: Some(encode_base64(&input)),
                    max_bytes: None,
                },
            )
            .await?;
        parse_native_ok(&response)
    }

    async fn read_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
        max_bytes: usize,
    ) -> Result<RawStdioReadResult, MobileLinuxError> {
        if !self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .contains_key(&handle.id)
        {
            return Err(MobileLinuxError::InvalidRequest(
                "unknown raw stdio session".to_string(),
            ));
        }
        let response = self
            .native_raw_stdio_call(
                RawStdioNativeOperation::Read,
                RawStdioRequestPayload {
                    session_id: handle.id.clone(),
                    data_base64: None,
                    max_bytes: Some(max_bytes.clamp(1, 1_048_576) as u32),
                },
            )
            .await?;
        parse_raw_stdio_read_response(&response)
    }

    async fn close_raw_stdio(
        &self,
        handle: &RawStdioSessionHandle,
    ) -> Result<(), MobileLinuxError> {
        if !self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .contains_key(&handle.id)
        {
            return Ok(());
        }
        let request = RawStdioRequestPayload {
            session_id: handle.id.clone(),
            data_base64: None,
            max_bytes: None,
        };
        let close_response = self
            .native_raw_stdio_call(RawStdioNativeOperation::Close, request)
            .await?;
        parse_native_ok(&close_response)?;

        let deadline = tokio::time::Instant::now() + BACKGROUND_REAP_BUDGET;
        let terminal_error = loop {
            let response = self
                .native_raw_stdio_call(
                    RawStdioNativeOperation::Read,
                    RawStdioRequestPayload {
                        session_id: handle.id.clone(),
                        data_base64: None,
                        max_bytes: Some(65_536),
                    },
                )
                .await?;
            let (closed, execution_error) = raw_stdio_close_progress(&response)?;
            if closed {
                break execution_error;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MobileLinuxError::Io(format!(
                    "raw stdio session {} did not reap within 3 seconds",
                    handle.id
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        tokio::time::sleep(Duration::from_millis(250)).await;
        let dispose_response = self
            .native_raw_stdio_call(
                RawStdioNativeOperation::Dispose,
                RawStdioRequestPayload {
                    session_id: handle.id.clone(),
                    data_base64: None,
                    max_bytes: None,
                },
            )
            .await?;
        parse_native_ok(&dispose_response)?;
        self.state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .remove(&handle.id);
        if let Some(error) = terminal_error {
            return Err(error);
        }
        Ok(())
    }

    async fn rootfs_status(&self) -> Result<RootfsStatus, MobileLinuxError> {
        self.refresh_rootfs_snapshot().await
    }

    async fn verify_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let mut status = self.refresh_rootfs_snapshot().await?;
        if !matches!(
            status.state,
            RootfsState::Ready | RootfsState::Missing | RootfsState::Unsupported
        ) {
            status.state = RootfsState::Corrupt;
        }
        Ok(status)
    }

    async fn repair_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let _lifecycle = self.state.lifecycle.lock().await;
        self.ensure_rootfs_mutable()?;
        self.invalidate_rootfs_snapshot();
        self.ensure_native_available()?;
        let config_json = self.native_config_json()?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::repair_rootfs_json(&config_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join repair_rootfs: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        let _ = parse_native_ok(&response)?;
        self.refresh_rootfs_snapshot().await
    }

    async fn reset_rootfs(&self) -> Result<RootfsStatus, MobileLinuxError> {
        let _lifecycle = self.state.lifecycle.lock().await;
        self.ensure_rootfs_mutable()?;
        self.invalidate_rootfs_snapshot();
        self.install_rootfs(true).await?;
        self.refresh_rootfs_snapshot().await
    }

    fn current_mounts(&self) -> Vec<MountSpec> {
        self.state
            .mounts
            .read()
            .expect("ios-ish mounts rwlock")
            .clone()
    }

    async fn configure_mounts(&self, mounts: Vec<MountSpec>) -> Result<(), MobileLinuxError> {
        self.ensure_session_open()?;
        let mut normalized = Vec::with_capacity(mounts.len().saturating_add(2));
        normalized.push(validate_mount(
            &self.state.config.default_workspace_mount(),
            &self.state.config,
        )?);
        normalized.push(self.state.config.persistent_home_mount());
        for mount in mounts {
            let mount = validate_mount(&mount, &self.state.config)?;
            if let Some(existing) = normalized
                .iter_mut()
                .find(|existing| existing.guest_path == mount.guest_path)
            {
                *existing = mount;
            } else {
                normalized.push(mount);
            }
        }
        *self.state.mounts.write().expect("ios-ish mounts rwlock") = normalized.clone();
        self.apply_mounts(&normalized).await
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
            .expect("ios-ish events mutex")
            .iter()
            .filter(|event| event.sequence > after)
            .take(limit.min(mobile_linux_api::mobile_linux::MAX_MOBILE_LINUX_EVENT_BATCH))
            .cloned()
            .collect())
    }

    async fn list_tasks(&self) -> Result<Vec<MobileLinuxTaskSnapshot>, MobileLinuxError> {
        let mut tasks: Vec<_> = self
            .state
            .tasks
            .lock()
            .expect("ios-ish tasks mutex")
            .values()
            .map(|task| {
                task.snapshot
                    .lock()
                    .expect("ios-ish task snapshot mutex")
                    .clone()
            })
            .collect();
        tasks.sort_by(|left, right| {
            left.started_at_ms
                .cmp(&right.started_at_ms)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
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
            .expect("ios-ish tasks mutex")
            .get(task_id)
            .map(|task| {
                task.snapshot
                    .lock()
                    .expect("ios-ish task snapshot mutex")
                    .clone()
            }))
    }
}
