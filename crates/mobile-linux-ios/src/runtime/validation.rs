use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MountPurpose, MountSpec, PtyOpenRequest,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::IosIshRuntimeConfig;

const MAX_AUTHORIZED_PROJECT_MOUNTS: usize = 64;

pub(super) fn validate_request(request: &LinuxCommandRequest) -> Result<(), MobileLinuxError> {
    if request.command.trim().is_empty() {
        return Err(MobileLinuxError::InvalidRequest(
            "command must not be empty".to_string(),
        ));
    }
    validate_no_nul(&request.command, "command")?;
    validate_args(&request.args)?;
    if let Some(cwd) = &request.cwd {
        validate_no_nul(cwd, "cwd")?;
        validate_guest_path(cwd, "cwd", true)?;
    }
    validate_env(&request.env)?;
    if let Some(stdin) = &request.stdin {
        validate_no_nul(stdin, "stdin")?;
    }
    let limits = request.resource_limits;
    if limits.max_cpu_seconds.is_some()
        || limits.max_processes.is_some()
        || limits.max_open_files.is_some()
    {
        return Err(MobileLinuxError::ResourceLimitExceeded(
            "ios-ish runtime supports only the per-execution memory limit".to_string(),
        ));
    }
    if matches!(limits.max_memory_mb, Some(0)) {
        return Err(MobileLinuxError::InvalidRequest(
            "max_memory_mb must be greater than zero".to_string(),
        ));
    }
    if matches!(request.timeout_ms, Some(0)) {
        return Err(MobileLinuxError::InvalidRequest(
            "timeout_ms must be greater than zero when provided".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_pty_request(request: &PtyOpenRequest) -> Result<(), MobileLinuxError> {
    if request.command.trim().is_empty() {
        return Err(MobileLinuxError::InvalidRequest(
            "command must not be empty".to_string(),
        ));
    }
    validate_no_nul(&request.command, "command")?;
    validate_args(&request.args)?;
    if request.size.cols == 0 || request.size.rows == 0 {
        return Err(MobileLinuxError::InvalidRequest(
            "PTY size must be non-zero".to_string(),
        ));
    }
    if let Some(cwd) = &request.cwd {
        validate_no_nul(cwd, "cwd")?;
        validate_guest_path(cwd, "cwd", true)?;
    }
    validate_env(&request.env)?;
    Ok(())
}

pub(super) fn validate_args(args: &[String]) -> Result<(), MobileLinuxError> {
    for arg in args {
        validate_no_nul(arg, "args")?;
    }
    Ok(())
}

pub(super) fn validate_env(env: &BTreeMap<String, String>) -> Result<(), MobileLinuxError> {
    for (key, value) in env {
        if key.is_empty() {
            return Err(MobileLinuxError::InvalidRequest(
                "environment keys must not be empty".to_string(),
            ));
        }
        if key.contains('=') {
            return Err(MobileLinuxError::InvalidRequest(
                "environment keys must not contain '='".to_string(),
            ));
        }
        validate_no_nul(key, "environment key")?;
        validate_no_nul(value, "environment value")?;
    }
    Ok(())
}

pub(super) fn validate_no_nul(value: &str, field: &str) -> Result<(), MobileLinuxError> {
    if value.contains('\0') {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} must not contain NUL bytes"
        )));
    }
    Ok(())
}

