use mobile_linux_api::mobile_linux::LinuxEnforcementReceipt;
use mobile_linux_api::{MobileLinuxError, MountSpec, RawStdioOpenRequest};
use tokio::task::spawn_blocking;

use super::{
    native, parse_raw_stdio_open_response, IosIshRuntime, RawStdioNativeOperation,
    RawStdioOpenPayload, RawStdioRequestPayload,
};

impl IosIshRuntime {
    pub(super) async fn native_open_raw_stdio(
        &self,
        request: &RawStdioOpenRequest,
        mounts: &[MountSpec],
    ) -> Result<(String, LinuxEnforcementReceipt), MobileLinuxError> {
        self.ensure_session_open()?;
        let config_json = self.native_config_json()?;
        let request_json = serde_json::to_string(&RawStdioOpenPayload::from_request(
            request, mounts,
        ))
        .map_err(|error| MobileLinuxError::Io(format!("serialize raw stdio open: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::raw_stdio_open_json(&config_json, &request_json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join raw stdio open: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_raw_stdio_open_response(&response)
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
            return transport.raw_call(operation);
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
