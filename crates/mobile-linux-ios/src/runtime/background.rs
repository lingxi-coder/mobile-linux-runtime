use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MobileLinuxEventKind, MobileLinuxTaskStatus, MountSpec,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::task::spawn_blocking;

use super::{
    background_terminal_state, decode_base64, native, parse_background_events,
    parse_background_kill_response, parse_process_id_response, BackgroundPollPayload,
    BackgroundProcessPayload, IosIshRuntime, NativeBackgroundEventPayload, NativeProcessStart,
    RunRequestPayload, TaskControl, BACKGROUND_IDLE_POLL,
};

impl IosIshRuntime {
    pub(super) async fn native_spawn_background(
        &self,
        request: &LinuxCommandRequest,
        mounts: &[MountSpec],
        include_default_mounts: bool,
    ) -> Result<NativeProcessStart, MobileLinuxError> {
        self.ensure_session_open()?;
        #[cfg(test)]
        if let Some(transport) = &self.state.test_transport {
            return transport
                .spawn(request, mounts, include_default_mounts)
                .await;
        }
        let config_json = self.native_config_json()?;
        let payload = RunRequestPayload::from_request(request, mounts, include_default_mounts);
        let request_json = serde_json::to_string(&payload).map_err(|error| {
            MobileLinuxError::Io(format!("serialize background request: {error}"))
        })?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::spawn_background_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join spawn_background: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_process_id_response(&response)
    }
}

impl IosIshRuntime {
    pub(super) async fn native_kill_background(
        &self,
        process_id: &str,
    ) -> Result<bool, MobileLinuxError> {
        #[cfg(test)]
        if let Some(transport) = &self.state.test_transport {
            return transport.kill(process_id);
        }
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&BackgroundProcessPayload {
            process_id: process_id.to_string(),
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize background kill: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::kill_background_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join kill_background: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_background_kill_response(&response)
    }
}

impl IosIshRuntime {
    pub(super) async fn native_poll_background(
        &self,
        process_id: &str,
        after_sequence: Option<u64>,
        limit: u32,
    ) -> Result<Vec<NativeBackgroundEventPayload>, MobileLinuxError> {
        #[cfg(test)]
        if let Some(transport) = &self.state.test_transport {
            return transport.poll(process_id, after_sequence);
        }
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&BackgroundPollPayload {
            process_id: process_id.to_string(),
            after_sequence,
            limit: Some(limit),
        })
        .map_err(|error| MobileLinuxError::Io(format!("serialize background poll: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::poll_background_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join poll_background: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_background_events(&response)
    }
}

impl IosIshRuntime {
    pub(super) fn spawn_background_reader(
        &self,
        task_id: String,
        native_process_id: String,
        task: Arc<TaskControl>,
    ) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut last_sequence = None;
            loop {
                match runtime
                    .native_poll_background(
                        &native_process_id,
                        last_sequence,
                        mobile_linux_api::MAX_MOBILE_LINUX_EVENT_BATCH as u32,
                    )
                    .await
                {
                    Ok(events) if events.is_empty() => {
                        if task.terminal_emitted.load(Ordering::Acquire) {
                            break;
                        }
                        tokio::time::sleep(BACKGROUND_IDLE_POLL).await;
                    }
                    Ok(events) => {
                        let mut terminal = false;
                        for event in events {
                            last_sequence = Some(
                                last_sequence
                                    .map_or(event.sequence, |current| current.max(event.sequence)),
                            );
                            match event.kind.as_str() {
                                "stdout_line" => runtime.emit(
                                    Some(task_id.clone()),
                                    MobileLinuxEventKind::StdoutLine {
                                        line: event.line.unwrap_or_default(),
                                    },
                                ),
                                "stderr_chunk" => match event.data_base64.as_deref() {
                                    Some(encoded) => match decode_base64(encoded) {
                                        Ok(chunk) => runtime.emit(
                                            Some(task_id.clone()),
                                            MobileLinuxEventKind::StderrChunk { chunk },
                                        ),
                                        Err(error) => runtime.emit_runtime_error(
                                            Some(task_id.clone()),
                                            error.to_string(),
                                        ),
                                    },
                                    None => runtime.emit_runtime_error(
                                        Some(task_id.clone()),
                                        "native stderr event omitted data_base64",
                                    ),
                                },
                                "process_exited" => {
                                    let (status, exit_code, detail) = background_terminal_state(
                                        task.cancel_requested.load(Ordering::Acquire),
                                        &event,
                                    );
                                    runtime.finish_task(&task_id, &task, status, exit_code, detail);
                                    terminal = true;
                                }
                                other => runtime.emit_runtime_error(
                                    Some(task_id.clone()),
                                    format!("unknown native background event kind: {other}"),
                                ),
                            }
                        }
                        if terminal {
                            break;
                        }
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
                        break;
                    }
                }
            }
        });
    }
}
