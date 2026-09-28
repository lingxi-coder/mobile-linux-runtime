//! Managed-root identity, paths, and status.
use crate::filesystem::{dir_size, ensure_real_directory, path_present};
use crate::{RootfsManifest, RootfsStoreError};
use mobile_linux_api::{MobileLinuxRuntimeMode, RootfsState, RootfsStatus, SandboxBackend};
use std::fs;
use std::path::{Path, PathBuf};
use verification::verification_failure_message;
mod install;
mod lifecycle;
mod verification;
pub use verification::{RootfsVerificationIssue, RootfsVerificationReport};

pub struct RootfsStore {
    managed_root: PathBuf,
    backend: SandboxBackend,
    mode: MobileLinuxRuntimeMode,
    platform: String,
    abi: String,
}

fn abis_match(manifest_abi: &str, store_abi: &str) -> bool {
    fn normalize(abi: &str) -> Option<&'static str> {
        match abi {
            "arm64" | "arm64-v8a" | "aarch64" => Some("arm64"),
            "x86_64" => Some("x86_64"),
            _ => None,
        }
    }
    normalize(manifest_abi)
        .zip(normalize(store_abi))
        .is_some_and(|(manifest, store)| manifest == store)
}
impl RootfsStore {
    #[must_use]
    pub fn new(
        managed_root: PathBuf,
        backend: SandboxBackend,
        mode: MobileLinuxRuntimeMode,
        platform: impl Into<String>,
        abi: impl Into<String>,
    ) -> Self {
        Self {
            managed_root,
            backend,
            mode,
            platform: platform.into(),
            abi: abi.into(),
        }
    }

    #[must_use]
    pub fn managed_root(&self) -> &Path {
        &self.managed_root
    }

    #[must_use]
    pub fn active_root(&self) -> PathBuf {
        self.managed_root.join("active")
    }

    #[must_use]
    pub fn staged_root(&self, version: &str) -> PathBuf {
        self.managed_root.join("staged").join(version)
    }

    #[must_use]
    pub fn status(&self, manifest: &RootfsManifest) -> RootfsStatus {
        let active_root = self.active_root();
        let mut last_error = self
            .validate_manifest_target(manifest)
            .err()
            .map(|error| error.to_string());
        let staged_root = if last_error.is_none() {
            self.staged_root(&manifest.rootfs_version)
        } else {
            self.managed_root.join("staged").join("invalid-manifest")
        };
        let active_present = path_present(&active_root);
        let staged_present = path_present(&staged_root);
        let state = if last_error.is_some() {
            RootfsState::Corrupt
        } else if active_present {
            match self.verify_active_root(manifest) {
                Ok(report) if report.ok => RootfsState::Ready,
                Ok(report) => {
                    last_error = Some(verification_failure_message(&report));
                    RootfsState::Corrupt
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    RootfsState::Corrupt
                }
            }
        } else if staged_present {
            RootfsState::Installing
        } else {
            RootfsState::Missing
        };
        RootfsStatus {
            state,
            backend: self.backend,
            mode: self.mode,
            platform: self.platform.clone(),
            abi: self.abi.clone(),
            version: Some(manifest.rootfs_version.clone()),
            managed_root: Some(self.managed_root.clone()),
            active_root: active_present.then_some(active_root),
            staged_root: staged_present.then_some(staged_root),
            archive_sha256: Some(manifest.archive.sha256.clone()),
            installed_size_bytes: dir_size(self.active_root()).ok(),
            writable_guest_paths: manifest.writable_paths.clone(),
            last_error,
        }
    }

    fn ensure_managed_root(&self) -> Result<(), RootfsStoreError> {
        fs::create_dir_all(&self.managed_root).map_err(|err| RootfsStoreError::Io {
            path: self.managed_root.clone(),
            message: err.to_string(),
        })?;
        ensure_real_directory(&self.managed_root)
    }

    fn validate_manifest_target(&self, manifest: &RootfsManifest) -> Result<(), RootfsStoreError> {
        manifest.validate()?;
        let expected_runtime = match self.backend {
            SandboxBackend::AndroidProot => "android-proot",
            SandboxBackend::IosIsh => "ios-ish",
            _ => {
                return Err(RootfsStoreError::TargetMismatch {
                    store: format!("{:?}/{}/{}", self.backend, self.platform, self.abi),
                    manifest: format!(
                        "{}/{}/{}",
                        manifest.runtime, manifest.platform, manifest.abi
                    ),
                });
            }
        };
        if manifest.runtime != expected_runtime
            || manifest.platform != self.platform
            || !abis_match(&manifest.abi, &self.abi)
        {
            return Err(RootfsStoreError::TargetMismatch {
                store: format!("{expected_runtime}/{}/{}", self.platform, self.abi),
                manifest: format!(
                    "{}/{}/{}",
                    manifest.runtime, manifest.platform, manifest.abi
                ),
            });
        }
        Ok(())
    }
}
