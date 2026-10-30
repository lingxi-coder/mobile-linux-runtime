use mobile_linux_api::{
    allocate_raw_stdio_session_id, LinuxEnforcementReceipt, MobileLinuxError, MobileLinuxEventKind,
    MountSpec, RawStdioOpenRequest, RawStdioReadResult, RawStdioSessionHandle, RawStdioStopResult,
    RawStdioTreeStatus,
};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::task::spawn_blocking;

use super::{
    bridge::{raw_stdio_dispose_confirmed, raw_stdio_open_not_started},
    native, parse_native_ok, parse_raw_stdio_open_response, raw_stdio_close_progress,
    IosIshRuntime, RawStdioControl, RawStdioNativeOperation, RawStdioOpenPayload,
    RawStdioRequestPayload, RuntimeState, BACKGROUND_REAP_BUDGET,
};

const MAX_TRACKED_RAW_OWNERS: usize = 128;

/// Own raw native operations independently of a caller's Tokio runtime.
/// Both the runtime thread and its blocking pool are process-wide and bounded.
pub(super) fn raw_stdio_executor() -> Result<&'static tokio::runtime::Handle, MobileLinuxError> {
    static EXECUTOR: OnceLock<Result<tokio::runtime::Handle, String>> = OnceLock::new();
    EXECUTOR
        .get_or_init(|| {
            let (reply, response) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("ios-raw-stdio-owner".into())
                .spawn(move || {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .max_blocking_threads(4)
                        .build()
                    {
                        Ok(runtime) => {
                            if reply.send(Ok(runtime.handle().clone())).is_ok() {
                                runtime.block_on(std::future::pending::<()>());
                            }
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error.to_string()));
                        }
                    }
                })
                .map_err(|error| format!("start raw stdio owner thread: {error}"))?;
            response
                .recv()
                .map_err(|error| format!("start raw stdio owner runtime: {error}"))?
        })
        .as_ref()
        .map_err(|error| MobileLinuxError::Io(error.clone()))
}

