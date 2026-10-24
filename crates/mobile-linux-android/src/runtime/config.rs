use mobile_linux_api::BrandRuntimeProfile;
use std::path::PathBuf;

#[derive(Debug, Clone)]
/// Paths and immutable identity expected by one managed Android PRoot runtime.
pub struct AndroidProotRuntimeConfig {
    /// App-private directory containing `active`, `staged`, `bin`, and `tmp`.
    pub managed_root: PathBuf,
    /// Authoritative product account, guest path, and config identity.
    pub brand_profile: BrandRuntimeProfile,
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
}

impl AndroidProotRuntimeConfig {
    pub(super) fn guest_home(&self) -> String {
        mobile_linux_api::guest_paths::home_for_profile(&self.brand_profile)
    }

    pub(super) fn workspace_root(&self) -> String {
        mobile_linux_api::guest_paths::workspace_root_for_profile(&self.brand_profile)
    }

    pub(super) fn persistent_home(&self) -> PathBuf {
        self.brand_profile
            .persistent_home_host_path(&self.managed_root)
    }

    pub(super) fn active_root(&self) -> PathBuf {
        self.managed_root.join("active")
    }

    pub(super) fn staged_root(&self) -> PathBuf {
        self.managed_root.join("staged").join(&self.rootfs_version)
    }
}
