use mobile_linux_api::{
    MobileLinuxError, MobileLinuxRuntimeMode, RootfsState, RootfsStatus, SandboxBackend,
};
use mobile_linux_core::{RootfsManifest, RootfsStore, RootfsStoreError};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::AndroidProotRuntime;

impl AndroidProotRuntime {
    pub(super) fn proot_binary(&self) -> Result<PathBuf, MobileLinuxError> {
        let root = &self.state.config.managed_root;
        let mut candidates = vec![
            root.join("bin/libproot.so"),
            root.join("libproot.so"),
            root.join("bin/proot"),
        ];
        if let Some(native_lib_dir) = self.state.config.native_library_dir.clone() {
            candidates.insert(0, native_lib_dir.join("libproot.so"));
        }
        candidates
            .into_iter()
            .find(|candidate| executable_regular_file(candidate))
            .ok_or_else(|| {
                MobileLinuxError::Unavailable(format!(
                    "PRoot executable is missing under {}",
                    root.display()
                ))
            })
    }
}

impl AndroidProotRuntime {
    pub(super) fn policy_launcher(&self) -> Result<PathBuf, MobileLinuxError> {
        let mut candidates = vec![
            self.state
                .config
                .managed_root
                .join("bin/libmobile_linux_policy_launcher.so"),
            self.state
                .config
                .managed_root
                .join("libmobile_linux_policy_launcher.so"),
        ];
        if let Some(native_lib_dir) = self.state.config.native_library_dir.clone() {
            candidates.insert(0, native_lib_dir.join("libmobile_linux_policy_launcher.so"));
        }
        candidates
            .into_iter()
            .find(|candidate| executable_regular_file(candidate))
            .ok_or_else(|| {
                MobileLinuxError::NetworkPolicyUnavailable(
                    "Android network policy launcher is not packaged or executable".to_string(),
                )
            })
    }
}

