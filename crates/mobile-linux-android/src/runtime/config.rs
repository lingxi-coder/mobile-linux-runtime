use std::path::PathBuf;

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
}

impl AndroidProotRuntimeConfig {
    pub(super) fn active_root(&self) -> PathBuf {
        self.managed_root.join("active")
    }

    pub(super) fn staged_root(&self) -> PathBuf {
        self.managed_root.join("staged").join(&self.rootfs_version)
    }
}
