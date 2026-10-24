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
    /// Preserve account homes outside replaceable images. Each legacy home is
    /// copied only into the persistent directory named by that image's own
    /// account profile; the source is retained and a per-user recovery record
    /// is written before activation can replace the image.
    pub fn preserve_guest_homes(&self) -> Result<(), RootfsStoreError> {
        let active = self.active_root();
        let Some(metadata) = fs::symlink_metadata(&active).ok() else {
            return Ok(());
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RootfsStoreError::UnsafeManagedPath(active));
        }
        let source_linux_user = legacy_active_linux_user(&active)?;
        let migration_root = self.managed_root.join("persistent/migrations");
        fs::create_dir_all(&migration_root).map_err(|error| RootfsStoreError::Io {
            path: migration_root.clone(),
            message: error.to_string(),
        })?;
        for (guest_path, source, target) in [
            (
                "/root".to_string(),
                active.join("root"),
                self.managed_root.join("persistent/root"),
            ),
            (
                format!("/home/{source_linux_user}"),
                active.join("home").join(&source_linux_user),
                self.managed_root
                    .join("persistent")
                    .join(&source_linux_user),
            ),
        ] {
            if fs::symlink_metadata(&source).is_err() {
                continue;
            }
            merge_home(&source, &target).map_err(|error| RootfsStoreError::Io {
                path: target.clone(),
                message: error.to_string(),
            })?;
            let record =
                migration_root.join(format!("guest-home-v1-{}.json", migration_key(&guest_path)));
            let payload = serde_json::json!({
                "schema_version": 1,
                "source_guest_path": guest_path,
                "destination_managed_relative_path": relative_managed_path(&self.managed_root, &target)?,
                "source_retained": true
            });
            let bytes = serde_json::to_vec_pretty(&payload).map_err(|error| {
                RootfsStoreError::Integrity(format!("encode guest-home migration record: {error}"))
            })?;
            let temporary = record.with_extension(format!("json.tmp-{}", std::process::id()));
            fs::write(&temporary, bytes)
                .and_then(|()| fs::rename(&temporary, &record))
                .map_err(|error| {
                    let _ = fs::remove_file(&temporary);
                    RootfsStoreError::Io {
                        path: record.clone(),
                        message: error.to_string(),
                    }
                })?;
        }
        Ok(())
    }
}

fn legacy_active_linux_user(active: &Path) -> Result<String, RootfsStoreError> {
    let manifest_path = active.join("rootfs-manifest.json");
    match fs::read(&manifest_path) {
        Ok(bytes) => {
            let manifest = serde_json::from_slice::<RootfsManifest>(&bytes).map_err(|error| {
                RootfsStoreError::Integrity(format!(
                    "read active rootfs identity from {}: {error}",
                    manifest_path.display()
                ))
            })?;
            let profile = manifest.brand_profile.unwrap_or_default();
            profile.validate().map_err(|error| {
                RootfsStoreError::Integrity(format!(
                    "active rootfs has invalid brand profile: {error}"
                ))
            })?;
            Ok(profile.linux_user)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Older images did not embed their manifest. Recover the uid-1000
            // account name from the actual passwd file, so an account from a
            // former brand is preserved under its own persistent/<user> key.
            let passwd_path = active.join("etc/passwd");
            let passwd = match fs::read_to_string(&passwd_path) {
                Ok(passwd) => passwd,
                Err(read_error) if read_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok("lingxi".to_string());
                }
                Err(read_error) => {
                    return Err(RootfsStoreError::Io {
                        path: passwd_path,
                        message: read_error.to_string(),
                    });
                }
            };
            let users = passwd
                .lines()
                .filter_map(|line| {
                    let fields = line.split(':').collect::<Vec<_>>();
                    (fields.len() >= 7 && fields[2] == "1000" && fields[3] == "1000")
                        .then(|| fields[0].to_string())
                })
                .collect::<Vec<_>>();
            match users.as_slice() {
                [user] => {
                    mobile_linux_api::BrandRuntimeProfile::new("legacy", user, ".legacy")
                        .validate()
                        .map_err(|error| {
                            RootfsStoreError::Integrity(format!(
                                "active rootfs has an unsafe uid-1000 account: {error}"
                            ))
                        })?;
                    Ok(user.clone())
                }
                [] => Ok("lingxi".to_string()),
                _ => Err(RootfsStoreError::Integrity(
                    "active rootfs has ambiguous uid-1000 accounts; refusing home migration"
                        .to_string(),
                )),
            }
        }
        Err(error) => Err(RootfsStoreError::Io {
            path: manifest_path,
            message: error.to_string(),
        }),
    }
}

fn migration_key(guest_path: &str) -> String {
    guest_path.trim_matches('/').replace('/', "-")
}

fn relative_managed_path(managed_root: &Path, target: &Path) -> Result<String, RootfsStoreError> {
    target
        .strip_prefix(managed_root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| RootfsStoreError::UnsafeManagedPath(target.to_path_buf()))
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
