use mobile_linux_api::{MobileLinuxError, MountSpec};
use tokio::task::spawn_blocking;

use super::{native, parse_native_ok, validate_mount, IosIshRuntime, MountConfigPayload};

impl IosIshRuntime {
    pub(super) async fn apply_mounts(&self, mounts: &[MountSpec]) -> Result<(), MobileLinuxError> {
        #[cfg(test)]
        if self.state.test_transport.is_some() {
            return Ok(());
        }
        self.ensure_native_available()?;
        let config_json = self.native_config_json()?;
        let payload = MountConfigPayload::from_mounts(mounts);
        let json = serde_json::to_string(&payload)
            .map_err(|error| MobileLinuxError::Io(format!("serialize mounts json: {error}")))?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            native::configure_mounts_json(&config_json, &json)
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join configure_mounts_json: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_native_ok(&response)?;
        Ok(())
    }
}

impl IosIshRuntime {
    pub(super) fn merged_mounts(
        &self,
        request_mounts: &[MountSpec],
    ) -> Result<Vec<MountSpec>, MobileLinuxError> {
        let mut mounts = self
            .state
            .mounts
            .read()
            .expect("ios-ish mounts rwlock")
            .clone();
        for mount in request_mounts {
            let normalized = validate_mount(mount, &self.state.config)?;
            if let Some(existing) = mounts
                .iter_mut()
                .find(|existing| existing.guest_path == normalized.guest_path)
            {
                *existing = normalized;
            } else {
                mounts.push(normalized);
            }
        }
        Ok(mounts)
    }
}

impl IosIshRuntime {
    pub(super) fn isolated_mounts(
        &self,
        request_mounts: &[MountSpec],
    ) -> Result<Vec<MountSpec>, MobileLinuxError> {
        if request_mounts.is_empty() {
            return Err(MobileLinuxError::InvalidRequest(
                "isolated execution requires explicit mounts".into(),
            ));
        }
        let mut mounts: Vec<MountSpec> = Vec::with_capacity(request_mounts.len());
        for mount in request_mounts {
            let normalized = validate_mount(mount, &self.state.config)?;
            mounts.retain(|existing| existing.guest_path != normalized.guest_path);
            mounts.push(normalized);
        }
        Ok(mounts)
    }
}
