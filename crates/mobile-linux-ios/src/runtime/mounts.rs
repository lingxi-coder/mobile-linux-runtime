use mobile_linux_api::{MobileLinuxError, MountSpec};
use tokio::task::spawn_blocking;

use super::{native, parse_native_ok, validate_mount, IosIshRuntime, MountConfigPayload};

impl IosIshRuntime {
    pub(super) async fn apply_mounts(&self, mounts: &[MountSpec]) -> Result<(), MobileLinuxError> {
        if let Err(error) = self.apply_mounts_native(mounts).await {
            // The native coordinator rejects mount changes before mutation
            // while an isolated process owns the scope. Keep reporting the
            // still-live prior table; changing it here would falsify host
            // path translation while that process is running.
            if matches!(
                &error,
                MobileLinuxError::InvalidRequest(message)
                    if message.contains("isolated guest process owns the mount scope")
                        || message.contains("another workspace has live guest tasks")
            ) {
                return Err(error);
            }
            let previous = self
                .state
                .mounts
                .read()
                .expect("ios-ish mounts rwlock")
                .clone();
            if self.apply_mounts_native(&previous).await.is_err() {
                // A native apply may have replaced only part of the table.
                // Try to remove every bind before reporting the state as
                // unavailable; never leave an uncertain wider mount table
                // available to guest-path translation or the next request.
                let _ = self.apply_mounts_native(&[]).await;
                self.state
                    .mounts
                    .write()
                    .expect("ios-ish mounts rwlock")
                    .clear();
                self.state
                    .mounts_consistent
                    .store(false, std::sync::atomic::Ordering::Release);
            }
            return Err(error);
        }
        Ok(())
    }

    async fn apply_mounts_native(&self, mounts: &[MountSpec]) -> Result<(), MobileLinuxError> {
        #[cfg(test)]
        if let Some(transport) = &self.state.test_transport {
            return transport.configure_mounts(mounts);
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
        if !self
            .state
            .mounts_consistent
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(MobileLinuxError::InvalidRequest(
                "runtime mount table is unavailable; configure mounts again".into(),
            ));
        }
        let mut mounts = self
            .state
            .mounts
            .read()
            .expect("ios-ish mounts rwlock")
            .clone();
        let configured_mounts = mounts.clone();
        for mount in request_mounts {
            let normalized = validate_mount(mount, &self.state.config)?;
            if self
                .state
                .config
                .authorized_project_mounts
                .iter()
                .any(|grant| grant == &normalized)
                && !configured_mounts.contains(&normalized)
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "Project mount must be configured before a run".into(),
                ));
            }
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
