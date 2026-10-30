use mobile_linux_api::{
    allocate_raw_stdio_session_id, LinuxCommandRequest, MobileLinuxError, MobileLinuxEventKind,
    MobileLinuxTaskStatus, RawStdioOpenRequest, RawStdioSessionHandle, RawStdioStopResult,
    RawStdioTreeStatus,
};
use nix::sys::signal::Signal;
use std::collections::VecDeque;
use std::fs;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};

use super::{
    check_snapshot_cancelled, snapshot_read_only_mounts, terminate_group, wait_for_child,
    AndroidProotRuntime, ChildWaitOutcome, ForegroundMountMode, RawStdioBuffers, RawStdioControl,
    RawStdioWrite, SnapshotCleanup, SpawnedChild, TaskControl, MAX_RAW_STDIO_BUFFER_BYTES,
    REAP_BUDGET,
};

impl AndroidProotRuntime {
    pub(super) fn create_raw_stdio_task(
        &self,
        command: String,
    ) -> Result<(String, Arc<TaskControl>), MobileLinuxError> {
        let id = allocate_raw_stdio_session_id()?;
        let status = MobileLinuxTaskStatus::Running;
        let task = Arc::new(TaskControl::new(id.clone(), command, status));
        {
            let mut tasks = self.state.tasks.lock().expect("mobile-linux tasks mutex");
            if tasks.contains_key(&id) {
                return Err(MobileLinuxError::Io(
                    "raw stdio session identity collision".into(),
                ));
            }
            tasks.insert(id.clone(), task.clone());
        }
        self.emit(
            Some(id.clone()),
            MobileLinuxEventKind::TaskStatusChanged {
                status,
                exit_code: None,
                detail: None,
            },
        );
        Ok((id, task))
    }

