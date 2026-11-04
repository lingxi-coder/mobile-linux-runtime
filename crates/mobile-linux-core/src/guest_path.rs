//! Pure guest-to-host path translation, independent of a host filesystem trait.
use crate::filesystem::ensure_real_directory;
use crate::{RootfsManifestError, RootfsStoreError};
use mobile_linux_api::{find_guest_mount, parse_guest_path_components, MountSpec};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// A guest path cannot be accessed through its host filesystem mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestPathError {
    /// The matching mount does not permit writes.
    ReadOnly { guest_path: String },
    /// The path is inside guest-owned space without a safe host mapping.
    NotHostBacked,
}

/// Resolve through the live mount snapshot while enforcing read-only mounts
/// and the caller's guest-space fence. Paths outside guest space pass through.
pub fn resolve_guest_path(
    path: &str,
    mounts: &[MountSpec],
    guest_roots: &[&str],
    write: bool,
) -> Result<Option<PathBuf>, GuestPathError> {
    if let Some((mount, host)) = find_guest_mount(path, mounts) {
        if write && mount.read_only {
            return Err(GuestPathError::ReadOnly {
                guest_path: mount.guest_path.clone(),
            });
        }
        return Ok(Some(host));
    }
    // Test the raw spelling: rejected traversal must never fall through to a
    // native filesystem as an apparently unrelated host path.
    let windows_spelling = path.contains('\\').then(|| path.replace('\\', "/"));
    let is_guest_owned = |candidate: &str| {
        guest_roots.iter().any(|root| {
            candidate == *root
                || candidate
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    };
    if is_guest_owned(path) || windows_spelling.as_deref().is_some_and(is_guest_owned) {
        return Err(GuestPathError::NotHostBacked);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mobile_linux_api::MountPurpose;
    #[test]
    fn readonly_and_unmapped_guest_space_fail_closed() {
        let mounts = vec![MountSpec {
            host_path: PathBuf::from("/host/project"),
            guest_path: "/home/lingxi/.lingxi/project".into(),
            read_only: true,
            purpose: MountPurpose::Workspace,
        }];
        let roots = ["/home/lingxi/.lingxi", "/root"];
        assert_eq!(
            resolve_guest_path("/home/lingxi/.lingxi/project/a", &mounts, &roots, false),
            Ok(Some(PathBuf::from("/host/project/a")))
        );
        assert!(matches!(
            resolve_guest_path("/home/lingxi/.lingxi/project/a", &mounts, &roots, true),
            Err(GuestPathError::ReadOnly { .. })
        ));
        assert_eq!(
            resolve_guest_path(
                "/home/lingxi/.lingxi/project/../secret",
                &mounts,
                &roots,
                false
            ),
            Err(GuestPathError::NotHostBacked)
        );
        assert_eq!(
            resolve_guest_path("/root/unmounted", &mounts, &roots, false),
            Err(GuestPathError::NotHostBacked)
        );
        assert_eq!(
            resolve_guest_path(r"/home/lingxi/.lingxi\..\secret", &mounts, &roots, false),
            Err(GuestPathError::NotHostBacked)
        );
        assert_eq!(
            resolve_guest_path(r"/home\lingxi\.lingxi\..\secret", &mounts, &roots, false),
            Err(GuestPathError::NotHostBacked)
        );
        assert_eq!(
            resolve_guest_path("/host/ordinary", &mounts, &roots, false),
            Ok(None)
        );
    }

    #[cfg(windows)]
    #[test]
    fn guest_component_cannot_escape_a_windows_root_or_open_a_stream() {
        let root = Path::new(r"C:\managed\rootfs");
        assert!(secure_guest_host_path(root, "/usr/C:/escape").is_err());
        assert!(secure_guest_host_path(root, "/usr/bin/name:stream").is_err());
    }
}

pub fn validate_guest_path(path: &str) -> Result<(), RootfsManifestError> {
    parse_guest_path_components(path)
        .map(|_| ())
        .ok_or_else(|| RootfsManifestError::InvalidGuestPath(path.to_string()))
}

pub fn path_is_within_guest_path(path: &str, candidate_parent: &str) -> bool {
    let Ok(path) = normalized_guest_path(path) else {
        return false;
    };
    let Ok(parent) = normalized_guest_path(candidate_parent) else {
        return false;
    };
    path == parent || path.starts_with(&(parent.clone() + "/"))
}

pub fn normalized_guest_path(path: &str) -> Result<String, RootfsManifestError> {
    validate_guest_path(path)?;
    // Validation already requires the canonical Linux guest spelling.
    Ok(path.to_string())
}

fn secure_guest_host_path(active_root: &Path, guest_path: &str) -> Result<PathBuf, PathBuf> {
    let components =
        parse_guest_path_components(guest_path).ok_or_else(|| active_root.to_path_buf())?;
    let mut host_path = active_root.to_path_buf();
    for component in components {
        let mut parts = Path::new(component).components();
        if !matches!(parts.next(), Some(Component::Normal(_)))
            || parts.next().is_some()
            || (cfg!(windows) && component.contains(':'))
        {
            return Err(active_root.to_path_buf());
        }
        host_path.push(component);
    }
    Ok(host_path)
}

pub(crate) fn secure_guest_host_path_with_real_parents(
    active_root: &Path,
    guest_path: &str,
) -> Result<PathBuf, RootfsStoreError> {
    let host_path = secure_guest_host_path(active_root, guest_path)
        .map_err(RootfsStoreError::UnsafeManagedPath)?;
    ensure_real_directory(active_root)?;

    let relative = host_path
        .strip_prefix(active_root)
        .map_err(|_| RootfsStoreError::UnsafeManagedPath(host_path.clone()))?;
    let mut parent = active_root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(RootfsStoreError::UnsafeManagedPath(host_path));
        };
        if components.peek().is_none() {
            break;
        }
        parent.push(segment);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(RootfsStoreError::UnsafeManagedPath(parent));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(RootfsStoreError::Io {
                    path: parent,
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(host_path)
}
