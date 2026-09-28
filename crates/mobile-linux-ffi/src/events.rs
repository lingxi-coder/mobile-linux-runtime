use mobile_linux_api as platform_api;
#[cfg(any(target_os = "android", target_os = "ios"))]
use std::path::PathBuf;

use crate::types::{MobileLinuxApiErrorFfi, MobileLinuxEventFfi, MobileLinuxEventKindFfi};

/// FFI stream sink for live command output.
#[uniffi::export(callback_interface)]
#[async_trait::async_trait]
pub trait RuntimeStreamSink: Send + Sync {
    async fn stdout_line(&self, line: String) -> Result<(), MobileLinuxApiErrorFfi>;
    async fn stderr_chunk(&self, chunk: Vec<u8>) -> Result<(), MobileLinuxApiErrorFfi>;
}

/// Crate-local callback interface for mobile-linux events.
#[uniffi::export(callback_interface)]
#[async_trait::async_trait]
pub trait RuntimeEventSink: Send + Sync {
    async fn on_event(&self, event: MobileLinuxEventFfi) -> Result<(), MobileLinuxApiErrorFfi>;
}

pub(crate) struct RuntimeEventSinkBridge {
    pub(crate) task_id: String,
    pub(crate) inner: Box<dyn RuntimeEventSink>,
}

impl RuntimeEventSinkBridge {
    pub(crate) async fn emit_event(
        &self,
        event: MobileLinuxEventFfi,
    ) -> Result<(), MobileLinuxApiErrorFfi> {
        self.inner.on_event(event).await
    }
}

#[async_trait::async_trait]
impl platform_api::ProcessStreamSink for RuntimeEventSinkBridge {
    async fn stdout_line(&self, line: String) -> Result<(), platform_api::ProcessError> {
        self.inner
            .on_event(MobileLinuxEventFfi {
                sequence: 0,
                task_id: Some(self.task_id.clone()),
                kind: MobileLinuxEventKindFfi::StdoutLine,
                data: None,
                text: Some(line),
                session_id: None,
                status: None,
                exit_code: None,
                timed_out: None,
                cancelled: None,
                detail: None,
            })
            .await
            .map_err(|err| platform_api::ProcessError::Io(err.to_string()))
    }

    async fn stderr_chunk(&self, chunk: Vec<u8>) -> Result<(), platform_api::ProcessError> {
        self.inner
            .on_event(MobileLinuxEventFfi {
                sequence: 0,
                task_id: Some(self.task_id.clone()),
                kind: MobileLinuxEventKindFfi::StderrChunk,
                data: Some(chunk),
                text: None,
                session_id: None,
                status: None,
                exit_code: None,
                timed_out: None,
                cancelled: None,
                detail: None,
            })
            .await
            .map_err(|err| platform_api::ProcessError::Io(err.to_string()))
    }
}