    pub(super) async fn open_raw_stdio_owned(
        &self,
        request: RawStdioOpenRequest,
        id: String,
        task: Arc<TaskControl>,
        owner_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<RawStdioSessionHandle, MobileLinuxError> {
        let snapshot_root = self
            .state
            .config
            .app_sandbox_root
            .join("cache/raw-stdio-snapshots")
            .join(&id);
        let mut snapshot_cleanup = SnapshotCleanup(Some(snapshot_root.clone()));
        let snapshot_root_for_copy = snapshot_root.clone();
        let copy_task = task.clone();
        let request_mounts = request.mounts.clone();
        // 0 = queued, 1 = running, 2 = cancelled before admission. Tokio's
        // blocking pool does not resolve even an aborted queued job until a
        // worker becomes free, so its admission must be revocable separately.
        let copy_phase = Arc::new(AtomicU8::new(0));
        let worker_phase = copy_phase.clone();
        let mut copy = tokio::task::spawn_blocking(move || {
            if worker_phase
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "snapshot cancelled before admission".into(),
                ));
            }
            snapshot_read_only_mounts(
                request_mounts,
                &snapshot_root_for_copy,
                &copy_task.cancel_requested,
            )
        });
        let snapshot_result = tokio::select! {
            result = &mut copy => result,
            _ = async {
                while !task.cancel_requested.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {
                if copy_phase.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    copy.abort();
                    // The revoked closure cannot write even if the pool later
                    // dequeues it. Releasing the owner and path guard is safe.
                    Ok(Err(MobileLinuxError::InvalidRequest("snapshot cancelled before admission".into())))
                } else {
                    copy.abort();
                    copy.await
                }
            }
        }.map_err(|error| MobileLinuxError::Io(format!("join LSP workspace snapshot: {error}")))?;
        let (prepared_mounts, snapshot_roots) = match snapshot_result {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_dir_all(&snapshot_root);
                self.finish_task(
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
                return Err(error);
            }
        };
        check_snapshot_cancelled(&task.cancel_requested)?;
        let mounts =
            match self.execution_mounts(&prepared_mounts, ForegroundMountMode::ExplicitOnly) {
                Ok(mounts) => mounts,
                Err(error) => {
                    for root in &snapshot_roots {
                        let _ = fs::remove_dir_all(root);
                    }
                    self.finish_task(
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
                    return Err(error);
                }
            };
        let command_request = LinuxCommandRequest {
            command: request.command,
            args: request.args,
            cwd: request.cwd,
            env: request.env,
            stdin: None,
            timeout_ms: None,
            network: request.network,
            resource_limits: request.resource_limits,
            mounts: mounts.clone(),
        };
        let spawned = match self
            .spawn_child_with_mounts(&command_request, &mounts)
            .await
        {
            Ok(spawned) => spawned,
            Err(error) => {
                for root in &snapshot_roots {
                    let _ = fs::remove_dir_all(root);
                }
                self.finish_task(
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
        if task.cancel_requested.load(Ordering::Acquire) {
            terminate_raw_child(&mut child, pid).await?;
            return Err(MobileLinuxError::InvalidRequest(
                "process startup cancelled".into(),
            ));
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stdin unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stdout unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| MobileLinuxError::Io("PRoot stderr unavailable".to_string()))?;
        let buffers = Arc::new(Mutex::new(RawStdioBuffers::default()));
        let (writes, receiver) = tokio::sync::mpsc::channel(1);
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let control = Arc::new(RawStdioControl {
            task: task.clone(),
            writes,
            stop,
            worker: Mutex::new(None),
            buffers: buffers.clone(),
            snapshot_roots,
            stop_gate: tokio::sync::Mutex::new(()),
            stop_result: Mutex::new(None),
            stop_completion: Mutex::new(None),
            _owner_permit: owner_permit,
        });
        let stdout_task = tokio::spawn(read_raw_stream(stdout, {
            let buffers = buffers.clone();
            move |chunk| {
                let buffers = buffers.clone();
                async move {
                    let mut guard = buffers.lock().expect("raw stdio stdout mutex");
                    if !Self::append_raw_stdio_bytes(&mut guard.stdout, &chunk) {
                        guard.stdout_overflowed = true;
                    }
                    Ok(())
                }
            }
        }));
        let stderr_task = tokio::spawn(read_raw_stream(stderr, {
            let buffers = buffers.clone();
            move |chunk| {
                let buffers = buffers.clone();
                async move {
                    let mut guard = buffers.lock().expect("raw stdio stderr mutex");
                    if !Self::append_raw_stdio_bytes(&mut guard.stderr, &chunk) {
                        guard.stderr_overflowed = true;
                    }
                    Ok(())
                }
            }
        }));
        let writer = tokio::spawn(write_raw_stream(stdin, receiver, stopped.clone()));
        let runtime = self.clone();
        let reaper_id = id.clone();
        let worker_control = control.clone();
        let worker = tokio::spawn(async move {
            let mut stopped = stopped;
            // The same process-group watchdog used by run/background execution
            // owns the child for the whole raw session; receipt and enforcement
            // therefore describe the same configured limit.
            let outcome = tokio::select! {
                result = wait_for_child(&mut child, pid, None, memory_limit_bytes) => result,
                _ = wait_raw_stop(&mut stopped) => {
                    terminate_raw_child(&mut child, pid).await.map(ChildWaitOutcome::Exited)
                }
            };
            // wait_for_child can already have reaped the child on error. Do
            // not signal its numeric PID again: it may now name another task.
            worker_control.stop.send_replace(true);
            let writer_result = writer
                .await
                .map_err(|error| MobileLinuxError::Io(format!("raw stdio writer failed: {error}")));
            let (stdout_result, stderr_result) = tokio::join!(
                join_raw_reader(stdout_task, "stdout"),
                join_raw_reader(stderr_task, "stderr"),
            );
            let stdout_eof = stdout_result.is_ok();
            let stderr_eof = stderr_result.is_ok();
            let cancelled = task.cancel_requested.load(Ordering::Acquire);
            let (status, exit_code, mut error, detail) = match outcome {
                Ok(ChildWaitOutcome::Exited(status)) => (
                    if cancelled {
                        MobileLinuxTaskStatus::Cancelled
                    } else if status.success() {
                        MobileLinuxTaskStatus::Completed
                    } else {
                        MobileLinuxTaskStatus::Failed
                    },
                    Some(status.code().unwrap_or(-1)),
                    None,
                    cancelled.then(|| "raw stdio session closed".to_string()),
                ),
                Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic)) => (
                    MobileLinuxTaskStatus::Failed,
                    Some(-1),
                    Some(MobileLinuxError::ResourceLimitExceeded(
                        diagnostic.summary(),
                    )),
                    Some(diagnostic.detail()),
                ),
                Ok(ChildWaitOutcome::TimedOut) => (
                    MobileLinuxTaskStatus::TimedOut,
                    Some(-1),
                    Some(MobileLinuxError::Timeout),
                    Some("raw stdio session timed out".to_string()),
                ),
                Err(error) => (MobileLinuxTaskStatus::Failed, Some(-1), Some(error), None),
            };
            error = error
                .or_else(|| writer_result.err())
                .or_else(|| stdout_result.err())
                .or_else(|| stderr_result.err());
            let detail = detail.or_else(|| error.as_ref().map(ToString::to_string));
            runtime.finish_task(
                &reaper_id,
                &task,
                if error.is_some() {
                    MobileLinuxTaskStatus::Failed
                } else {
                    status
                },
                exit_code,
                detail,
            );
            // Publish closure only after the child is reaped and BOTH readers
            // have reached EOF (or their explicit errors have been recorded).
            let mut guard = buffers.lock().expect("raw stdio buffers mutex");
            guard.exit_code = exit_code;
            guard.error = error.clone();
            guard.stdout_eof = stdout_eof;
            guard.stderr_eof = stderr_eof;
            guard.closed = true;
            error.map_or(Ok(()), Err)
        });
        *control.worker.lock().expect("raw stdio worker mutex") = Some(worker);
        self.state
            .raw_stdio
            .lock()
            .expect("mobile-linux raw stdio mutex")
            .insert(id.clone(), control);
        snapshot_cleanup.0 = None;
        Ok(RawStdioSessionHandle { id, enforcement })
    }
}