pub(super) fn validate_mount(
    mount: &MountSpec,
    config: &IosIshRuntimeConfig,
) -> Result<MountSpec, MobileLinuxError> {
    let host_path = normalize_host_path(&mount.host_path, "host_path")?;
    validate_guest_path(&mount.guest_path, "guest_path", false)?;
    let normalized_mount = MountSpec {
        host_path: host_path.clone(),
        guest_path: mount.guest_path.clone(),
        read_only: mount.read_only,
        purpose: mount.purpose,
    };
    // A Project path is an exact grant. In particular, do not let path
    // normalization turn a symlink or lexical alias into the granted path.
    let authorized_project = config
        .authorized_project_mounts
        .iter()
        .any(|grant| grant == &normalized_mount && mount.host_path == grant.host_path);
    let managed_root = normalize_host_path(&config.managed_root, "managed_root")?;
    let app_root = normalize_host_path(&config.app_sandbox_root, "app_sandbox_root")?;
    let guest_home = mobile_linux_api::guest_paths::home_for_profile(&config.brand_profile);
    let apps_guest = config
        .brand_profile
        .apps_guest_path()
        .to_string_lossy()
        .into_owned();
    let local_app_config_root = normalize_host_path(
        &config
            .brand_profile
            .persistent_home_host_path(&managed_root)
            .join(&config.brand_profile.config_dir_name),
        "Project config root",
    )?;
    let local_app_source = authorized_project
        && guest_path_has_prefix(&mount.guest_path, &apps_guest)
        && mount.guest_path != apps_guest.as_str();
    if ["/", "/home"].contains(&mount.guest_path.as_str())
        || (guest_path_has_prefix(&mount.guest_path, &guest_home)
            && !matches!(mount.purpose, MountPurpose::Workspace)
            && !authorized_project)
        || guest_path_has_prefix(&mount.guest_path, mobile_linux_api::guest_paths::ADMIN_HOME)
    {
        return Err(MobileLinuxError::InvalidRequest(
            "request mounts may not replace the persistent guest home".into(),
        ));
    }
    let user_workspace = mobile_linux_core::is_persistent_user_workspace(
        &managed_root,
        &config.brand_profile,
        mount,
    );
    let mut protected = if user_workspace || authorized_project {
        Vec::new()
    } else {
        vec![managed_root]
    };
    for path in &config.protected_host_roots {
        let protected_path = normalize_host_path(path, "protected root")?;
        if local_app_source && protected_path == local_app_config_root {
            continue;
        }
        protected.push(protected_path);
    }
    if host_path == app_root
        || protected
            .iter()
            .any(|path| host_path.starts_with(path) || path.starts_with(&host_path))
    {
        return Err(MobileLinuxError::InvalidRequest(
            "request mount overlaps protected host storage".into(),
        ));
    }
    if matches!(mount.purpose, MountPurpose::Workspace) {
        if host_path != normalize_host_path(&config.workspace_host_path, "workspace")?
            || mount.guest_path != config.workspace_guest_path()
        {
            return Err(MobileLinuxError::InvalidRequest(
                "workspace mount must match configured workspace".into(),
            ));
        }
    } else {
        if mount.guest_path == config.workspace_guest_path() {
            return Err(MobileLinuxError::InvalidRequest(
                "only workspace mounts may target the managed workspace".into(),
            ));
        }
        if !authorized_project {
            let allowed_host = config
                .allowed_mount_roots
                .iter()
                .map(|path| normalize_host_path(path, "allowed root"))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|path| host_path.starts_with(path));
            if !allowed_host
                || !config
                    .allowed_guest_roots
                    .iter()
                    .any(|path| guest_path_has_prefix(&mount.guest_path, path))
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "request mount is outside explicit host/guest mount policy".into(),
                ));
            }
        }
    }
    Ok(normalized_mount)
}

/// Validate the host-supplied, exact Project directory allowlist. Every tuple
/// is pinned to the selected brand profile's persistent-home mapping and must
/// point at an existing canonical directory. No ancestor, config root, or
/// `.apps` root is granted by implication.
pub(super) fn validate_authorized_project_mounts(
    config: &IosIshRuntimeConfig,
) -> Result<Vec<MountSpec>, MobileLinuxError> {
    if config.authorized_project_mounts.len() > MAX_AUTHORIZED_PROJECT_MOUNTS {
        return Err(MobileLinuxError::InvalidRequest(
            "Project mount grants exceed the directory limit".into(),
        ));
    }
    let managed_root = normalize_host_path(&config.managed_root, "managed_root")?;
    let app_root = normalize_host_path(&config.app_sandbox_root, "app_sandbox_root")?;
    let persistent_home = normalize_host_path(
        &config
            .brand_profile
            .persistent_home_host_path(&managed_root),
        "Project persistent home",
    )?;
    let guest_home = config.brand_profile.guest_home();
    let config_dir = config.brand_profile.config_dir_name.as_str();
    let mut validated: Vec<MountSpec> = Vec::with_capacity(config.authorized_project_mounts.len());

    for grant in &config.authorized_project_mounts {
        if !matches!(grant.purpose, MountPurpose::External) || grant.read_only {
            return Err(MobileLinuxError::InvalidRequest(
                "Project mount grants must be writable External mounts".into(),
            ));
        }
        validate_guest_path(&grant.guest_path, "Project guest_path", false)?;
        let guest_path = Path::new(&grant.guest_path);
        let Some(relative_guest) = guest_path.strip_prefix(&guest_home).ok() else {
            return Err(MobileLinuxError::InvalidRequest(
                "Project guest path must be below the selected account home".into(),
            ));
        };
        let guest_components = normal_components(relative_guest, "Project guest path")?;
        if guest_components.is_empty() {
            return Err(MobileLinuxError::InvalidRequest(
                "Project mount may not replace the selected account home".into(),
            ));
        }
        if guest_home.join(relative_guest).to_string_lossy().as_ref() != grant.guest_path.as_str() {
            return Err(MobileLinuxError::InvalidRequest(
                "Project guest_path must be normalized".into(),
            ));
        }

        // The config tree is otherwise protected. A Local App source may be
        // any strict, normalized descendant of `.apps`, but neither `.apps`
        // nor its parent is implicitly granted.
        let local_app_source = guest_components
            .first()
            .is_some_and(|part| part == config_dir)
            && guest_components.get(1).map(String::as_str) == Some(".apps")
            && guest_components.len() >= 3;
        if guest_components
            .first()
            .is_some_and(|part| part == config_dir)
        {
            if !local_app_source
                || guest_components[2..]
                    .iter()
                    .any(|part| !is_safe_project_component(part))
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "config Project mounts must target a normalized .apps descendant".into(),
                ));
            }
        }

        // Derive the host path from the exact same relative path. This
        // prevents profile mismatches and host/guest remapping in the grant.
        let expected_host = persistent_home.join(relative_guest);
        if grant.host_path != expected_host {
            return Err(MobileLinuxError::InvalidRequest(
                "Project host and guest paths do not match the selected profile".into(),
            ));
        }
        ensure_canonical_directory(&grant.host_path, "Project host_path")?;
        if grant.host_path == managed_root || grant.host_path == app_root {
            return Err(MobileLinuxError::InvalidRequest(
                "Project mount may not expose an application or managed root".into(),
            ));
        }
        for path in &config.protected_host_roots {
            let protected = normalize_host_path(path, "protected root")?;
            let local_app_config_root =
                normalize_host_path(&persistent_home.join(config_dir), "Project config root")?;
            if local_app_source && protected == local_app_config_root {
                continue;
            }
            if grant.host_path.starts_with(&protected) || protected.starts_with(&grant.host_path) {
                return Err(MobileLinuxError::InvalidRequest(
                    "Project mount overlaps protected host storage".into(),
                ));
            }
        }

        for existing in &validated {
            if paths_overlap(&existing.host_path, &grant.host_path)
                || paths_overlap(
                    Path::new(&existing.guest_path),
                    Path::new(&grant.guest_path),
                )
            {
                return Err(MobileLinuxError::InvalidRequest(
                    "Project mount grants may not duplicate or overlap".into(),
                ));
            }
        }
        validated.push(grant.clone());
    }

    Ok(validated)
}

