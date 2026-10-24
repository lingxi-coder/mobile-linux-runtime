use mobile_linux_api::{guest_paths, BrandRuntimeProfile, MountPurpose, MountSpec};
use std::path::Path;

/// Whether a workspace is the exact host twin of a directory in the runtime
/// user's home. Canonical containment prevents symlinks from extending this
/// exception into rootfs, administrator, or host-owned storage.
#[must_use]
pub fn is_persistent_user_workspace(
    managed_root: &Path,
    profile: &BrandRuntimeProfile,
    mount: &MountSpec,
) -> bool {
    if !matches!(mount.purpose, MountPurpose::Workspace) {
        return false;
    }
    let Some(expected) =
        guest_paths::workspace_host_path_for_profile(managed_root, profile, &mount.guest_path)
    else {
        return false;
    };
    let (Ok(home), Ok(expected), Ok(actual)) = (
        profile
            .persistent_home_host_path(managed_root)
            .canonicalize(),
        expected.canonicalize(),
        mount.host_path.canonicalize(),
    ) else {
        return false;
    };
    let Ok(managed) = managed_root.canonicalize() else {
        return false;
    };
    home == profile.persistent_home_host_path(&managed)
        && expected.starts_with(home)
        && actual == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_corresponding_user_workspaces() {
        let root = tempfile::tempdir().unwrap();
        let guest = guest_paths::workspace("projects");
        let host = guest_paths::workspace_host_path(root.path(), &guest).unwrap();
        std::fs::create_dir_all(&host).unwrap();
        let mut mount = MountSpec {
            host_path: host,
            guest_path: guest,
            read_only: false,
            purpose: MountPurpose::Workspace,
        };
        let profile = BrandRuntimeProfile::default();
        assert!(is_persistent_user_workspace(root.path(), &profile, &mount));
        mount.guest_path = guest_paths::workspace("other");
        assert!(!is_persistent_user_workspace(root.path(), &profile, &mount));
        mount.guest_path = format!("{}/projects/../../.lingxi", guest_paths::WORKSPACE_ROOT);
        assert!(!is_persistent_user_workspace(root.path(), &profile, &mount));
        mount.guest_path = guest_paths::HOME.to_string();
        assert!(!is_persistent_user_workspace(root.path(), &profile, &mount));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_from_user_home() {
        let root = tempfile::tempdir().unwrap();
        let home = guest_paths::home_host_path(root.path());
        std::fs::create_dir_all(home.join("lingxi")).unwrap();
        let outside = root.path().join("alpine-rootfs");
        std::fs::create_dir_all(&outside).unwrap();
        let host = home.join("lingxi/projects");
        std::os::unix::fs::symlink(outside, &host).unwrap();
        let mount = MountSpec {
            host_path: host,
            guest_path: guest_paths::workspace("projects"),
            read_only: false,
            purpose: MountPurpose::Workspace,
        };
        assert!(!is_persistent_user_workspace(
            root.path(),
            &BrandRuntimeProfile::default(),
            &mount
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_replacing_the_user_home_with_a_protected_tree() {
        let root = tempfile::tempdir().unwrap();
        let home = guest_paths::home_host_path(root.path());
        std::fs::create_dir_all(home.parent().unwrap()).unwrap();
        let outside = root.path().join("alpine-rootfs");
        std::fs::create_dir_all(outside.join("lingxi/projects")).unwrap();
        std::os::unix::fs::symlink(outside, &home).unwrap();
        let mount = MountSpec {
            host_path: home.join("lingxi/projects"),
            guest_path: guest_paths::workspace("projects"),
            read_only: false,
            purpose: MountPurpose::Workspace,
        };
        assert!(!is_persistent_user_workspace(
            root.path(),
            &BrandRuntimeProfile::default(),
            &mount
        ));
    }
}
