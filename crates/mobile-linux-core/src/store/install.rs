//! Verify staging trees and atomically activate a rootfs.
use super::{verification::verification_failure_message, RootfsStore};
use crate::filesystem::{ensure_real_directory, remove_path};
use crate::{RootfsManifest, RootfsStoreError};
use mobile_linux_api::RootfsStatus;
use std::{
    fs,
    path::{Path, PathBuf},
};

impl RootfsStore {
    pub fn stage_prepared_rootfs(
        &self,
        prepared_root: &Path,
        manifest: &RootfsManifest,
    ) -> Result<PathBuf, RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        self.ensure_managed_root()?;
        let prepared_metadata =
            fs::symlink_metadata(prepared_root).map_err(|err| RootfsStoreError::Io {
                path: prepared_root.to_path_buf(),
                message: err.to_string(),
            })?;
        if prepared_metadata.file_type().is_symlink() || !prepared_metadata.is_dir() {
            return Err(RootfsStoreError::UnsafeManagedPath(
                prepared_root.to_path_buf(),
            ));
        }
        let report = self.verify_root_at(prepared_root, manifest)?;
        if !report.ok {
            return Err(RootfsStoreError::Integrity(format!(
                "prepared {}",
                verification_failure_message(&report)
            )));
        }

        let staged_parent = self.managed_root.join("staged");
        fs::create_dir_all(&staged_parent).map_err(|err| RootfsStoreError::Io {
            path: staged_parent.clone(),
            message: err.to_string(),
        })?;
        ensure_real_directory(&staged_parent)?;
        let staged = self.staged_root(&manifest.rootfs_version);
        if fs::symlink_metadata(&staged).is_ok() {
            remove_path(staged.clone())?;
        }
        fs::rename(prepared_root, &staged).map_err(|err| RootfsStoreError::Io {
            path: staged.clone(),
            message: err.to_string(),
        })?;
        Ok(staged)
    }

    pub fn activate_staged_rootfs(
        &self,
        manifest: &RootfsManifest,
    ) -> Result<RootfsStatus, RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        self.ensure_managed_root()?;
        self.recover_interrupted_activation(manifest)?;

        let staged = self.staged_root(&manifest.rootfs_version);
        let report = self.verify_root_at(&staged, manifest)?;
        if !report.ok {
            return Err(RootfsStoreError::Integrity(format!(
                "staged {}",
                verification_failure_message(&report)
            )));
        }

        self.preserve_guest_homes()?;
        let active = self.active_root();
        let next = self.managed_root.join("active.next");
        let rollback = self.managed_root.join("active.rollback");
        if fs::symlink_metadata(&next).is_ok() {
            remove_path(next.clone())?;
        }
        fs::rename(&staged, &next).map_err(|err| RootfsStoreError::Io {
            path: next.clone(),
            message: err.to_string(),
        })?;
        if fs::symlink_metadata(&active).is_ok() {
            fs::rename(&active, &rollback).map_err(|err| RootfsStoreError::Io {
                path: rollback.clone(),
                message: err.to_string(),
            })?;
        }
        if let Err(err) = fs::rename(&next, &active) {
            if fs::symlink_metadata(&rollback).is_ok() && fs::symlink_metadata(&active).is_err() {
                let _ = fs::rename(&rollback, &active);
            }
            return Err(RootfsStoreError::Io {
                path: active,
                message: err.to_string(),
            });
        }
        if fs::symlink_metadata(&rollback).is_ok() {
            remove_path(rollback)?;
        }
        Ok(self.status(manifest))
    }
}

impl RootfsStore {
    /// Preserve guest homes outside replaceable images. Existing files win;
    /// a failed copy is retried before activation and never removes the source.
    pub fn preserve_guest_homes(&self) -> Result<(), RootfsStoreError> {
        let marker = self.managed_root.join("persistent/.user-home-migrated");
        if marker.exists() {
            return Ok(());
        }
        let active = self.active_root();
        for (source, target) in [
            (
                active.join("root"),
                self.managed_root.join("persistent/root"),
            ),
            (
                active.join("home/lingxi"),
                self.managed_root.join("persistent/lingxi"),
            ),
        ] {
            merge_home(&source, &target).map_err(|error| RootfsStoreError::Io {
                path: target,
                message: error.to_string(),
            })?;
        }
        if !marker.exists() {
            merge_home(
                &self.managed_root.join("persistent/root"),
                &self.managed_root.join("persistent/lingxi"),
            )
            .map_err(|error| RootfsStoreError::Io {
                path: marker.clone(),
                message: error.to_string(),
            })?;
            fs::create_dir_all(marker.parent().expect("persistent parent"))
                .and_then(|()| fs::write(&marker, b"1\n"))
                .map_err(|error| RootfsStoreError::Io {
                    path: marker,
                    message: error.to_string(),
                })?;
        }
        Ok(())
    }
}

fn merge_home(source: &Path, target: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if fs::symlink_metadata(target).is_ok() {
        if !metadata.is_dir()
            || !fs::symlink_metadata(target)?.is_dir()
            || fs::symlink_metadata(target)?.file_type().is_symlink()
        {
            return Ok(());
        }
    } else if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(source)?, target)?;
        return Ok(());
    } else if metadata.is_dir() {
        fs::create_dir_all(target)?;
    } else if metadata.is_file() {
        fs::copy(source, target)?;
        return Ok(());
    } else {
        return Ok(());
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        merge_home(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}