impl AndroidProotRuntime {
    pub(super) async fn close_raw_stdio_if_present(
        &self,
        id: &str,
    ) -> Result<(), MobileLinuxError> {
        self.close_raw_stdio_owned(id, true).await
    }

    pub(super) async fn close_raw_stdio_owned(
        &self,
        id: &str,
        absent_ok: bool,
    ) -> Result<(), MobileLinuxError> {
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("raw stdio mutex")
            .get(id)
            .cloned();
        let Some(control) = control else {
            return if absent_ok {
                Ok(())
            } else {
                Err(MobileLinuxError::InvalidRequest(
                    "unknown raw stdio session".into(),
                ))
            };
        };
        let _gate = control.stop_gate.lock().await;
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
        {
            let mut sessions = self.state.raw_stdio.lock().expect("raw stdio mutex");
            if !sessions
                .get(id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &control))
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "raw stdio owner changed".into(),
                ));
            }
            sessions.remove(id);
        }
        Self::dispose_raw_control(control.clone()).await?;
        Ok(())
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
            .expect("raw stdio mutex")
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| {
                MobileLinuxError::InvalidRequest(
                    "unknown raw stdio session; cleanup is unverified".into(),
                )
            })?;
        let _gate = control.stop_gate.lock().await;
        let same_owner = self
            .state
            .raw_stdio
            .lock()
            .expect("raw stdio mutex")
            .get(&handle.id)
            .is_some_and(|entry| Arc::ptr_eq(entry, &control));
        if !same_owner {
            return Err(MobileLinuxError::InvalidRequest(
                "raw stdio owner changed; cleanup is unverified".into(),
            ));
        }
        let cached = control.stop_result.lock().expect("raw stop result").clone();
        let result = if let Some(result) = cached {
            result
        } else {
            let worker = control
                .worker
                .lock()
                .expect("raw stdio worker mutex")
                .take();
            let termination_requested = worker.as_ref().is_some_and(|task| !task.is_finished())
                && !control.task.terminal_emitted.load(Ordering::Acquire);
            control.task.cancel_requested.store(true, Ordering::Release);
            // Ask the live owner to stop its child. A stored PID may have been
            // reused after the owner reaped or failed and is not safe to signal.
            control.stop.send_replace(true);
            let failure = match worker {
                Some(worker) => match worker.await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(MobileLinuxError::Io(format!(
                        "raw stdio owner failed: {error}"
                    ))),
                },
                None => Some(MobileLinuxError::Io(
                    "raw stdio owner handle is missing".into(),
                )),
            };
            let mut buffers = control.buffers.lock().expect("raw stdio buffers mutex");
            let result = RawStdioStopResult {
                session_id: handle.id.clone(),
                stdout: buffers.stdout.drain(..).collect(),
                stderr: buffers.stderr.drain(..).collect(),
                stdout_truncated: buffers.stdout_overflowed,
                stderr_truncated: buffers.stderr_overflowed,
                stdout_eof: buffers.stdout_eof,
                stderr_eof: buffers.stderr_eof,
                exit_code: buffers.exit_code.filter(|code| *code >= 0),
                termination_requested: Some(termination_requested),
                cleanup: RawStdioTreeStatus::Unverified(
                    "Android raw stdio has no identity-bound process-tree exit receipt".into(),
                ),
                failure: failure.or_else(|| buffers.error.clone()),
            };
            drop(buffers);
            *control.stop_result.lock().expect("raw stop result") = Some(result.clone());
            result
        };
        Ok(bounded_stop_result(result, limit))
    }
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