fn normal_components(path: &Path, field: &str) -> Result<Vec<String>, MobileLinuxError> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned).ok_or_else(|| {
                MobileLinuxError::InvalidRequest(format!("{field} must use UTF-8 path components"))
            }),
            _ => Err(MobileLinuxError::InvalidRequest(format!(
                "{field} may contain only normalized path components"
            ))),
        })
        .collect()
}

fn is_safe_project_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component
            .chars()
            .any(|character| character.is_control() || character == '\\')
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn ensure_canonical_directory(path: &Path, field: &str) -> Result<(), MobileLinuxError> {
    let normalized = normalize_host_path(path, field)?;
    if normalized != path {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} must be the canonical host path"
        )));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        MobileLinuxError::InvalidRequest(format!("{field} must exist as a directory: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} must be a real directory without a symlink"
        )));
    }

    let canonical = fs::canonicalize(path).map_err(|error| {
        MobileLinuxError::InvalidRequest(format!("{field} cannot be canonicalized: {error}"))
    })?;
    if canonical != path {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} must not use a symlink alias"
        )));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            MobileLinuxError::InvalidRequest(format!(
                "{field} has an unreadable path component: {error}"
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "{field} may not contain symlink components"
            )));
        }
    }
    Ok(())
}

pub(super) fn guest_path_has_prefix(path: &str, prefix: &str) -> bool {
    (prefix == "/" && path.starts_with('/'))
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(super) fn validate_guest_path(
    value: &str,
    field: &str,
    allow_root: bool,
) -> Result<(), MobileLinuxError> {
    if !value.starts_with('/') {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} must be an absolute guest path"
        )));
    }
    if !allow_root && value == "/" {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} may not be the guest root"
        )));
    }
    for component in Path::new(value).components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(MobileLinuxError::InvalidRequest(format!(
                "{field} may not contain path traversal"
            )));
        }
    }
    Ok(())
}

pub(super) fn normalize_host_path(path: &Path, field: &str) -> Result<PathBuf, MobileLinuxError> {
    if !path.is_absolute() {
        return Err(MobileLinuxError::InvalidRequest(format!(
            "{field} must be absolute"
        )));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(MobileLinuxError::InvalidRequest(format!(
                    "{field} may not contain parent traversal"
                )));
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }

    let mut existing = normalized.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(MobileLinuxError::InvalidRequest(format!(
                        "{field} has no resolvable ancestor"
                    )));
                };
                missing.push(name.to_os_string());
                let Some(parent) = existing.parent() else {
                    return Err(MobileLinuxError::InvalidRequest(format!(
                        "{field} has no resolvable ancestor"
                    )));
                };
                existing = parent;
            }
            Err(error) => {
                return Err(MobileLinuxError::InvalidRequest(format!(
                    "{field} cannot be resolved safely: {error}"
                )));
            }
        }
    }
}
