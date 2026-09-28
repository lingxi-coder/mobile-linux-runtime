//! Recover interrupted activation and reset only runtime scratch state.
use super::RootfsStore;
use crate::filesystem::remove_path;
use crate::guest_path::secure_guest_host_path_with_real_parents;
use crate::{RootfsManifest, RootfsStoreError};
use mobile_linux_api::RootfsStatus;
use std::fs;

const RESETTABLE_GUEST_PATHS: &[&str] = mobile_linux_api::guest_paths::SCRATCH;
impl RootfsStore {
    pub fn recover_interrupted_activation(
        &self,
        manifest: &RootfsManifest,
    ) -> Result<RootfsStatus, RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        self.ensure_managed_root()?;
        let active = self.active_root();
        let next = self.managed_root.join("active.next");
        let rollback = self.managed_root.join("active.rollback");

        if fs::symlink_metadata(&active).is_err() {
            if fs::symlink_metadata(&next).is_ok() && self.verify_root_at(&next, manifest)?.ok {
                fs::rename(&next, &active).map_err(|err| RootfsStoreError::Io {
                    path: active.clone(),
                    message: err.to_string(),
                })?;
            } else if fs::symlink_metadata(&rollback).is_ok()
                && self.verify_root_at(&rollback, manifest)?.ok
            {
                fs::rename(&rollback, &active).map_err(|err| RootfsStoreError::Io {
                    path: active.clone(),
                    message: err.to_string(),
                })?;
            }
        }

        if fs::symlink_metadata(&active).is_ok() {
            if self.verify_root_at(&active, manifest)?.ok {
                for stale in [&next, &rollback] {
                    if fs::symlink_metadata(stale).is_ok() {
                        remove_path(stale.clone())?;
                    }
                }
            } else if fs::symlink_metadata(&rollback).is_ok()
                && self.verify_root_at(&rollback, manifest)?.ok
            {
                remove_path(active.clone())?;
                fs::rename(&rollback, &active).map_err(|err| RootfsStoreError::Io {
                    path: active.clone(),
                    message: err.to_string(),
                })?;
            }
        }
        Ok(self.status(manifest))
    }

    pub fn reset_writable_state(&self, manifest: &RootfsManifest) -> Result<(), RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        let active_root = self.active_root();
        let active_metadata =
            fs::symlink_metadata(&active_root).map_err(|err| RootfsStoreError::Io {
                path: active_root.clone(),
                message: err.to_string(),
            })?;
        if active_metadata.file_type().is_symlink() || !active_metadata.is_dir() {
            return Err(RootfsStoreError::UnsafeResetPath(active_root));
        }
        for guest_path in &manifest.writable_paths {
            // `/root` can contain user SSH material and `/workspace` contains
            // projects. Rootfs reset may clear only runtime scratch paths.
            if !RESETTABLE_GUEST_PATHS.contains(&guest_path.as_str()) {
                continue;
            }
            let target = secure_guest_host_path_with_real_parents(&active_root, guest_path)
                .map_err(|error| match error {
                    RootfsStoreError::UnsafeManagedPath(path) => {
                        RootfsStoreError::UnsafeResetPath(path)
                    }
                    other => other,
                })?;
            if target == active_root {
                return Err(RootfsStoreError::UnsafeResetPath(target));
            }
            let metadata = match fs::symlink_metadata(&target) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(RootfsStoreError::Io {
                        path: target.clone(),
                        message: err.to_string(),
                    });
                }
            };
            if metadata.file_type().is_symlink() {
                fs::remove_file(&target).map_err(|err| RootfsStoreError::Io {
                    path: target.clone(),
                    message: err.to_string(),
                })?;
                continue;
            }
            if metadata.is_dir() {
                for entry in fs::read_dir(&target).map_err(|err| RootfsStoreError::Io {
                    path: target.clone(),
                    message: err.to_string(),
                })? {
                    let entry = entry.map_err(|err| RootfsStoreError::Io {
                        path: target.clone(),
                        message: err.to_string(),
                    })?;
                    remove_path(entry.path())?;
                }
            } else {
                fs::remove_file(&target).map_err(|err| RootfsStoreError::Io {
                    path: target.clone(),
                    message: err.to_string(),
                })?;
            }
        }
        Ok(())
    }
}