pub(super) fn raw_stdio_stop_executor() -> Result<&'static tokio::runtime::Handle, MobileLinuxError>
{
    static EXECUTOR: OnceLock<Result<tokio::runtime::Handle, String>> = OnceLock::new();
    EXECUTOR
        .get_or_init(|| {
            let (reply, response) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("android-raw-stop-owner".into())
                .spawn(move || {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
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
                .map_err(|error| format!("start raw stop owner: {error}"))?;
            response
                .recv()
                .map_err(|error| format!("start raw stop runtime: {error}"))?
        })
        .as_ref()
        .map_err(|error| MobileLinuxError::Io(error.clone()))
}

pub(super) fn raw_stdio_stop_capacity() -> Arc<tokio::sync::Semaphore> {
    static CAPACITY: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    CAPACITY
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(32)))
        .clone()
}

impl AndroidProotRuntime {
    pub(super) async fn dispose_raw_control(
        control: Arc<RawStdioControl>,
    ) -> Result<(), MobileLinuxError> {
        // Never wait on a writer's pipe or mutex before signalling the child.
        // The owner task cancels stdin, reaps the process and joins both drains.
        control.task.cancel_requested.store(true, Ordering::Release);
        control.stop.send_replace(true);
        let worker = control
            .worker
            .lock()
            .expect("raw stdio worker mutex")
            .take();
        let result = match worker {
            Some(worker) => worker.await.map_err(|error| {
                MobileLinuxError::Io(format!("raw stdio owner failed: {error}"))
            })?,
            None => Ok(()),
        };
        for root in &control.snapshot_roots {
            let _ = fs::remove_dir_all(root);
        }
        result
    }
}

impl AndroidProotRuntime {
    pub(super) fn append_raw_stdio_bytes(queue: &mut VecDeque<u8>, chunk: &[u8]) -> bool {
        if queue.len().saturating_add(chunk.len()) > MAX_RAW_STDIO_BUFFER_BYTES {
            return false;
        }
        queue.extend(chunk.iter().copied());
        true
    }
}

impl AndroidProotRuntime {
    pub(super) fn drain_raw_stdio_bytes(queue: &mut VecDeque<u8>, max_bytes: usize) -> Vec<u8> {
        queue.drain(..queue.len().min(max_bytes)).collect()
    }

