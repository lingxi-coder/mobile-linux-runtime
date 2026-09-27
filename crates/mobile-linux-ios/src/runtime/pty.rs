use mobile_linux_api::{
    MobileLinuxError, MobileLinuxEventKind, MobileLinuxTaskStatus, MountSpec, PtyOpenRequest,
    PtySize,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::task::spawn_blocking;

use super::{
    decode_base64, encode_base64, native, parse_native_ok, parse_poll_events,
    parse_session_id_response, IosIshRuntime, NativePtyEventPayload, PtyClosePayload, PtyControl,
    PtyOpenPayload, PtyPollPayload, PtyResizePayload, PtyWritePayload, PTY_IDLE_POLL,
};

impl IosIshRuntime {
    pub(super) async fn native_open_pty(
        &self,
        request: &PtyOpenRequest,
        mounts: &[MountSpec],
    ) -> Result<String, MobileLinuxError> {
        self.ensure_session_open()?;
        let config_json = self.native_config_json()?;
        let payload = PtyOpenPayload::from_request(request, mounts);
        let json = serde_json::to_string(&payload)
            .map_err(|error| MobileLinuxError::Io(format!("serialize PTY request: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::pty_open_json(&config_json, &json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join open_pty_json: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_session_id_response(&response)
    }
}

impl IosIshRuntime {
    pub(super) async fn native_write_pty(
        &self,
        session_id: &str,
        data: Vec<u8>,
    ) -> Result<(), MobileLinuxError> {
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&PtyWritePayload {
            session_id: session_id.to_string(),
            data_base64: encode_base64(&data),
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize PTY write request: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::pty_write_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join write_pty: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_native_ok(&response)?;
        Ok(())
    }
}

impl IosIshRuntime {
    pub(super) async fn native_resize_pty(
        &self,
        session_id: &str,
        size: PtySize,
    ) -> Result<(), MobileLinuxError> {
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&PtyResizePayload {
            session_id: session_id.to_string(),
            cols: size.cols,
            rows: size.rows,
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize PTY resize request: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::pty_resize_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join resize_pty: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_native_ok(&response)?;
        Ok(())
    }
}

impl IosIshRuntime {
    pub(super) async fn native_close_pty(&self, session_id: &str) -> Result<(), MobileLinuxError> {
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&PtyClosePayload {
            session_id: session_id.to_string(),
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize PTY close request: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::pty_close_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join close_pty: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_native_ok(&response)?;
        Ok(())
    }
}

impl IosIshRuntime {
    pub(super) async fn native_poll_pty(
        &self,
        after_sequence: Option<u64>,
        limit: u32,
    ) -> Result<Vec<NativePtyEventPayload>, MobileLinuxError> {
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&PtyPollPayload {
            after_sequence,
            limit: Some(limit),
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize PTY poll request: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::poll_output_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join poll_output_json: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_poll_events(&response)
    }
}

impl IosIshRuntime {
    pub(super) fn emit_pty_closed_once(
        &self,
        session_id: &str,
        control: &PtyControl,
        exit_code: Option<i32>,
        detail: Option<String>,
    ) {
        if control.close_emitted.swap(true, Ordering::AcqRel) {
            return;
        }
        self.emit(
            Some(session_id.to_string()),
            MobileLinuxEventKind::PtyClosed {
                session_id: session_id.to_string(),
                exit_code,
                detail,
            },
        );
    }
}

impl IosIshRuntime {
    pub(super) fn current_pty(
        &self,
        session_id: &str,
    ) -> Result<Arc<PtyControl>, MobileLinuxError> {
        let guard = self.state.pty.lock().expect("ios-ish pty mutex");
        let (id, control) = guard.as_ref().ok_or_else(|| {
            MobileLinuxError::InvalidRequest("no PTY session is open".to_string())
        })?;
        if id != session_id {
            return Err(MobileLinuxError::InvalidRequest(
                "unknown PTY session handle".to_string(),
            ));
        }
        Ok(control.clone())
    }
}

impl IosIshRuntime {
    pub(super) fn clear_current_pty(&self, session_id: &str) {
        let mut guard = self.state.pty.lock().expect("ios-ish pty mutex");
        if guard.as_ref().is_some_and(|(id, _)| id == session_id) {
            *guard = None;
        }
    }
}

impl IosIshRuntime {
    pub(super) fn spawn_pty_reader(&self, session_id: String, control: Arc<PtyControl>) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut after_sequence = None;
            loop {
                if !control.open.load(Ordering::Acquire) {
                    break;
                }
                match runtime
                    .native_poll_pty(
                        after_sequence,
                        mobile_linux_api::mobile_linux::MAX_MOBILE_LINUX_EVENT_BATCH as u32,
                    )
                    .await
                {
                    Ok(events) if events.is_empty() => {
                        tokio::time::sleep(PTY_IDLE_POLL).await;
                    }
                    Ok(events) => {
                        for event in events {
                            after_sequence = Some(event.sequence);
                            if event.session_id != session_id {
                                // Skip silently. A straggler event from a
                                // previous session (the journal outlives a
                                // shell; reopen is a supported flow now) is
                                // not a fault of THIS session — but this used
                                // to emit a RuntimeError tagged with the NEW
                                // session id, which the terminal maps
                                // straight to a failed state: one stale event
                                // killed a healthy restarted shell.
                                continue;
                            }
                            match event.kind.as_str() {
                                "pty_output" => {
                                    let data = event
                                        .data_base64
                                        .as_deref()
                                        .map(decode_base64)
                                        .transpose()
                                        .unwrap_or_default()
                                        .unwrap_or_default();
                                    runtime.emit(
                                        Some(session_id.clone()),
                                        MobileLinuxEventKind::PtyOutput {
                                            session_id: session_id.clone(),
                                            data,
                                        },
                                    );
                                }
                                "pty_closed" => {
                                    control.open.store(false, Ordering::Release);
                                    // A shell SELF-exit (user typed `exit`, or the
                                    // guest process died) rides in with the real
                                    // wait-status-decoded code from the bridge's
                                    // ISHProcessExited observer; a bridge-initiated
                                    // close carries none and stays code 0.
                                    let exit_code = Some(event.exit_code.unwrap_or(0));
                                    runtime.finish_task(
                                        &session_id,
                                        &control.task,
                                        MobileLinuxTaskStatus::Completed,
                                        exit_code,
                                        event.detail.clone(),
                                    );
                                    runtime.emit_pty_closed_once(
                                        &session_id,
                                        &control,
                                        exit_code,
                                        event.detail,
                                    );
                                    runtime.clear_current_pty(&session_id);
                                    return;
                                }
                                other => runtime.emit_runtime_error(
                                    Some(session_id.clone()),
                                    format!("unknown PTY event kind: {other}"),
                                ),
                            }
                        }
                    }
                    Err(error) => {
                        control.open.store(false, Ordering::Release);
                        runtime.emit_runtime_error(Some(session_id.clone()), error.to_string());
                        runtime.finish_task(
                            &session_id,
                            &control.task,
                            MobileLinuxTaskStatus::Failed,
                            None,
                            Some(error.to_string()),
                        );
                        // Release the NATIVE side before announcing the death.
                        // The Swift bridge frees its one-interactive-PTY slot
                        // only through a close call; without this, the exit
                        // event below reaches the terminal, the user taps
                        // 重新启动 shell, and every reopen is refused with
                        // "only one interactive PTY session is supported per
                        // managed root" until the app is relaunched. Closing
                        // before the emit also keeps a prompt restart from
                        // racing this task for the slot. Best-effort: if the
                        // pipeline is so broken that close itself fails, the
                        // Swift bridge now releases the slot regardless.
                        let _ = runtime.native_close_pty(&session_id).await;
                        runtime.emit_pty_closed_once(
                            &session_id,
                            &control,
                            None,
                            Some(error.to_string()),
                        );
                        runtime.clear_current_pty(&session_id);
                        break;
                    }
                }
            }
        });
    }
}