impl AndroidProotRuntime {
    pub(super) fn checked_active_root(&self) -> Result<PathBuf, MobileLinuxError> {
        let root = self.state.config.active_root();
        let metadata = fs::symlink_metadata(&root).map_err(|error| {
            MobileLinuxError::Unavailable(format!(
                "active rootfs is missing at {}: {error}",
                root.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(MobileLinuxError::Integrity(
                "active rootfs must be a real directory".to_string(),
            ));
        }
        let shell = root.join("bin/sh");
        if !executable_regular_file(&shell) {
            return Err(MobileLinuxError::Integrity(format!(
                "rootfs shell is missing or not executable: {}",
                shell.display()
            )));
        }
        Ok(root)
    }
}

impl AndroidProotRuntime {
    pub(super) fn readiness(&self) -> Result<(PathBuf, PathBuf), MobileLinuxError> {
        Ok((self.proot_binary()?, self.checked_active_root()?))
    }
}

impl AndroidProotRuntime {
    pub(super) fn prepare_execution(&self) -> Result<(), MobileLinuxError> {
        self.readiness()?;
        fs::create_dir_all(self.state.config.managed_root.join("tmp"))
            .map_err(|error| MobileLinuxError::Io(format!("create PRoot tmp: {error}")))?;
        self.state.booted.store(true, Ordering::Release);
        Ok(())
    }
}

impl AndroidProotRuntime {
    pub(super) fn rootfs_store(&self) -> RootfsStore {
        RootfsStore::new(
            self.state.config.managed_root.clone(),
            SandboxBackend::AndroidProot,
            MobileLinuxRuntimeMode::MobileLinux,
            "android",
            self.state.config.abi.clone(),
        )
    }
}

impl AndroidProotRuntime {
    pub(super) fn rootfs_manifest_paths(&self) -> [PathBuf; 2] {
        [
            self.state.config.managed_root.join("rootfs-manifest.json"),
            self.state.config.active_root().join("rootfs-manifest.json"),
        ]
    }
}

impl AndroidProotRuntime {
    pub(super) fn load_rootfs_manifest(&self) -> Result<RootfsManifest, MobileLinuxError> {
        let paths = self.rootfs_manifest_paths();
        for path in &paths {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(MobileLinuxError::Io(format!(
                        "read rootfs manifest metadata {}: {error}",
                        path.display()
                    )))
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(MobileLinuxError::Integrity(format!(
                    "rootfs manifest must be a regular file: {}",
                    path.display()
                )));
            }
            let bytes = fs::read(path).map_err(|error| {
                MobileLinuxError::Io(format!("read rootfs manifest {}: {error}", path.display()))
            })?;
            let manifest = serde_json::from_slice::<RootfsManifest>(&bytes).map_err(|error| {
                MobileLinuxError::Integrity(format!(
                    "parse rootfs manifest {}: {error}",
                    path.display()
                ))
            })?;
            return Ok(manifest);
        }
        Err(MobileLinuxError::Unavailable(format!(
            "rootfs manifest is missing (looked for {} and {})",
            paths[0].display(),
            paths[1].display()
        )))
    }
}

impl AndroidProotRuntime {
    pub(super) fn rootfs_snapshot(&self) -> RootfsStatus {
        if let Ok(manifest) = self.load_rootfs_manifest() {
            return self.rootfs_store().status(&manifest);
        }
        let active = self.state.config.active_root();
        let staged = self.state.config.staged_root();
        let root = self.checked_active_root();
        let proot = self.proot_binary();
        let (state, last_error) = match (&root, &proot) {
            (Ok(_), Ok(_)) => (RootfsState::Ready, None),
            (Err(error), _) if path_present(&active) => {
                (RootfsState::Corrupt, Some(error.to_string()))
            }
            (Err(error), _) if path_present(&staged) => {
                (RootfsState::Installing, Some(error.to_string()))
            }
            (Err(error), _) => (RootfsState::Missing, Some(error.to_string())),
            (_, Err(error)) => (RootfsState::Unsupported, Some(error.to_string())),
        };
        RootfsStatus {
            state,
            backend: SandboxBackend::AndroidProot,
            mode: MobileLinuxRuntimeMode::MobileLinux,
            platform: "android".to_string(),
            abi: self.state.config.abi.clone(),
            version: Some(self.state.config.rootfs_version.clone()),
            managed_root: Some(self.state.config.managed_root.clone()),
            active_root: path_present(&active).then_some(active.clone()),
            staged_root: path_present(&staged).then_some(staged),
            archive_sha256: self.state.config.archive_sha256.clone(),
            installed_size_bytes: directory_size(&active).ok(),
            writable_guest_paths: vec![
                "/root".to_string(),
                "/tmp".to_string(),
                "/var/tmp".to_string(),
            ],
            last_error,
        }
    }
}

pub(super) fn executable_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.permissions().mode() & 0o111 != 0
    })
}

pub(super) fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_symlink())
}

pub(super) fn directory_size(path: &Path) -> std::io::Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut size = 0_u64;
    for entry in fs::read_dir(path)? {
        size = size.saturating_add(directory_size(&entry?.path())?);
    }
    Ok(size)
}

pub(super) fn rootfs_store_error(error: RootfsStoreError) -> MobileLinuxError {
    match error {
        RootfsStoreError::Manifest(error) => MobileLinuxError::Integrity(error.to_string()),
        RootfsStoreError::Io { path, message } => {
            MobileLinuxError::Io(format!("{}: {message}", path.display()))
        }
        RootfsStoreError::ArchiveSizeMismatch { expected, actual } => MobileLinuxError::Integrity(
            format!("archive size mismatch: expected {expected}, got {actual}"),
        ),
        RootfsStoreError::ArchiveHashMismatch { expected, actual } => MobileLinuxError::Integrity(
            format!("archive hash mismatch: expected {expected}, got {actual}"),
        ),
        RootfsStoreError::TargetMismatch { store, manifest } => MobileLinuxError::Integrity(
            format!("rootfs manifest target {manifest} does not match store target {store}"),
        ),
        RootfsStoreError::UnsafeResetPath(path) | RootfsStoreError::UnsafeManagedPath(path) => {
            MobileLinuxError::Integrity(format!("unsafe rootfs path: {}", path.display()))
        }
        RootfsStoreError::Integrity(message) => MobileLinuxError::Integrity(message),
        RootfsStoreError::ExecutionDenied { path, reason } => {
            MobileLinuxError::Integrity(format!("{path}: {reason}"))
        }
    }
}
