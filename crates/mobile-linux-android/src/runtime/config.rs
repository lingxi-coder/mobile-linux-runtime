use mobile_linux_api::MobileLinuxError;
use std::path::PathBuf;

use super::validate_guest_path;

/// Caller-supplied isolated build layout. The SDK does not select a product profile.
#[derive(Debug, Clone)]
pub struct IsolatedBuildProfile {
    /// Absolute guest prefix containing app/channel/project directories.
    pub guest_root: String,
    /// Final guest project directory name.
    pub project_directory: String,
    /// Optional shared dependency store's guest mount path.
    pub dependency_store: String,
    /// Per-project directory containing HOME/TMP/XDG state.
    pub state_directory: String,
    /// Host application directory below app_sandbox_root.
    pub host_apps_directory: String,
    /// Host build directory below each application.
    pub host_build_directory: String,
    /// Host workspace directory below each application.
    pub host_workspace_directory: String,
    /// Accepted distribution/channel identifiers.
    pub channels: Vec<String>,
}

impl IsolatedBuildProfile {
    pub(super) fn validate(&self) -> Result<(), MobileLinuxError> {
        validate_guest_path(&self.guest_root)?;
        validate_guest_path(&self.dependency_store)?;
        for component in [
            &self.project_directory,
            &self.state_directory,
            &self.host_apps_directory,
            &self.host_build_directory,
            &self.host_workspace_directory,
        ]
        .into_iter()
        .chain(self.channels.iter())
        {
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.contains('/')
                || component.as_bytes().contains(&0)
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "build profile directories and channels must be single path components".into(),
                ));
            }
        }
        if self.channels.is_empty() {
            return Err(MobileLinuxError::InvalidRequest(
                "build profile requires at least one channel".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
/// Paths and immutable identity expected by one managed Android PRoot runtime.
pub struct AndroidProotRuntimeConfig {
    /// App-private directory containing `active`, `staged`, `bin`, and `tmp`.
    pub managed_root: PathBuf,
    /// Canonical app sandbox root (`Context.filesDir`) that owns app build roots.
    pub app_sandbox_root: PathBuf,
    /// Guest ABI label (`arm64-v8a` or `x86_64`).
    pub abi: String,
    /// Version directory selected below `staged`.
    pub rootfs_version: String,
    /// Expected source archive digest surfaced in status/diagnostics.
    pub archive_sha256: Option<String>,
    /// Android Context.applicationInfo.nativeLibraryDir, supplied by the caller.
    pub native_library_dir: Option<PathBuf>,
    /// Optional explicit product build profile; absent profiles reject isolated builds.
    pub isolated_build_profile: Option<IsolatedBuildProfile>,
}

impl AndroidProotRuntimeConfig {
    pub(super) fn active_root(&self) -> PathBuf {
        self.managed_root.join("active")
    }

    pub(super) fn staged_root(&self) -> PathBuf {
        self.managed_root.join("staged").join(&self.rootfs_version)
    }
}
