use mobile_linux_api::{MountPurpose, MountSpec};
use std::path::PathBuf;

use super::NativeConfigPayload;

/// Immutable iSH runtime identity and path configuration supplied by the iOS
/// framework bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosIshRuntimeConfig {
    /// App-private directory that holds active/staged iSH rootfs state.
    pub managed_root: PathBuf,
    /// Canonical app sandbox root used to protect `.lingxi` and config trees.
    pub app_sandbox_root: PathBuf,
    /// Host workspace path exposed as the default `/workspace/<id>` mount.
    pub workspace_host_path: PathBuf,
    /// Stable guest workspace identifier appended under `/workspace`.
    pub stable_workspace_id: String,
    /// Guest ABI label surfaced in status responses.
    pub abi: String,
    /// Expected rootfs version label.
    pub rootfs_version: String,
    /// Optional archive digest surfaced in status responses.
    pub archive_sha256: Option<String>,
    /// Optional authorization file forwarded to the native bridge.
    pub authorization_file: Option<String>,
    /// Explicit host filesystem path of the rootfs ZIP; no bundle or environment lookup.
    pub rootfs_archive_path: Option<PathBuf>,
    /// Optional explicit directory of initial shared guest files.
    pub default_mount_path: Option<PathBuf>,
    /// Optional explicit rootfs overlay bundle; no application-bundle lookup.
    pub rootfs_patch_path: Option<PathBuf>,
    /// Host directories that request mounts may never expose, including ancestors.
    pub protected_host_roots: Vec<PathBuf>,
    /// Additional host roots allowed for request mounts beyond the workspace.
    pub allowed_mount_roots: Vec<PathBuf>,
    /// Guest prefixes allowed for non-workspace request mounts.
    pub allowed_guest_roots: Vec<String>,
}

impl IosIshRuntimeConfig {
    pub(super) fn active_root(&self) -> PathBuf {
        self.managed_root.join("alpine-rootfs")
    }

    pub(super) fn workspace_guest_path(&self) -> String {
        mobile_linux_api::guest_paths::workspace(&self.stable_workspace_id)
    }

    pub(super) fn persistent_home_host_path(&self) -> PathBuf {
        self.managed_root.join("persistent/root")
    }

    pub(super) fn default_workspace_mount(&self) -> MountSpec {
        MountSpec {
            host_path: self.workspace_host_path.clone(),
            guest_path: self.workspace_guest_path(),
            read_only: false,
            purpose: MountPurpose::Workspace,
        }
    }

    pub(super) fn persistent_home_mount(&self) -> MountSpec {
        MountSpec {
            host_path: self.persistent_home_host_path(),
            guest_path: mobile_linux_api::guest_paths::HOME.to_string(),
            read_only: false,
            purpose: MountPurpose::Shared,
        }
    }

    pub(super) fn native_payload(&self) -> NativeConfigPayload {
        NativeConfigPayload {
            managed_root: self.managed_root.display().to_string(),
            workspace_host_path: self.workspace_host_path.display().to_string(),
            stable_workspace_id: self.stable_workspace_id.clone(),
            abi: self.abi.clone(),
            rootfs_version: self.rootfs_version.clone(),
            archive_sha256: self.archive_sha256.clone(),
            authorization_file: self.authorization_file.clone(),
            rootfs_archive_path: self
                .rootfs_archive_path
                .as_ref()
                .map(|p| p.display().to_string()),
            rootfs_patch_path: self
                .rootfs_patch_path
                .as_ref()
                .map(|p| p.display().to_string()),
            default_mount_path: self
                .default_mount_path
                .as_ref()
                .map(|p| p.display().to_string()),
        }
    }
}