// The process registry intentionally holds only Weak session references. Keep
// unresolved raw owners alive separately so a dropped runtime cannot lose a
// native id that still needs a confirmed close or an explicit no-start receipt.
fn raw_stdio_owners() -> &'static Mutex<HashMap<(usize, String), Arc<RuntimeState>>> {
    static OWNERS: OnceLock<Mutex<HashMap<(usize, String), Arc<RuntimeState>>>> = OnceLock::new();
    OWNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl IosIshRuntime {
    pub(super) fn accept_raw_stdio(&self, id: &str) -> Result<(), MobileLinuxError> {
        let sessions = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex");
        let control = sessions.get(id).ok_or_else(|| {
            MobileLinuxError::Io(format!("raw stdio session {id} disappeared during handoff"))
        })?;
        if control.disposed.load(Ordering::Acquire) || *control.stop_requested.borrow() {
            return Err(MobileLinuxError::Io(format!(
                "raw stdio session {id} was stopped during handoff"
            )));
        }
        control.uncertain.store(false, Ordering::Release);
        Ok(())
    }

    pub(super) fn mark_raw_stdio_uncertain(&self, id: &str) {
        if let Some(control) = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .get(id)
        {
            control.uncertain.store(true, Ordering::Release);
        }
    }

    pub(super) fn prepare_raw_stdio_open(
        &self,
        request: &RawStdioOpenRequest,
        mounts: &[MountSpec],
        session_id: &str,
    ) -> Result<(String, String), MobileLinuxError> {
        self.ensure_session_open()?;
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&RawStdioOpenPayload::from_request(
            request, mounts, session_id,
        ))
        .map_err(|error| MobileLinuxError::Io(format!("serialize raw stdio open: {error}")))?;
        Ok((config_json, request_json))
    }

    pub(super) async fn native_open_raw_stdio(
        &self,
        config_json: String,
        request_json: String,
    ) -> Result<String, MobileLinuxError> {
        #[cfg(test)]
        if let Some(transport) = &self.state.test_transport {
            return transport.raw_open(&request_json).await;
        }
        let native_lock = self.state.native_lock.clone();
        spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::raw_stdio_open_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join raw stdio open: {error}")))?
        .map_err(MobileLinuxError::Io)
    }

    /// Runs under the lifecycle lock in a task independent of the caller.
    pub(super) async fn open_raw_stdio_owned(
        &self,
        request: RawStdioOpenRequest,
    ) -> Result<RawStdioSessionHandle, MobileLinuxError> {
        self.ensure_session_open()?;
        // Each admitted native session reserves its own cleanup capacity.
        let owner_permit = raw_stdio_stop_capacity().try_acquire_owned().map_err(|_| {
            MobileLinuxError::ResourceLimitExceeded("raw stdio owner capacity exhausted".into())
        })?;
        let mounts = self.merged_mounts(&request.mounts)?;
        let id = allocate_raw_stdio_session_id()?;
        let (config, payload) = self.prepare_raw_stdio_open(&request, &mounts, &id)?;
        let provisional = RawStdioSessionHandle {
            id: id.clone(),
            enforcement: LinuxEnforcementReceipt::default(),
        };
        let control = Arc::new(RawStdioControl::new(provisional.clone(), owner_permit));
        {
            let mut sessions = self
                .state
                .raw_stdio
                .lock()
                .expect("ios-ish raw stdio mutex");
            if sessions.values().any(|session| {
                session.uncertain.load(Ordering::Acquire) || *session.stop_requested.borrow()
            }) {
                return Err(MobileLinuxError::Io(
                    "a previous raw stdio cleanup remains unconfirmed; retry it before opening another".into(),
                ));
            }
            if sessions.contains_key(&id) {
                return Err(MobileLinuxError::Io(
                    "duplicate raw stdio session id".into(),
                ));
            }
            let mut owners = raw_stdio_owners().lock().expect("raw stdio owner mutex");
            if owners.len() >= MAX_TRACKED_RAW_OWNERS {
                return Err(MobileLinuxError::Io(
                    "raw stdio session tracking capacity exhausted".into(),
                ));
            }
            sessions.insert(id.clone(), control.clone());
            owners.insert(
                (Arc::as_ptr(&self.state) as usize, id.clone()),
                self.state.clone(),
            );
        }

        let response = self.native_open_raw_stdio(config, payload).await;
        if let Ok(json) = &response {
            if raw_stdio_open_not_started(json) {
                self.remove_raw_stdio_control(&id, &control);
                return parse_raw_stdio_open_response(json).map(|_| provisional);
            }
            // A mismatched id has uncertain ownership. Keep both the reserved
            // id and any new returned id available for shutdown/reconciliation;
            // do not terminate an unrelated existing session by guessing.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
                if let Some(other_id) = value.get("session_id").and_then(|v| v.as_str()) {
                    if !other_id.is_empty() && other_id != id {
                        *control
                            .unexpected_id
                            .lock()
                            .expect("raw stdio unexpected id") = Some(other_id.to_owned());
                        control.uncertain.store(true, Ordering::Release);
                        let original = parse_raw_stdio_open_response(json).err();
                        return Err(MobileLinuxError::Io(format!(
                            "native raw stdio open returned unexpected session id {other_id}; \
                             ownership of reserved session {id} is unknown; native result: {original:?}"
                        )));
                    }
                }
            }
        }
        let parsed = response.and_then(|json| parse_raw_stdio_open_response(&json));
        let result = parsed.and_then(|(actual_id, enforcement)| {
            if actual_id != id {
                return Err(MobileLinuxError::Io(
                    "native raw stdio session id mismatch".into(),
                ));
            }
            enforcement.ensure_for(request.network, request.resource_limits)?;
            Ok(RawStdioSessionHandle {
                id: id.clone(),
                enforcement,
            })
        });
        match result {
            Ok(handle) => Ok(handle),
            Err(error) => match self.close_raw_stdio_owned(&provisional).await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(attach_cleanup_error(
                    error,
                    &id,
                    &cleanup,
                    control.disposed.load(Ordering::Acquire),
                )),
            },
        }
    }

    fn remove_raw_stdio_control(&self, id: &str, control: &Arc<RawStdioControl>) {
        let mut sessions = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex");
        if sessions
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, control))
        {
            sessions.remove(id);
            raw_stdio_owners()
                .lock()
                .expect("raw stdio owner mutex")
                .remove(&(Arc::as_ptr(&self.state) as usize, id.to_owned()));
        }
    }

    pub(super) fn report_raw_cleanup_failure(&self, id: &str, error: &MobileLinuxError) {
        self.emit(
            None,
            MobileLinuxEventKind::RuntimeError {
                detail: format!("raw stdio session {id} cleanup unconfirmed: {error}"),
            },
        );
    }

    pub(super) async fn close_raw_stdio_owned(
        &self,
        handle: &RawStdioSessionHandle,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest(
                    "unknown raw stdio session; cleanup is not confirmed".into(),
                )
            })?;
        // Wake a read whose caller stopped polling after delivery; close must
        // be able to take the bytes back before waiting for the read gate.
        control.stop_requested.send_replace(true);
        let _read_gate = control.read_gate.lock().await;
        let _gate = control.close_gate.lock().await;
        if control
            .stop_result
            .lock()
            .expect("raw stop result")
            .is_some()
        {
            return Err(MobileLinuxError::Io(
                "raw stdio cleanup remains unverified".into(),
            ));
        }
        if control.disposed.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(other_id) = control
            .unexpected_id
            .lock()
            .expect("raw stdio unexpected id")
            .clone()
        {
            return Err(MobileLinuxError::Io(format!(
                "native raw stdio session {} has unverified response id {other_id}; cleanup is unconfirmed", handle.id
            )));
        }
        let result = self.finish_raw_stdio_close(handle, &control).await;
        if result.is_err() && !control.disposed.load(Ordering::Acquire) {
            control.uncertain.store(true, Ordering::Release);
        }
        result
    }

    async fn finish_raw_stdio_close(
        &self,
        handle: &RawStdioSessionHandle,
        control: &Arc<RawStdioControl>,
    ) -> Result<(), MobileLinuxError> {
        let response = self
            .native_raw_stdio_call(
                RawStdioNativeOperation::Close,
                RawStdioRequestPayload {
                    session_id: handle.id.clone(),
                    data_base64: None,
                    max_bytes: None,
                },
            )
            .await?;
        parse_native_ok(&response)?;
        let receipt: serde_json::Value = serde_json::from_str(&response)
            .map_err(|error| MobileLinuxError::Io(format!("parse raw stdio close: {error}")))?;
        if receipt.get("already_closed").and_then(|v| v.as_bool()) == Some(true) {
            return Err(MobileLinuxError::Io(format!(
                "native raw stdio session {} is absent; cleanup is unconfirmed",
                handle.id
            )));
        }
        if receipt
            .get("termination_requested")
            .and_then(|v| v.as_bool())
            != Some(true)
            && receipt.get("closed").and_then(|v| v.as_bool()) != Some(true)
        {
            return Err(MobileLinuxError::Io(format!(
                "native raw stdio close did not acknowledge termination for {}",
                handle.id
            )));
        }
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
        let response = self
            .native_raw_stdio_call(
                RawStdioNativeOperation::Dispose,
                RawStdioRequestPayload {
                    session_id: handle.id.clone(),
                    data_base64: None,
                    max_bytes: None,
                },
            )
            .await?;
        raw_stdio_dispose_confirmed(&response)?;
        control.disposed.store(true, Ordering::Release);
        self.remove_raw_stdio_control(&handle.id, &control);
        if let Some(error) = terminal_error {
            return Err(error);
        }
        Ok(())
    }
}

