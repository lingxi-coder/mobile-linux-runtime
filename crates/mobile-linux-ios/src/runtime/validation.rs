use mobile_linux_api::{
    LinuxCommandRequest, MobileLinuxError, MountPurpose, MountSpec, PtyOpenRequest,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::IosIshRuntimeConfig;

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
    let managed_root = normalize_host_path(&config.managed_root, "managed_root")?;
    let app_root = normalize_host_path(&config.app_sandbox_root, "app_sandbox_root")?;
    if guest_path_has_prefix(
        &mount.guest_path,
        mobile_linux_api::mobile_linux::guest_paths::HOME,
    ) {
        return Err(MobileLinuxError::InvalidRequest(
            "request mounts may not replace the persistent guest home".into(),
        ));
    }
    let mut protected = vec![managed_root];
    for path in &config.protected_host_roots {
        protected.push(normalize_host_path(path, "protected root")?);
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
    Ok(MountSpec {
        host_path,
        guest_path: mount.guest_path.clone(),
        read_only: mount.read_only,
        purpose: mount.purpose,
    })
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
                )))
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
                )))
            }
        }
    }
}
