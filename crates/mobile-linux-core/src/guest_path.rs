//! Pure guest-to-host path translation, independent of a host filesystem trait.
use mobile_linux_api::{find_guest_mount, MountSpec};
use std::path::PathBuf;

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