fn split_unaccepted_read(
    result: Result<RawStdioReadResult, MobileLinuxError>,
    limit: usize,
) -> (
    Result<RawStdioReadResult, MobileLinuxError>,
    Option<Result<RawStdioReadResult, MobileLinuxError>>,
) {
    let Ok(mut pending) = result else {
        return (result, None);
    };
    let stdout_count = pending.stdout.len().min(limit);
    let stderr_count = pending.stderr.len().min(limit - stdout_count);
    let stdout = pending.stdout.drain(..stdout_count).collect();
    let stderr = pending.stderr.drain(..stderr_count).collect();
    let remaining = !pending.stdout.is_empty() || !pending.stderr.is_empty();
    let delivered = RawStdioReadResult {
        stdout,
        stderr,
        closed: pending.closed && !remaining,
        exit_code: pending.exit_code,
    };
    (Ok(delivered), remaining.then_some(Ok(pending)))
}

pub(super) fn raw_stdio_read_capacity() -> Arc<tokio::sync::Semaphore> {
    static CAPACITY: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    CAPACITY
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(32)))
        .clone()
}

pub(super) fn raw_stdio_stop_capacity() -> Arc<tokio::sync::Semaphore> {
    static CAPACITY: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    CAPACITY
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(32)))
        .clone()
}

