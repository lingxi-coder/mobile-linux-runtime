use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MountPurpose, MountSpec, PtyOpenRequest,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{requested_memory_limit_bytes, AndroidProotRuntime, ForegroundMountMode};

pub(super) fn check_snapshot_cancelled(cancelled: &AtomicBool) -> Result<(), MobileLinuxError> {
    if cancelled.load(Ordering::Acquire) {
        Err(MobileLinuxError::InvalidRequest(
            "raw stdio snapshot startup cancelled".into(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn copy_raw_stdio_snapshot(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
) -> Result<(), MobileLinuxError> {
    check_snapshot_cancelled(cancelled)?;
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        MobileLinuxError::Io(format!(
            "inspect read-only LSP workspace {}: {error}",
            source.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(|error| {
            MobileLinuxError::Io(format!(
                "read LSP workspace symlink {}: {error}",
                source.display()
            ))
        })?;
        std::os::unix::fs::symlink(target, destination).map_err(|error| {
            MobileLinuxError::Io(format!(
                "copy LSP workspace symlink {}: {error}",
                source.display()
            ))
        })?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|error| {
            MobileLinuxError::Io(format!(
                "create LSP workspace snapshot {}: {error}",
                destination.display()
            ))
        })?;
        for entry in fs::read_dir(source).map_err(|error| {
            MobileLinuxError::Io(format!("read LSP workspace {}: {error}", source.display()))
        })? {
            let entry = entry.map_err(|error| MobileLinuxError::Io(error.to_string()))?;
            copy_raw_stdio_snapshot(
                &entry.path(),
                &destination.join(entry.file_name()),
                cancelled,
            )?;
        }
        fs::set_permissions(destination, metadata.permissions()).map_err(|error| {
            MobileLinuxError::Io(format!("preserve LSP snapshot permissions: {error}"))
        })?;
        return Ok(());
    }
    if metadata.is_file() {
        (|| -> std::io::Result<()> {
            let mut input = fs::File::open(source)?;
            let mut output = fs::File::create(destination)?;
            let mut bytes = [0; 64 * 1024];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "snapshot cancelled",
                    ));
                }
                let count = std::io::Read::read(&mut input, &mut bytes)?;
                if count == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &bytes[..count])?;
            }
            Ok(())
        })()
        .map_err(|error| {
            MobileLinuxError::Io(format!(
                "copy LSP workspace file {}: {error}",
                source.display()
            ))
        })?;
        fs::set_permissions(destination, metadata.permissions()).map_err(|error| {
            MobileLinuxError::Io(format!("preserve LSP snapshot permissions: {error}"))
        })?;
        return Ok(());
    }
    Err(MobileLinuxError::InvalidRequest(format!(
        "unsupported special file in LSP workspace snapshot: {}",
        source.display()
    )))
}

pub(super) fn snapshot_read_only_mounts(
    mounts: Vec<MountSpec>,
    snapshot_root: &Path,
    cancelled: &AtomicBool,
) -> Result<(Vec<MountSpec>, Vec<PathBuf>), MobileLinuxError> {
    check_snapshot_cancelled(cancelled)?;
    if snapshot_root.exists() {
        fs::remove_dir_all(snapshot_root).map_err(|error| {
            MobileLinuxError::Io(format!("clear stale LSP workspace snapshot: {error}"))
        })?;
    }
    let mut prepared = Vec::with_capacity(mounts.len());
    let mut roots = Vec::new();
    for (index, mut mount) in mounts.into_iter().enumerate() {
        check_snapshot_cancelled(cancelled)?;
        if mount.read_only {
            let destination = snapshot_root.join(index.to_string());
            let node_modules = mount.host_path.join("node_modules");
            if mount.host_path.is_dir() {
                fs::create_dir_all(&destination).map_err(|error| {
                    MobileLinuxError::Io(format!(
                        "create LSP workspace snapshot {}: {error}",
                        destination.display()
                    ))
                })?;
                for entry in fs::read_dir(&mount.host_path).map_err(|error| {
                    MobileLinuxError::Io(format!(
                        "read LSP workspace {}: {error}",
                        mount.host_path.display()
                    ))
                })? {
                    let entry = entry.map_err(|error| MobileLinuxError::Io(error.to_string()))?;
                    if entry.file_name() == "node_modules" {
                        continue;
                    }
                    copy_raw_stdio_snapshot(
                        &entry.path(),
                        &destination.join(entry.file_name()),
                        cancelled,
                    )?;
                }
            } else {
                copy_raw_stdio_snapshot(&mount.host_path, &destination, cancelled)?;
            }
            let guest_root = mount.guest_path.clone();
            mount.host_path = destination;
            mount.read_only = false;
            roots.push(snapshot_root.to_path_buf());
            prepared.push(mount);
            if node_modules.is_dir() {
                prepared.push(MountSpec {
                    host_path: node_modules,
                    guest_path: format!("{guest_root}/node_modules"),
                    read_only: false,
                    purpose: MountPurpose::External,
                });
            }
            continue;
        }
        prepared.push(mount);
    }
    roots.sort();
    roots.dedup();
    Ok((prepared, roots))
}

