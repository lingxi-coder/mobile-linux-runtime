use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MountPurpose, MountSpec, PtyOpenRequest,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    requested_memory_limit_bytes, AndroidProotRuntime, ForegroundMountMode, IsolatedBuildProfile,
};

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
        if matches!(mode, ForegroundMountMode::RequestOnly) {
            validate_isolated_local_app_mounts(
                request_mounts,
                &self.state.config.managed_root,
                &self.state.config.app_sandbox_root,
                self.state
                    .config
                    .isolated_build_profile
                    .as_ref()
                    .ok_or_else(|| {
                        MobileLinuxError::InvalidRequest(
                            "isolated build profile is not configured".into(),
                        )
                    })?,
            )?;
        }
        let mut mounts = match mode {
            ForegroundMountMode::Merged => self
                .state
                .mounts
                .read()
                .expect("mobile-linux mounts rwlock")
                .clone(),
            ForegroundMountMode::RequestOnly | ForegroundMountMode::ExplicitOnly => {
                Vec::with_capacity(request_mounts.len())
            }
        };
        for mount in request_mounts {
            validate_mount(mount, &self.state.config.managed_root)?;
            mounts.retain(|existing| existing.guest_path != mount.guest_path);
            mounts.push(mount.clone());
        }
        Ok(mounts)
    }
}

pub(super) fn validate_request(
    request: &LinuxCommandRequest,
    isolated_local_app_build_mounts: Option<&[MountSpec]>,
    profile: Option<&IsolatedBuildProfile>,
) -> Result<(), MobileLinuxError> {
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
    validate_env_map(&request.env, isolated_local_app_build_mounts, profile)
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
    validate_env_map(&request.env, None, None)
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

pub(super) fn validate_isolated_local_app_mounts(
    mounts: &[MountSpec],
    managed_root: &Path,
    app_sandbox_root: &Path,
    profile: &IsolatedBuildProfile,
) -> Result<(), MobileLinuxError> {
    profile.validate()?;
    let build_mounts: Vec<_> = mounts
        .iter()
        .filter(|mount| matches!(mount.purpose, MountPurpose::LocalAppBuild))
        .collect();
    let store_mounts: Vec<_> = mounts
        .iter()
        .filter(|mount| {
            matches!(mount.purpose, MountPurpose::Shared)
                && mount.guest_path == profile.dependency_store
        })
        .collect();
    if build_mounts.len() != 1 || mounts.len() != 1 + store_mounts.len() || store_mounts.len() > 1 {
        return Err(MobileLinuxError::InvalidRequest(
            "isolated local-app execution requires exactly one LocalAppBuild mount and at most one validated dependency store mount".to_string(),
        ));
    }
    for mount in &store_mounts {
        let host = mount.host_path.canonicalize().map_err(|error| {
            MobileLinuxError::InvalidRequest(format!(
                "dependency store mount is unavailable ({}): {error}",
                mount.host_path.display()
            ))
        })?;
        if !host.starts_with(app_sandbox_root) {
            return Err(MobileLinuxError::InvalidRequest(
                "dependency store mount must remain inside the app sandbox".to_string(),
            ));
        }
    }
    let mount = build_mounts[0];
    let host_path = mount.host_path.canonicalize().map_err(|error| {
        MobileLinuxError::InvalidRequest(format!(
            "mount host path is unavailable ({}): {error}",
            mount.host_path.display()
        ))
    })?;
    let managed_root = managed_root
        .canonicalize()
        .unwrap_or_else(|_| managed_root.to_path_buf());
    if host_path.starts_with(&managed_root) || managed_root.starts_with(&host_path) {
        return Err(MobileLinuxError::InvalidRequest(
            "mount must not expose the managed rootfs".to_string(),
        ));
    }
    let (app_id, channel) = parse_local_app_build_guest_path(&mount.guest_path, profile)?;
    let sandbox_root = app_sandbox_root
        .canonicalize()
        .unwrap_or_else(|_| app_sandbox_root.to_path_buf());
    let expected = sandbox_root
        .join(&profile.host_apps_directory)
        .join(app_id)
        .join(&profile.host_build_directory)
        .join(channel);
    let workspace = sandbox_root
        .join(&profile.host_apps_directory)
        .join(app_id)
        .join(&profile.host_workspace_directory);
    if host_path != workspace
        && host_path != expected
        && !local_app_build_host_path_matches(&host_path, &expected, channel)
    {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "local-app build mount host_path must match {} or workspace {} or its .{channel}.staging-<numeric nonce> sibling (got {})",
            expected.display(), workspace.display(),
            host_path.display()
        )));
    }
    Ok(())
}