    pub(super) fn drain_raw_stdio_fair(
        buffers: &mut RawStdioBuffers,
        max_bytes: usize,
    ) -> (Vec<u8>, Vec<u8>) {
        let both = !buffers.stdout.is_empty() && !buffers.stderr.is_empty();
        let stderr_first = both && buffers.stderr_first;
        if both {
            buffers.stderr_first = !buffers.stderr_first;
        }
        let first_budget = if both {
            max_bytes / 2 + max_bytes % 2
        } else {
            max_bytes
        };
        let (first, second) = if stderr_first {
            (&mut buffers.stderr, &mut buffers.stdout)
        } else {
            (&mut buffers.stdout, &mut buffers.stderr)
        };
        let mut first_bytes = Self::drain_raw_stdio_bytes(first, first_budget);
        let second_bytes = Self::drain_raw_stdio_bytes(second, max_bytes - first_bytes.len());
        let remaining = max_bytes - first_bytes.len() - second_bytes.len();
        first_bytes.extend(Self::drain_raw_stdio_bytes(first, remaining));
        if stderr_first {
            (second_bytes, first_bytes)
        } else {
            (first_bytes, second_bytes)
        }
    }
}

pub(super) async fn wait_raw_stop(stopped: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = stopped.wait_for(|stopped| *stopped).await;
}

pub(super) fn raw_stdin_closed() -> MobileLinuxError {
    MobileLinuxError::InvalidRequest("raw stdio stdin is closed".to_string())
}

pub(super) async fn write_raw_stream(
    mut stdin: ChildStdin,
    mut writes: tokio::sync::mpsc::Receiver<RawStdioWrite>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let write = tokio::select! {
            biased;
            _ = wait_raw_stop(&mut stopped) => break,
            write = writes.recv() => match write { Some(write) => write, None => break },
        };
        let result = tokio::select! {
            biased;
            _ = wait_raw_stop(&mut stopped) => Err(raw_stdin_closed()),
            result = stdin.write_all(&write.input) => result.map_err(|error| {
                MobileLinuxError::Io(format!("write raw stdio stdin: {error}"))
            }),
        };
        let failed = result.is_err();
        let _ = write.reply.send(result);
        if failed {
            break;
        }
    }
}

// Only the task that owns the unreaped child may signal its process group.
// Once wait returns, its numeric PID can be reused, so do not send a further
// signal even if a descendant might still hold a pipe open.
async fn terminate_raw_child(
    child: &mut Child,
    pid: u32,
) -> Result<std::process::ExitStatus, MobileLinuxError> {
    terminate_group(pid, Signal::SIGTERM);
    if let Ok(result) = tokio::time::timeout(REAP_BUDGET / 2, child.wait()).await {
        return result
            .map_err(|error| MobileLinuxError::Io(format!("reap raw stdio child: {error}")));
    }
    terminate_group(pid, Signal::SIGKILL);
    tokio::time::timeout(REAP_BUDGET / 2, child.wait())
        .await
        .map_err(|_| MobileLinuxError::Io("raw stdio child did not exit after SIGKILL".into()))?
        .map_err(|error| MobileLinuxError::Io(format!("reap raw stdio child: {error}")))
}

pub(super) async fn join_raw_reader(
    mut task: tokio::task::JoinHandle<Result<(), MobileLinuxError>>,
    stream: &str,
) -> Result<(), MobileLinuxError> {
    match tokio::time::timeout(REAP_BUDGET, &mut task).await {
        Ok(result) => result.map_err(|error| {
            MobileLinuxError::Io(format!("raw {stream} reader failed: {error}"))
        })?,
        Err(_) => {
            // A descendant can hold the pipe after the direct child was
            // reaped. The old PID is no longer safe to signal; retain the
            // incomplete-drain error instead of claiming EOF or tree exit.
            task.abort();
            let _ = task.await;
            Err(MobileLinuxError::Io(format!(
                "raw {stream} did not reach EOF after process termination"
            )))
        }
    }
}

pub(super) async fn read_raw_stream<R, F, Fut>(
    mut reader: R,
    mut on_chunk: F,
) -> Result<(), MobileLinuxError>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(), MobileLinuxError>>,
{
    let mut buffer = vec![0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("read raw stdio: {error}")))?;
        if read == 0 {
            return Ok(());
        }
        on_chunk(buffer[..read].to_vec()).await?;
    }
}