pub(super) fn bounded_stop_result(
    mut result: RawStdioStopResult,
    limit: usize,
) -> RawStdioStopResult {
    if result.stdout.len() > limit {
        result.stdout.truncate(limit);
        result.stdout_truncated = true;
    }
    if result.stderr.len() > limit {
        result.stderr.truncate(limit);
        result.stderr_truncated = true;
    }
    result
}

fn append_stop_bytes(target: &mut Vec<u8>, bytes: &[u8], truncated: &mut bool) {
    const LIMIT: usize = 1_048_576;
    let keep = LIMIT.saturating_sub(target.len()).min(bytes.len());
    target.extend_from_slice(&bytes[..keep]);
    *truncated |= keep < bytes.len();
}

fn note_stop_failure(result: &mut RawStdioStopResult, error: MobileLinuxError) {
    if let RawStdioTreeStatus::Unverified(detail) = &mut result.cleanup {
        detail.push_str("; ");
        detail.push_str(&error.to_string());
    }
    if result.failure.is_none() {
        result.failure = Some(error);
    }
}

impl IosIshRuntime {
    pub(super) async fn read_raw_stdio_owned(
        &self,
        handle: &RawStdioSessionHandle,
        max_bytes: usize,
    ) -> Result<RawStdioReadResult, MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| MobileLinuxError::InvalidRequest("unknown raw stdio session".into()))?;
        if max_bytes == 0 {
            let _gate = control.read_gate.lock().await;
            let same = self
                .state
                .raw_stdio
                .lock()
                .expect("ios-ish raw stdio mutex")
                .get(&handle.id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &control));
            if !same || *control.stop_requested.borrow() || control.disposed.load(Ordering::Acquire)
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "raw stdio session is stopping or closed".into(),
                ));
            }
            // A zero-byte read cannot observe native closure and must not
            // consume any queued output just to discover it.
            return Ok(RawStdioReadResult {
                stdout: Vec::new(),
                stderr: Vec::new(),
                closed: false,
                exit_code: None,
            });
        }
        let permit = raw_stdio_read_capacity().try_acquire_owned().map_err(|_| {
            MobileLinuxError::ResourceLimitExceeded("raw stdio read capacity exhausted".into())
        })?;
        let (reply, response) = tokio::sync::oneshot::channel();
        let (accepted, accepted_response) = tokio::sync::oneshot::channel::<()>();
        let runtime = self.clone();
        let handle = handle.clone();
        let limit = max_bytes.clamp(1, 1_048_576);
        raw_stdio_executor()?.spawn(async move {
            let _permit = permit;
            let mut stopped = control.stop_requested.subscribe();
            let _gate = control.read_gate.lock().await;
            if reply.is_closed() {
                return;
            }
            let same = runtime
                .state
                .raw_stdio
                .lock()
                .expect("ios-ish raw stdio mutex")
                .get(&handle.id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &control));
            if !same
                || *stopped.borrow()
                || control.disposed.load(Ordering::Acquire)
                || control
                    .stop_result
                    .lock()
                    .expect("raw stop result")
                    .is_some()
            {
                let _ = reply.send(Arc::new(Mutex::new(Some(Err(
                    MobileLinuxError::InvalidRequest(
                        "raw stdio session is stopping or closed".into(),
                    ),
                )))));
                return;
            }
            let replay = control
                .unaccepted_reads
                .lock()
                .expect("raw read replay")
                .pop_front();
            let original = match replay {
                Some(result) => result,
                None => {
                    let observation = runtime
                        .native_raw_stdio_call(
                            RawStdioNativeOperation::Read,
                            RawStdioRequestPayload {
                                session_id: handle.id.clone(),
                                data_base64: None,
                                max_bytes: Some(limit as u32),
                            },
                        )
                        .await
                        .and_then(|response| {
                            super::bridge::parse_raw_stdio_read_observation(&response)
                        });
                    match observation {
                        Ok(observation)
                            if observation.failure.is_some()
                                && (!observation.read.stdout.is_empty()
                                    || !observation.read.stderr.is_empty()) =>
                        {
                            let mut result = observation.read;
                            result.closed = false;
                            control
                                .unaccepted_reads
                                .lock()
                                .expect("raw read replay")
                                .push_back(Err(observation.failure.expect("guarded failure")));
                            Ok(result)
                        }
                        Ok(observation) => match observation.failure {
                            Some(error) => Err(error),
                            None => Ok(observation.read),
                        },
                        Err(error) => Err(error),
                    }
                }
            };
            let rollback = original.clone();
            let (delivered, remainder) = split_unaccepted_read(original, limit);
            // Sending a oneshot is not acceptance: the caller may stay alive
            // without polling it. Stop may atomically revoke the slot and
            // replay the bytes; a later caller cannot receive them twice.
            let delivery = Arc::new(Mutex::new(Some(delivered)));
            let sent = reply.send(delivery.clone()).is_ok();
            if sent {
                tokio::select! {
                    _ = accepted_response => {},
                    _ = stopped.wait_for(|requested| *requested) => {},
                }
            }
            let accepted = delivery.lock().expect("raw read delivery").take().is_none();
            let pending = if accepted { remainder } else { Some(rollback) };
            if let Some(pending) = pending {
                control
                    .unaccepted_reads
                    .lock()
                    .expect("raw read replay")
                    .push_front(pending);
            }
        });
        let delivery = response.await.map_err(|error| {
            MobileLinuxError::Io(format!("raw stdio read owner failed: {error}"))
        })?;
        let result = delivery
            .lock()
            .expect("raw read delivery")
            .take()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest("raw stdio read was collected by stop".into())
            })?;
        let _ = accepted.send(());
        result
    }

    pub(super) async fn stop_raw_stdio_owned(
        &self,
        handle: &RawStdioSessionHandle,
        limit: usize,
    ) -> Result<RawStdioStopResult, MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest(
                    "unknown raw stdio session; cleanup is unverified".into(),
                )
            })?;
        let _read_gate = control.read_gate.lock().await;
        let _close_gate = control.close_gate.lock().await;
        let same = self
            .state
            .raw_stdio
            .lock()
            .expect("ios-ish raw stdio mutex")
            .get(&handle.id)
            .is_some_and(|entry| Arc::ptr_eq(entry, &control));
        if !same || control.disposed.load(Ordering::Acquire) {
            return Err(MobileLinuxError::InvalidRequest(
                "raw stdio owner changed; cleanup is unverified".into(),
            ));
        }
        if let Some(result) = control.stop_result.lock().expect("raw stop result").clone() {
            return Ok(bounded_stop_result(result, limit));
        }
        let mut result = RawStdioStopResult {
            session_id: handle.id.clone(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            // These become true only when native readers report zero-byte
            // reads and their corresponding retained queues are drained.
            stdout_eof: false,
            stderr_eof: false,
            exit_code: None,
            termination_requested: None,
            cleanup: RawStdioTreeStatus::Unverified(
                "iOS raw stdio has no identity-bound process-tree exit receipt".into(),
            ),
            failure: None,
        };
        let pending: Vec<_> = control
            .unaccepted_reads
            .lock()
            .expect("raw read replay")
            .drain(..)
            .collect();
        for item in pending {
            match item {
                Ok(read) => collect_stop_read(&mut result, read),
                Err(error) => note_stop_failure(&mut result, error),
            }
        }
        let close = self
            .native_raw_stdio_call(
                RawStdioNativeOperation::Close,
                RawStdioRequestPayload {
                    session_id: handle.id.clone(),
                    data_base64: None,
                    max_bytes: None,
                },
            )
            .await;
        let mut can_wait = false;
        match close {
            Ok(response) => {
                let parsed = parse_native_ok(&response).and_then(|()| {
                    serde_json::from_str::<serde_json::Value>(&response).map_err(|error| {
                        MobileLinuxError::Io(format!("parse raw stdio close: {error}"))
                    })
                });
                match parsed {
                    Ok(value)
                        if value.get("already_closed").and_then(|v| v.as_bool()) != Some(true) =>
                    {
                        result.termination_requested =
                            value.get("termination_requested").and_then(|v| v.as_bool());
                        can_wait = result.termination_requested == Some(true)
                            || value.get("closed").and_then(|v| v.as_bool()) == Some(true);
                        if !can_wait {
                            note_stop_failure(
                                &mut result,
                                MobileLinuxError::Io(
                                    "native raw stop did not acknowledge termination".into(),
                                ),
                            );
                        }
                    }
                    Ok(_) => note_stop_failure(
                        &mut result,
                        MobileLinuxError::Io(
                            "native raw session is absent; cleanup is unverified".into(),
                        ),
                    ),
                    Err(error) => note_stop_failure(&mut result, error),
                }
            }
            Err(error) => note_stop_failure(&mut result, error),
        }
        let deadline = tokio::time::Instant::now() + BACKGROUND_REAP_BUDGET;
        loop {
            let read = self
                .native_raw_stdio_call(
                    RawStdioNativeOperation::Read,
                    RawStdioRequestPayload {
                        session_id: handle.id.clone(),
                        data_base64: None,
                        max_bytes: Some(65_536),
                    },
                )
                .await
                .and_then(|response| super::bridge::parse_raw_stdio_read_observation(&response));
            let observation = match read {
                Ok(value) => value,
                Err(error) => {
                    note_stop_failure(&mut result, error);
                    break;
                }
            };
            let closed = observation.read.closed;
            result.stdout_eof |= observation.stdout_eof;
            result.stderr_eof |= observation.stderr_eof;
            result.stdout_truncated |= observation.stdout_truncated;
            result.stderr_truncated |= observation.stderr_truncated;
            collect_stop_read(&mut result, observation.read);
            if let Some(error) = observation.failure {
                note_stop_failure(&mut result, error);
                break;
            }
            if closed || !can_wait {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                note_stop_failure(
                    &mut result,
                    MobileLinuxError::Io(
                        "raw stdio stop did not reach terminal output within 3 seconds".into(),
                    ),
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        control.uncertain.store(true, Ordering::Release);
        *control.stop_result.lock().expect("raw stop result") = Some(result.clone());
        Ok(bounded_stop_result(result, limit))
    }
}

fn collect_stop_read(result: &mut RawStdioStopResult, read: RawStdioReadResult) {
    append_stop_bytes(
        &mut result.stdout,
        &read.stdout,
        &mut result.stdout_truncated,
    );
    append_stop_bytes(
        &mut result.stderr,
        &read.stderr,
        &mut result.stderr_truncated,
    );
    // Native iSH returns shell-style 128+signal for signalled exits; values
    // above 127 do not distinguish a signal from a real exit code.
    if let Some(code) = read.exit_code.filter(|code| (0..128).contains(code)) {
        result.exit_code = Some(code);
    }
}

fn attach_cleanup_error(
    error: MobileLinuxError,
    id: &str,
    cleanup: &MobileLinuxError,
    disposed: bool,
) -> MobileLinuxError {
    let suffix = if disposed {
        format!("; raw stdio session {id} was disposed with terminal error: {cleanup}")
    } else {
        format!("; raw stdio session {id} cleanup unconfirmed: {cleanup}")
    };
    match error {
        MobileLinuxError::NetworkPolicyUnavailable(message) => {
            MobileLinuxError::NetworkPolicyUnavailable(message + &suffix)
        }
        MobileLinuxError::ResourceLimitExceeded(message) => {
            MobileLinuxError::ResourceLimitExceeded(message + &suffix)
        }
        other => MobileLinuxError::Io(format!("{other}{suffix}")),
    }
}

impl IosIshRuntime {
    pub(super) async fn native_raw_stdio_call(
        &self,
        operation: RawStdioNativeOperation,
        payload: RawStdioRequestPayload,
    ) -> Result<String, MobileLinuxError> {
        #[cfg(test)]
        if let Some(transport) = &self.state.test_transport {
            return transport.raw_call(operation, &payload).await;
        }
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&payload).map_err(|error| {
            MobileLinuxError::Io(format!("serialize raw stdio request: {error}"))
        })?;
        let native_lock = self.state.native_lock.clone();
        spawn_blocking(move || {
            let _guard = (!matches!(operation, RawStdioNativeOperation::Write))
                .then(|| native_lock.lock().expect("ios-ish native lock"));
            match operation {
                RawStdioNativeOperation::Write => {
                    native::raw_stdio_write_json(&config_json, &request_json)
                }
                RawStdioNativeOperation::Read => {
                    native::raw_stdio_read_json(&config_json, &request_json)
                }
                RawStdioNativeOperation::Close => {
                    native::raw_stdio_close_json(&config_json, &request_json)
                }
                RawStdioNativeOperation::Dispose => {
                    native::raw_stdio_dispose_json(&config_json, &request_json)
                }
            }
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join raw stdio request: {error}")))?
        .map_err(MobileLinuxError::Io)
    }
}