pub(super) fn local_app_build_host_path_matches(
    host_path: &Path,
    expected_host_path: &Path,
    channel: &str,
) -> bool {
    if host_path == expected_host_path {
        return true;
    }
    if host_path.parent() != expected_host_path.parent() {
        return false;
    }
    let staging_prefix = format!(".{channel}.staging-");
    let Some(nonce) = host_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(staging_prefix.as_str()))
    else {
        return false;
    };
    !nonce.is_empty() && nonce.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn parse_local_app_build_guest_path<'a>(
    path: &'a str,
    profile: &IsolatedBuildProfile,
) -> Result<(&'a str, &'a str), MobileLinuxError> {
    profile.validate()?;
    let relative = path
        .strip_prefix(profile.guest_root.as_str())
        .and_then(|suffix| suffix.strip_prefix('/'))
        .ok_or_else(|| {
            MobileLinuxError::InvalidRequest(format!(
                "local-app build guest_path must be {}/<app-id>/<channel>/project",
                profile.guest_root.as_str()
            ))
        })?;
    let mut segments = relative.split('/');
    let app_id = segments.next().unwrap_or_default();
    let channel = segments.next().unwrap_or_default();
    let project = segments.next().unwrap_or_default();
    if segments.next().is_some()
        || !is_valid_local_app_id(app_id)
        || !profile.channels.iter().any(|allowed| allowed == channel)
        || project != profile.project_directory
    {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "local-app build guest_path must be {}/<app-id>/<store|full>/project",
            profile.guest_root.as_str()
        )));
    }
    Ok((app_id, channel))
}

pub(super) fn is_valid_local_app_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

pub(super) fn validate_env_map(
    env: &BTreeMap<String, String>,
    isolated_local_app_build_mounts: Option<&[MountSpec]>,
    profile: Option<&IsolatedBuildProfile>,
) -> Result<(), MobileLinuxError> {
    let fixed_local_app_build_env = isolated_local_app_build_mounts
        .map(|mounts| {
            expected_local_app_build_env(
                mounts,
                profile.ok_or_else(|| {
                    MobileLinuxError::InvalidRequest(
                        "isolated build profile is not configured".into(),
                    )
                })?,
            )
        })
        .transpose()?;
    if let Some(expected_env) = fixed_local_app_build_env.as_ref() {
        for (key, expected_value) in expected_env {
            match env.get(key) {
                Some(value) if value == expected_value => {}
                Some(_) => {
                    return Err(MobileLinuxError::InvalidRequest(format!(
                        "isolated local-app build environment variable {key} must equal {expected_value}"
                    )))
                }
                None => {
                    return Err(MobileLinuxError::InvalidRequest(format!(
                        "isolated local-app build requires environment variable {key}"
                    )))
                }
            }
        }
    }
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
        if let Some(expected_value) = fixed_local_app_build_env
            .as_ref()
            .and_then(|expected| expected.get(key))
        {
            debug_assert_eq!(value, expected_value);
            continue;
        }
        if is_host_reserved_env_var(key) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "host-reserved environment variable: {key}"
            )));
        }
    }
    Ok(())
}

pub(super) fn expected_local_app_build_env(
    mounts: &[MountSpec],
    profile: &IsolatedBuildProfile,
) -> Result<BTreeMap<String, String>, MobileLinuxError> {
    let mount = mounts
        .iter()
        .find(|mount| matches!(mount.purpose, MountPurpose::LocalAppBuild))
        .ok_or_else(|| {
            MobileLinuxError::InvalidRequest("missing LocalAppBuild mount".to_string())
        })?;
    parse_local_app_build_guest_path(&mount.guest_path, profile)?;
    let build_state_root = format!("{}/{}", mount.guest_path, profile.state_directory);
    Ok(BTreeMap::from([
        ("HOME".into(), format!("{build_state_root}/home")),
        ("TMPDIR".into(), format!("{build_state_root}/tmp")),
        ("TMP".into(), format!("{build_state_root}/tmp")),
        ("TEMP".into(), format!("{build_state_root}/tmp")),
        (
            "XDG_CACHE_HOME".into(),
            format!("{build_state_root}/xdg-cache"),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            format!("{build_state_root}/xdg-config"),
        ),
        (
            "XDG_DATA_HOME".into(),
            format!("{build_state_root}/xdg-data"),
        ),
    ]))
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
