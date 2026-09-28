//! Pure guest-to-host path translation, independent of a host filesystem trait.
use crate::filesystem::ensure_real_directory;
use crate::{RootfsManifestError, RootfsStoreError};
use mobile_linux_api::{find_guest_mount, MountSpec};
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
    if guest_roots.iter().any(|root| {
        path == *root
            || path
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
    }) {
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
            guest_path: "/workspace/project".into(),
            read_only: true,
            purpose: MountPurpose::Workspace,
        }];
        let roots = ["/workspace", "/root"];
        assert_eq!(
            resolve_guest_path("/workspace/project/a", &mounts, &roots, false),
            Ok(Some(PathBuf::from("/host/project/a")))
        );
        assert!(matches!(
            resolve_guest_path("/workspace/project/a", &mounts, &roots, true),
            Err(GuestPathError::ReadOnly { .. })
        ));
        assert_eq!(
            resolve_guest_path("/workspace/project/../secret", &mounts, &roots, false),
            Err(GuestPathError::NotHostBacked)
        );
        assert_eq!(
            resolve_guest_path("/root/unmounted", &mounts, &roots, false),
            Err(GuestPathError::NotHostBacked)
        );
        assert_eq!(
            resolve_guest_path("/host/ordinary", &mounts, &roots, false),
            Ok(None)
        );
    }
}

pub fn validate_guest_path(path: &str) -> Result<(), RootfsManifestError> {
    if !path.starts_with('/')
        || path == "/"
        || path.ends_with('/')
        || path.contains("//")
        || path.as_bytes().contains(&0)
    {
        return Err(RootfsManifestError::InvalidGuestPath(path.to_string()));
    }
    let parsed = Path::new(path);
    if parsed
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(RootfsManifestError::InvalidGuestPath(path.to_string()));
    }
    Ok(())
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
    // Manifest paths name the Linux guest, never the host filesystem. Native
    // PathBuf::push inserts Windows separators and breaks containment checks.
    let mut normalized = String::new();
    for component in Path::new(path).components() {
        match component {
            Component::RootDir => {}
            Component::Normal(segment) => {
                normalized.push('/');
                normalized.push_str(&segment.to_string_lossy());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(RootfsManifestError::InvalidGuestPath(path.to_string()));
            }
        }
    }
    Ok(normalized)
}

fn secure_guest_host_path(active_root: &Path, guest_path: &str) -> Result<PathBuf, PathBuf> {
    let normalized = normalized_guest_path(guest_path).map_err(|_| active_root.to_path_buf())?;
    let mut host_path = active_root.to_path_buf();
    for component in Path::new(&normalized).components() {
        if let Component::Normal(segment) = component {
            host_path.push(segment);
        }
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
