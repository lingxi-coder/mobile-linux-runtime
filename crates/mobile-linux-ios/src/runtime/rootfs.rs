use mobile_linux_api::{
    BrandRuntimeProfile, MobileLinuxError, MobileLinuxRuntimeMode, RootfsState, RootfsStatus,
    SandboxBackend,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use tokio::task::spawn_blocking;

use super::{native, parse_native_ok, IosIshRuntime, IosIshRuntimeConfig};

#[derive(Deserialize)]
struct BundleRootfsIdentity {
    schema_version: u32,
    platform: String,
    guest_abi: String,
    rootfs_version: String,
    rootfs_zip_sha256: String,
    brand_profile: BrandRuntimeProfile,
}

#[derive(Deserialize)]
struct InstalledRootfsIdentity {
    abi: String,
    brand_id: String,
    linux_user: String,
    config_dir_name: String,
    rootfs_version: String,
    archive_sha256: String,
    arch: String,
}

fn read_identity_file(path: &Path) -> Result<Vec<u8>, MobileLinuxError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        MobileLinuxError::Integrity(format!("rootfs identity {}: {error}", path.display()))
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        return Err(MobileLinuxError::Integrity(format!(
            "rootfs identity must be a bounded regular file: {}",
            path.display()
        )));
    }
    fs::read(path).map_err(|error| MobileLinuxError::Integrity(error.to_string()))
}

pub(super) fn validate_bundle_rootfs_identity(
    config: &IosIshRuntimeConfig,
) -> Result<(), MobileLinuxError> {
    let archive = config
        .rootfs_archive_path
        .as_ref()
        .ok_or_else(|| MobileLinuxError::Integrity("explicit rootfs archive is required".into()))?;
    let metadata = fs::symlink_metadata(archive)
        .map_err(|error| MobileLinuxError::Integrity(format!("rootfs archive: {error}")))?;
    if !archive.is_absolute() || !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MobileLinuxError::Integrity(
            "rootfs archive must be an absolute regular file".into(),
        ));
    }
    let sidecar = archive
        .parent()
        .ok_or_else(|| {
            MobileLinuxError::Integrity("rootfs archive has no parent directory".into())
        })?
        .join("linux-runtime-manifest.json");
    let identity: BundleRootfsIdentity = serde_json::from_slice(&read_identity_file(&sidecar)?)
        .map_err(|error| {
            MobileLinuxError::Integrity(format!("invalid rootfs manifest: {error}"))
        })?;
    let expected = config
        .archive_sha256
        .as_deref()
        .ok_or_else(|| MobileLinuxError::Integrity("rootfs archive digest is required".into()))?;
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || identity.schema_version != 1
        || identity.platform != "ios"
        || identity.guest_abi != "aarch64"
        || identity.rootfs_version != config.rootfs_version
        || identity.brand_profile != config.brand_profile
        || identity.rootfs_zip_sha256 != expected
    {
        return Err(MobileLinuxError::Integrity(
            "rootfs manifest does not match runtime identity".into(),
        ));
    }
    let mut file = fs::File::open(archive)
        .map_err(|error| MobileLinuxError::Integrity(format!("open rootfs archive: {error}")))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            MobileLinuxError::Integrity(format!("read rootfs archive: {error}"))
        })?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Err(MobileLinuxError::Integrity(
            "rootfs archive digest mismatch".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_installed_rootfs_identity(
    config: &IosIshRuntimeConfig,
) -> Result<(), MobileLinuxError> {
    let path = config.active_root().join("bridge-state.json");
    let identity: InstalledRootfsIdentity = serde_json::from_slice(&read_identity_file(&path)?)
        .map_err(|error| {
            MobileLinuxError::Integrity(format!("invalid installed rootfs identity: {error}"))
        })?;
    if identity.abi != config.abi
        || identity.arch != "aarch64"
        || identity.brand_id != config.brand_profile.brand_id
        || identity.linux_user != config.brand_profile.linux_user
        || identity.config_dir_name != config.brand_profile.config_dir_name
        || identity.rootfs_version != config.rootfs_version
        || Some(identity.archive_sha256.as_str()) != config.archive_sha256.as_deref()
    {
        return Err(MobileLinuxError::Integrity(
            "installed rootfs does not match runtime identity".into(),
        ));
    }
    Ok(())
}

impl IosIshRuntime {
    pub(super) async fn validate_bundle_rootfs_identity(&self) -> Result<(), MobileLinuxError> {
        let config = self.state.config.clone();
        spawn_blocking(move || validate_bundle_rootfs_identity(&config))
            .await
            .map_err(|error| MobileLinuxError::Io(format!("join rootfs identity check: {error}")))?
    }

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
        let identity_error = if state == RootfsState::Ready {
            validate_installed_rootfs_identity(&self.state.config).err()
        } else {
            None
        };
        let state = if identity_error.is_some() {
            RootfsState::Corrupt
        } else {
            state
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
            writable_guest_paths: mobile_linux_api::guest_paths::writable_roots_for_profile(
                &self.state.config.brand_profile,
            ),
            last_error: last_error
                .or_else(|| identity_error.map(|error| error.to_string()))
                .or(native_unavailable),
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
        if !reset {
            self.validate_bundle_rootfs_identity().await?;
        }
        self.install_verified_rootfs(reset).await
    }

    pub(super) async fn install_verified_rootfs(
        &self,
        reset: bool,
    ) -> Result<(), MobileLinuxError> {
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
        if reset {
            Ok(())
        } else {
            validate_installed_rootfs_identity(&self.state.config)
        }
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
