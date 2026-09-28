//! Validate archive bytes, rootfs inventories, and executable authorization.
use super::RootfsStore;
use crate::filesystem::{sha256_bytes, sha256_file};
use crate::guest_path::secure_guest_host_path_with_real_parents;
use crate::manifest::ALLOWED_WRITABLE_GUEST_PATHS;
use crate::{
    path_is_within_guest_path, validate_guest_path, RootfsImmutableEntry, RootfsImmutableKind,
    RootfsManifest, RootfsStoreError,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootfsVerificationReport {
    pub ok: bool,
    pub verified_files: usize,
    pub verified_bytes: u64,
    pub issues: Vec<RootfsVerificationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootfsVerificationIssue {
    pub path: String,
    pub reason: String,
}

pub(super) fn verification_failure_message(report: &RootfsVerificationReport) -> String {
    let mut reasons = report
        .issues
        .iter()
        .take(3)
        .map(|issue| format!("{}: {}", issue.path, issue.reason))
        .collect::<Vec<_>>();
    if report.issues.len() > reasons.len() {
        reasons.push(format!(
            "{} additional issue(s)",
            report.issues.len() - reasons.len()
        ));
    }
    format!("rootfs verification failed: {}", reasons.join("; "))
}

enum ImmutableEntryVerification {
    Verified(u64),
    Failed(String),
}

fn verify_writable_root(root: &Path, guest_path: &str) -> Option<String> {
    let host_path = match secure_guest_host_path_with_real_parents(root, guest_path) {
        Ok(path) => path,
        Err(RootfsStoreError::UnsafeManagedPath(path)) => {
            return Some(format!(
                "writable root has a symlinked or invalid ancestor ({})",
                path.display()
            ));
        }
        Err(error) => return Some(error.to_string()),
    };
    match fs::symlink_metadata(&host_path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => None,
        Ok(_) => Some("writable root must be a real directory, not a symlink or file".to_string()),
        Err(error) => Some(format!("writable root is missing: {error}")),
    }
}

fn verify_immutable_entry(
    root: &Path,
    entry: &RootfsImmutableEntry,
) -> Result<ImmutableEntryVerification, RootfsStoreError> {
    let host_path = match secure_guest_host_path_with_real_parents(root, &entry.path) {
        Ok(path) => path,
        Err(RootfsStoreError::UnsafeManagedPath(path)) => {
            return Ok(ImmutableEntryVerification::Failed(format!(
                "symlinked or invalid ancestor blocks verification ({})",
                path.display()
            )));
        }
        Err(error) => return Ok(ImmutableEntryVerification::Failed(error.to_string())),
    };
    let metadata = match fs::symlink_metadata(&host_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Ok(ImmutableEntryVerification::Failed(format!(
                "missing: {error}"
            )));
        }
    };
    match entry.kind {
        RootfsImmutableKind::RegularFile => verify_immutable_regular(&host_path, &metadata, entry),
        RootfsImmutableKind::Symlink => verify_immutable_symlink(&host_path, &metadata, entry),
    }
}

fn verify_immutable_regular(
    host_path: &Path,
    metadata: &fs::Metadata,
    entry: &RootfsImmutableEntry,
) -> Result<ImmutableEntryVerification, RootfsStoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(ImmutableEntryVerification::Failed(
            "immutable regular-file path is a symlink or has the wrong file type".to_string(),
        ));
    }
    if metadata.len() != entry.size_bytes {
        return Ok(ImmutableEntryVerification::Failed(format!(
            "size mismatch (expected {}, got {})",
            entry.size_bytes,
            metadata.len()
        )));
    }
    let actual = sha256_file(host_path)?;
    if actual != entry.sha256 {
        return Ok(ImmutableEntryVerification::Failed(format!(
            "hash mismatch (expected {}, got {})",
            entry.sha256, actual
        )));
    }
    Ok(ImmutableEntryVerification::Verified(entry.size_bytes))
}

