use mobile_linux_api::{
    MobileLinuxError, MobileLinuxRuntimeMode, RootfsState, RootfsStatus, SandboxBackend,
};
use std::fs;
use std::path::Path;
use tokio::task::spawn_blocking;

use super::{native, parse_native_ok, IosIshRuntime};

impl IosIshRuntime {
    pub(super) fn rootfs_snapshot_with_error(
        &self,
        override_state: Option<RootfsState>,
        last_error: Option<String>,
    ) -> RootfsStatus {
        let active_root = self.state.config.active_root();
        let native_unavailable = self.native_unavailable_reason();
        let state = if let Some(reason) = native_unavailable.clone() {
            if override_state.is_some() {
                override_state.unwrap_or(RootfsState::Unsupported)
            } else {
                let _ = reason;
                RootfsState::Unsupported
            }
        } else if let Some(state) = override_state {
            state
        } else {
            filesystem_rootfs_state(&active_root, &self.state.config.managed_root)
        };
        RootfsStatus {
            state,
            backend: SandboxBackend::IosIsh,
            mode: MobileLinuxRuntimeMode::MobileLinux,
            platform: "ios".to_string(),
            abi: self.state.config.abi.clone(),
            version: Some(self.state.config.rootfs_version.clone()),
            managed_root: Some(self.state.config.managed_root.clone()),
            active_root: path_present(&active_root).then_some(active_root.clone()),
            staged_root: None,
            archive_sha256: self.state.config.archive_sha256.clone(),
            installed_size_bytes: directory_size(&active_root).ok(),
            writable_guest_paths: vec![
                mobile_linux_api::mobile_linux::guest_paths::HOME.to_string(),
                mobile_linux_api::mobile_linux::guest_paths::SCRATCH[0].to_string(),
                mobile_linux_api::mobile_linux::guest_paths::SCRATCH[1].to_string(),
                self.state.config.workspace_guest_path(),
            ],
            last_error: last_error.or(native_unavailable),
        }
    }
}

impl IosIshRuntime {
    pub(super) fn rootfs_snapshot(&self) -> RootfsStatus {
        self.rootfs_snapshot_with_error(None, None)
    }
}

impl IosIshRuntime {
    pub(super) async fn refresh_rootfs_snapshot(&self) -> Result<RootfsStatus, MobileLinuxError> {
        // A rootfs contains tens of thousands of entries. Never enumerate it on
        // the foreign executor's poll thread (which may be the UI main actor).
        let runtime = self.clone();
        let snapshot = spawn_blocking(move || runtime.rootfs_snapshot())
            .await
            .map_err(|error| MobileLinuxError::Io(format!("join rootfs status scan: {error}")))?;
        *self
            .state
            .rootfs_status_cache
            .lock()
            .expect("ios-ish rootfs status cache") = Some(snapshot.clone());
        Ok(snapshot)
    }
}

impl IosIshRuntime {
    pub(super) fn invalidate_rootfs_snapshot(&self) {
        *self
            .state
            .rootfs_status_cache
            .lock()
            .expect("ios-ish rootfs status cache") = None;
    }
}

impl IosIshRuntime {
    pub(super) async fn install_rootfs(&self, reset: bool) -> Result<(), MobileLinuxError> {
        self.ensure_native_available()?;
        fs::create_dir_all(&self.state.config.managed_root).map_err(|error| {
            MobileLinuxError::Io(format!(
                "create managed root {}: {error}",
                self.state.config.managed_root.display()
            ))
        })?;
        let config_json = self.native_config_json()?;
        let native_lock = self.state.native_lock.clone();
        let response = spawn_blocking(move || {
            let _guard = native_lock.lock().expect("ios-ish native lock");
            if reset {
                native::reset_rootfs_json(&config_json)
            } else {
                native::install_rootfs_json(&config_json)
            }
        })
        .await
        .map_err(|error| MobileLinuxError::Io(format!("join install_rootfs: {error}")))?
        .map_err(MobileLinuxError::Io)?;
        parse_native_ok(&response)?;
        Ok(())
    }
}

pub(super) fn filesystem_rootfs_state(active_root: &Path, managed_root: &Path) -> RootfsState {
    let active_metadata = fs::symlink_metadata(active_root).ok();
    if let Some(metadata) = active_metadata {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return RootfsState::Corrupt;
        }
        let data_root = active_root.join("data");
        let meta_db = active_root.join("meta.db");
        let arch = active_root.join(".arch");
        let arch_ok = fs::read_to_string(&arch)
            .map(|value| value.trim() == "aarch64")
            .unwrap_or(false);
        return if data_root.is_dir() && meta_db.is_file() && arch_ok {
            RootfsState::Ready
        } else {
            RootfsState::Corrupt
        };
    }
    if let Ok(metadata) = fs::symlink_metadata(managed_root) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return RootfsState::Corrupt;
        }
    }
    RootfsState::Missing
}

pub(super) fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub(super) fn directory_size(path: &Path) -> std::io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}
