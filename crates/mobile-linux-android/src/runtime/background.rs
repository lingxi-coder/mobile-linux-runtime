use mobile_linux_api::{
    LinuxCommandRequest, LinuxProcessHandle, MobileLinuxError, MobileLinuxEventKind,
    MobileLinuxTaskStatus,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::{
    join_reader, read_stderr, read_stdout, terminate_and_reap, wait_for_child, AndroidProotRuntime,
    ChildWaitOutcome, SpawnedChild, StdinWriter, TaskControl,
};

impl AndroidProotRuntime {
    pub(super) async fn spawn_background_owned(
        &self,
        request: LinuxCommandRequest,
        id: String,
        task: Arc<TaskControl>,
    ) -> Result<LinuxProcessHandle, MobileLinuxError> {
        let spawned = match self.spawn_child(&request).await {
            Ok(spawned) => spawned,
            Err(error) => {
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
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let runtime = self.clone();
        let reaper_id = id.clone();
        tokio::spawn(async move {
            let stdout_task = stdout.map(|stdout| {
                let rt = runtime.clone();
                let stream_id = reaper_id.clone();
                tokio::spawn(async move {
                    read_stdout(stdout, move |line| {
                        rt.emit(
                            Some(stream_id.clone()),
                            MobileLinuxEventKind::StdoutLine { line },
                        );
                        async { Ok(()) }
                    })
                    .await
                })
            });
            let stderr_task = stderr.map(|stderr| {
                let rt = runtime.clone();
                let stream_id = reaper_id.clone();
                tokio::spawn(async move {
                    read_stderr(stderr, move |chunk| {
                        rt.emit(
                            Some(stream_id.clone()),
                            MobileLinuxEventKind::StderrChunk { chunk },
                        );
                        async { Ok(()) }
                    })
                    .await
                })
            });
            let stdin_writer = StdinWriter::start(child.stdin.take(), request.stdin);
            let result = wait_for_child(
                &mut child,
                pid,
                request.timeout_ms.map(Duration::from_millis),
                memory_limit_bytes,
            )
            .await;
            let stdin_result = stdin_writer.finish().await;
            let stdout_result = match stdout_task {
                Some(task) => join_reader(task, "stdout").await,
                None => Ok(Vec::new()),
            };
            let stderr_result = match stderr_task {
                Some(task) => join_reader(task, "stderr").await,
                None => Ok(Vec::new()),
            };
            let reader_error = stdout_result
                .err()
                .or_else(|| stderr_result.err())
                .or_else(|| {
                    if matches!(&result, Ok(ChildWaitOutcome::Exited(status)) if status.success()) {
                        stdin_result.err()
                    } else {
                        None
                    }
                });
            let cancelled = task.cancel_requested.load(Ordering::Acquire);
            match (result, reader_error) {
                (_, Some(error)) => runtime.finish_task(
                    &reaper_id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                ),
                (Ok(ChildWaitOutcome::Exited(status)), None) => {
                    let code = status.code().unwrap_or(-1);
                    runtime.finish_task(
                        &reaper_id,
                        &task,
                        if cancelled {
                            MobileLinuxTaskStatus::Cancelled
                        } else if code == 0 {
                            MobileLinuxTaskStatus::Completed
                        } else {
                            MobileLinuxTaskStatus::Failed
                        },
                        Some(code),
                        cancelled.then(|| "task cancelled".to_string()),
                    );
                }
                (Ok(ChildWaitOutcome::TimedOut), None) => runtime.finish_task(
                    &reaper_id,
                    &task,
                    MobileLinuxTaskStatus::TimedOut,
                    None,
                    Some("command timed out".to_string()),
                ),
                (Ok(ChildWaitOutcome::MemoryLimitExceeded(diagnostic)), None) => {
                    runtime.finish_task(
                        &reaper_id,
                        &task,
                        MobileLinuxTaskStatus::Failed,
                        None,
                        Some(diagnostic.detail()),
                    );
                }
                (Err(error), None) => runtime.finish_task(
                    &reaper_id,
                    &task,
                    MobileLinuxTaskStatus::Failed,
                    None,
                    Some(error.to_string()),
                ),
            }
        });
        Ok(LinuxProcessHandle { id, enforcement })
    }
}