fn verify_immutable_symlink(
    host_path: &Path,
    metadata: &fs::Metadata,
    entry: &RootfsImmutableEntry,
) -> Result<ImmutableEntryVerification, RootfsStoreError> {
    if !metadata.file_type().is_symlink() {
        return Ok(ImmutableEntryVerification::Failed(
            "immutable symlink path has the wrong file type".to_string(),
        ));
    }
    let target = fs::read_link(host_path).map_err(|err| RootfsStoreError::Io {
        path: host_path.to_path_buf(),
        message: err.to_string(),
    })?;
    if !symlink_target_stays_within_root(&entry.path, &target) {
        return Ok(ImmutableEntryVerification::Failed(format!(
            "symlink target escapes the rootfs: {}",
            target.display()
        )));
    }
    let target_bytes = target.as_os_str().as_encoded_bytes();
    if target_bytes.len() as u64 != entry.size_bytes {
        return Ok(ImmutableEntryVerification::Failed(format!(
            "symlink target size mismatch (expected {}, got {})",
            entry.size_bytes,
            target_bytes.len()
        )));
    }
    let actual = sha256_bytes(target_bytes);
    if actual != entry.sha256 {
        return Ok(ImmutableEntryVerification::Failed(format!(
            "hash mismatch (expected {}, got {})",
            entry.sha256, actual
        )));
    }
    Ok(ImmutableEntryVerification::Verified(entry.size_bytes))
}