impl AndroidProotRuntime {
    pub(super) fn execution_mounts(
        &self,
        request_mounts: &[MountSpec],
        mode: ForegroundMountMode,
    ) -> Result<Vec<MountSpec>, MobileLinuxError> {
        let mut mounts = match mode {
            ForegroundMountMode::Merged => self
                .state
                .mounts
                .read()
                .expect("mobile-linux mounts rwlock")
                .clone(),
            ForegroundMountMode::ExplicitOnly => Vec::with_capacity(request_mounts.len()),
        };
        for mount in request_mounts {
            validate_mount(mount, &self.state.config.managed_root)?;
            mounts.retain(|existing| existing.guest_path != mount.guest_path);
            mounts.push(mount.clone());
        }
        Ok(mounts)
    }
}

pub(super) fn validate_request(request: &LinuxCommandRequest) -> Result<(), MobileLinuxError> {
    if request.command.trim().is_empty() || request.command.as_bytes().contains(&0) {
        return Err(MobileLinuxError::InvalidRequest(
            "command must not be empty or contain NUL".to_string(),
        ));
    }
    if matches!(request.timeout_ms, Some(0)) {
        return Err(MobileLinuxError::InvalidRequest(
            "timeout must be greater than zero".to_string(),
        ));
    }
    let _ = requested_memory_limit_bytes(request)?;
    validate_guest_path(request.cwd.as_deref().unwrap_or("/root"))?;
    for value in &request.args {
        if value.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(
                "argument contains NUL".to_string(),
            ));
        }
    }
    validate_env_map(&request.env)
}

pub(super) fn validate_pty_request(request: &PtyOpenRequest) -> Result<(), MobileLinuxError> {
    if request.command.trim().is_empty()
        || request.command.as_bytes().contains(&0)
        || request.size.cols == 0
        || request.size.rows == 0
    {
        return Err(MobileLinuxError::InvalidRequest(
            "PTY command and dimensions must be valid".to_string(),
        ));
    }
    for value in &request.args {
        if value.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(
                "PTY argument contains NUL".to_string(),
            ));
        }
    }
    validate_guest_path(request.cwd.as_deref().unwrap_or("/root"))?;
    validate_env_map(&request.env)
}

pub(super) fn validate_mount(
    mount: &MountSpec,
    managed_root: &Path,
) -> Result<(), MobileLinuxError> {
    if !mount.host_path.is_absolute() {
        return Err(MobileLinuxError::InvalidRequest(
            "mount host path must be absolute".to_string(),
        ));
    }
    if mount.read_only {
        return Err(MobileLinuxError::InvalidRequest(
            "Android PRoot does not enforce read-only bind mounts".to_string(),
        ));
    }
    validate_guest_path(&mount.guest_path)?;
    let host = mount.host_path.canonicalize().map_err(|error| {
        MobileLinuxError::InvalidRequest(format!(
            "mount host path is unavailable ({}): {error}",
            mount.host_path.display()
        ))
    })?;
    let managed = managed_root
        .canonicalize()
        .unwrap_or_else(|_| managed_root.to_path_buf());
    if host.starts_with(&managed) || managed.starts_with(&host) {
        return Err(MobileLinuxError::InvalidRequest(
            "mount must not expose the managed rootfs".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_env_map(env: &BTreeMap<String, String>) -> Result<(), MobileLinuxError> {
    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "invalid environment variable name: {key}"
            )));
        }
        if value.as_bytes().contains(&0) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "environment variable value contains NUL: {key}"
            )));
        }
        if is_host_reserved_env_var(key) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "host-reserved environment variable: {key}"
            )));
        }
    }
    Ok(())
}

pub(super) fn is_host_reserved_env_var(key: &str) -> bool {
    matches!(
        key,
        "HOME"
            | "PATH"
            | "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "PROOT_LOADER"
            | "PROOT_LOADER_32"
            | "PROOT_TMP_DIR"
    )
}

pub(super) fn validate_guest_path(path: &str) -> Result<(), MobileLinuxError> {
    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(MobileLinuxError::InvalidRequest(
            "guest path must be normalized and absolute".to_string(),
        ));
    }
    Ok(())
}
