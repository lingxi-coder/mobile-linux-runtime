use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MobileLinuxTaskStatus, RawStdioOpenRequest,
    RawStdioSessionHandle,
};
use nix::sys::signal::Signal;
use std::collections::VecDeque;
use std::fs;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;

use super::{
    check_snapshot_cancelled, snapshot_read_only_mounts, terminate_and_reap, terminate_group,
    wait_for_child, AndroidProotRuntime, ChildWaitOutcome, ForegroundMountMode, RawStdioBuffers,
    RawStdioControl, RawStdioWrite, SnapshotCleanup, SpawnedChild, TaskControl,
    MAX_RAW_STDIO_BUFFER_BYTES, REAP_BUDGET,
};

impl AndroidProotRuntime {
    pub(super) async fn open_raw_stdio_owned(
        &self,
        request: RawStdioOpenRequest,
        id: String,
        task: Arc<TaskControl>,
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
            terminate_and_reap(&mut child, pid).await?;
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
        });
        let stdout_task = tokio::spawn(read_raw_stream(stdout, {
            let buffers = buffers.clone();
            move |chunk| {
                let buffers = buffers.clone();
                async move {
                    let mut guard = buffers.lock().expect("raw stdio stdout mutex");
                    if !Self::append_raw_stdio_bytes(&mut guard.stdout, &chunk) {
                        guard.overflowed = true;
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
                        guard.overflowed = true;
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
                    terminate_and_reap(&mut child, pid).await.map(ChildWaitOutcome::Exited)
                }
            };
            if outcome.is_err() {
                let _ = terminate_and_reap(&mut child, pid).await;
            }
            worker_control.stop.send_replace(true);
            let writer_result = writer
                .await
                .map_err(|error| MobileLinuxError::Io(format!("raw stdio writer failed: {error}")));
            let (stdout_result, stderr_result) = tokio::join!(
                join_raw_reader(stdout_task, pid, "stdout"),
                join_raw_reader(stderr_task, pid, "stderr"),
            );
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
        let control = self
            .state
            .raw_stdio
            .lock()
            .expect("raw stdio mutex")
            .remove(id);
        if let Some(control) = control {
            Self::dispose_raw_control(control).await?;
        }
        Ok(())
    }
}

impl AndroidProotRuntime {
    pub(super) async fn dispose_raw_control(
        control: Arc<RawStdioControl>,
    ) -> Result<(), MobileLinuxError> {
        // Never wait on a writer's pipe or mutex before signalling the child.
        // The owner task cancels stdin, reaps the process and joins both drains.
        control.task.cancel_requested.store(true, Ordering::Release);
        if !control.task.terminal_emitted.load(Ordering::Acquire) {
            let pid = control.task.pid.load(Ordering::Acquire) as u32;
            terminate_group(pid, Signal::SIGTERM);
        }
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

pub(super) async fn join_raw_reader(
    mut task: tokio::task::JoinHandle<Result<(), MobileLinuxError>>,
    pid: u32,
    stream: &str,
) -> Result<(), MobileLinuxError> {
    match tokio::time::timeout(REAP_BUDGET, &mut task).await {
        Ok(result) => result.map_err(|error| {
            MobileLinuxError::Io(format!("raw {stream} reader failed: {error}"))
        })?,
        Err(_) => {
            // A descendant can retain a pipe after the direct child exits.
            terminate_group(pid, Signal::SIGKILL);
            match tokio::time::timeout(REAP_BUDGET, &mut task).await {
                Ok(result) => result.map_err(|error| {
                    MobileLinuxError::Io(format!("raw {stream} reader failed: {error}"))
                })?,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Err(MobileLinuxError::Io(format!(
                        "raw {stream} did not reach EOF after process termination"
                    )))
                }
            }
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