fn symlink_target_stays_within_root(guest_path: &str, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = Path::new(guest_path)
        .parent()
        .map_or(0, |parent| parent.components().count().saturating_sub(1));
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn collect_immutable_rootfs_paths(
    root: &Path,
    writable_paths: &[String],
) -> Result<Vec<String>, RootfsStoreError> {
    fn visit(
        root: &Path,
        directory: &Path,
        writable_paths: &[String],
        paths: &mut Vec<String>,
    ) -> Result<(), RootfsStoreError> {
        for entry in fs::read_dir(directory).map_err(|err| RootfsStoreError::Io {
            path: directory.to_path_buf(),
            message: err.to_string(),
        })? {
            let entry = entry.map_err(|err| RootfsStoreError::Io {
                path: directory.to_path_buf(),
                message: err.to_string(),
            })?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| RootfsStoreError::UnsafeManagedPath(path.clone()))?;
            let guest_path = format!(
                "/{}",
                relative
                    .components()
                    .filter_map(|component| match component {
                        Component::Normal(segment) => Some(segment.to_string_lossy()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/")
            );
            if writable_paths
                .iter()
                .any(|writable| path_is_within_guest_path(&guest_path, writable))
            {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|err| RootfsStoreError::Io {
                path: path.clone(),
                message: err.to_string(),
            })?;
            if metadata.is_dir() {
                visit(root, &path, writable_paths, paths)?;
            } else {
                paths.push(guest_path);
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    visit(root, root, writable_paths, &mut paths)?;
    paths.sort();
    Ok(paths)
}
impl RootfsStore {
    pub fn verify_archive(
        &self,
        archive_path: &Path,
        manifest: &RootfsManifest,
    ) -> Result<(), RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        let metadata = fs::symlink_metadata(archive_path).map_err(|err| RootfsStoreError::Io {
            path: archive_path.to_path_buf(),
            message: err.to_string(),
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RootfsStoreError::UnsafeManagedPath(
                archive_path.to_path_buf(),
            ));
        }
        if metadata.len() != manifest.archive.size_bytes {
            return Err(RootfsStoreError::ArchiveSizeMismatch {
                expected: manifest.archive.size_bytes,
                actual: metadata.len(),
            });
        }
        let actual = sha256_file(archive_path)?;
        if actual != manifest.archive.sha256 {
            return Err(RootfsStoreError::ArchiveHashMismatch {
                expected: manifest.archive.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }

    pub fn verify_active_root(
        &self,
        manifest: &RootfsManifest,
    ) -> Result<RootfsVerificationReport, RootfsStoreError> {
        self.verify_root_at(&self.active_root(), manifest)
    }

    pub(super) fn verify_root_at(
        &self,
        root: &Path,
        manifest: &RootfsManifest,
    ) -> Result<RootfsVerificationReport, RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        let active_root = root.to_path_buf();
        let active_metadata = match fs::symlink_metadata(&active_root) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RootfsVerificationReport {
                    ok: false,
                    verified_files: 0,
                    verified_bytes: 0,
                    issues: vec![RootfsVerificationIssue {
                        path: "/".to_string(),
                        reason: "active rootfs is missing".to_string(),
                    }],
                });
            }
            Err(err) => {
                return Err(RootfsStoreError::Io {
                    path: active_root.clone(),
                    message: err.to_string(),
                });
            }
        };
        if active_metadata.file_type().is_symlink() || !active_metadata.is_dir() {
            return Ok(RootfsVerificationReport {
                ok: false,
                verified_files: 0,
                verified_bytes: 0,
                issues: vec![RootfsVerificationIssue {
                    path: "/".to_string(),
                    reason: "active rootfs must be a real directory, not a symlink".to_string(),
                }],
            });
        }

        let mut issues = Vec::new();
        let mut verified_files = 0usize;
        let mut verified_bytes = 0u64;
        for writable_path in &manifest.writable_paths {
            if let Some(reason) = verify_writable_root(&active_root, writable_path) {
                issues.push(RootfsVerificationIssue {
                    path: writable_path.clone(),
                    reason,
                });
            }
        }
        for entry in &manifest.immutable_files {
            match verify_immutable_entry(&active_root, entry)? {
                ImmutableEntryVerification::Verified(bytes) => {
                    verified_files += 1;
                    verified_bytes += bytes;
                }
                ImmutableEntryVerification::Failed(reason) => {
                    issues.push(RootfsVerificationIssue {
                        path: entry.path.clone(),
                        reason,
                    });
                }
            }
        }

        let expected_paths = manifest
            .immutable_files
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<BTreeSet<_>>();
        for actual_path in collect_immutable_rootfs_paths(&active_root, &manifest.writable_paths)? {
            if !expected_paths.contains(actual_path.as_str()) {
                issues.push(RootfsVerificationIssue {
                    path: actual_path,
                    reason: "file is absent from immutable inventory".to_string(),
                });
            }
        }

        Ok(RootfsVerificationReport {
            ok: issues.is_empty(),
            verified_files,
            verified_bytes,
            issues,
        })
    }

    pub fn authorize_manifest_file(
        &self,
        guest_path: &str,
        manifest: &RootfsManifest,
    ) -> Result<PathBuf, RootfsStoreError> {
        self.validate_manifest_target(manifest)?;
        validate_guest_path(guest_path)?;
        if ALLOWED_WRITABLE_GUEST_PATHS
            .iter()
            .any(|writable| path_is_within_guest_path(guest_path, writable))
        {
            return Err(RootfsStoreError::ExecutionDenied {
                path: guest_path.to_string(),
                reason: "execution and dynamic loading from writable paths are forbidden"
                    .to_string(),
            });
        }
        let Some(entry) = manifest
            .executable_allowlist
            .iter()
            .find(|entry| entry.path == guest_path)
        else {
            return Err(RootfsStoreError::ExecutionDenied {
                path: guest_path.to_string(),
                reason: "path is absent from the rootfs manifest allowlist".to_string(),
            });
        };
        let host_path = secure_guest_host_path_with_real_parents(&self.active_root(), guest_path)
            .map_err(|error| RootfsStoreError::ExecutionDenied {
            path: guest_path.to_string(),
            reason: error.to_string(),
        })?;
        let metadata = fs::symlink_metadata(&host_path).map_err(|err| RootfsStoreError::Io {
            path: host_path.clone(),
            message: err.to_string(),
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RootfsStoreError::ExecutionDenied {
                path: guest_path.to_string(),
                reason: "allowlisted path is not a regular non-symlink file".to_string(),
            });
        }
        if entry
            .size_bytes
            .is_some_and(|expected| expected != metadata.len())
        {
            return Err(RootfsStoreError::ExecutionDenied {
                path: guest_path.to_string(),
                reason: "allowlisted file size no longer matches".to_string(),
            });
        }
        let actual = sha256_file(&host_path)?;
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            return Err(RootfsStoreError::ExecutionDenied {
                path: guest_path.to_string(),
                reason: "allowlisted file hash no longer matches".to_string(),
            });
        }
        Ok(host_path)
    }
}
