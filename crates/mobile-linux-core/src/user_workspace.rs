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
    let container = guest_paths::workspace_root_for_profile(profile);
    if !["chat", "code", "projects", "scheduled"]
        .iter()
        .any(|name| {
            let prefix = format!("{container}/{name}");
            mount.guest_path == prefix
                || mount
                    .guest_path
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    {
        return false;
    }
    let Some(expected) =
        guest_paths::workspace_host_path_for_profile(managed_root, profile, &mount.guest_path)
    else {
        return false;
    };
    let Ok(managed) = managed_root.canonicalize() else {
        return false;
    };
    let Some(expected_lexical) =
        guest_paths::workspace_host_path_for_profile(&managed, profile, &mount.guest_path)
    else {
        return false;
    };
    if mount.host_path.as_os_str() != expected.as_os_str()
        && mount.host_path.as_os_str() != expected_lexical.as_os_str()
    {
        return false;
    }
    let (Ok(home), Ok(expected), Ok(actual)) = (
        profile
            .persistent_home_host_path(managed_root)
            .canonicalize(),
        expected.canonicalize(),
        mount.host_path.canonicalize(),
    ) else {
        return false;
    };
    home == profile.persistent_home_host_path(&managed)
        && expected.starts_with(&home)
        && expected == expected_lexical
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

    #[test]
    fn managed_workspace_exception_excludes_the_rest_of_brand_config() {
        let root = tempfile::tempdir().unwrap();
        let profile = BrandRuntimeProfile::new("atlas", "guest", ".atlas-config");
        let home = profile.persistent_home_host_path(root.path());
        for relative in ["chat", "code/nested", "projects/project", "scheduled/job"] {
            let host = home.join(".atlas-config").join(relative);
            std::fs::create_dir_all(&host).unwrap();
            let mount = MountSpec {
                host_path: host,
                guest_path: format!("/home/guest/.atlas-config/{relative}"),
                read_only: false,
                purpose: MountPurpose::Workspace,
            };
            assert!(
                is_persistent_user_workspace(root.path(), &profile, &mount),
                "{relative}"
            );
        }
        for relative in ["settings", "providers", ".apps/agent", "projects-other"] {
            let host = home.join(".atlas-config").join(relative);
            std::fs::create_dir_all(&host).unwrap();
            let mount = MountSpec {
                host_path: host,
                guest_path: format!("/home/guest/.atlas-config/{relative}"),
                read_only: false,
                purpose: MountPurpose::Workspace,
            };
            assert!(
                !is_persistent_user_workspace(root.path(), &profile, &mount),
                "{relative}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_workspace_symlink_into_sensitive_sibling_within_home() {
        let root = tempfile::tempdir().unwrap();
        let profile = BrandRuntimeProfile::default();
        let container = profile
            .persistent_home_host_path(root.path())
            .join(".lingxi");
        std::fs::create_dir_all(container.join("settings")).unwrap();
        std::os::unix::fs::symlink(container.join("settings"), container.join("projects")).unwrap();
        let mount = MountSpec {
            host_path: container.join("projects"),
            guest_path: "/home/lingxi/.lingxi/projects".into(),
            read_only: false,
            purpose: MountPurpose::Workspace,
        };
        assert!(!is_persistent_user_workspace(root.path(), &profile, &mount));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_host_alias_of_an_otherwise_valid_workspace() {
        let root = tempfile::tempdir().unwrap();
        let profile = BrandRuntimeProfile::default();
        let home = profile.persistent_home_host_path(root.path());
        let host = home.join(".lingxi/projects");
        std::fs::create_dir_all(&host).unwrap();
        let alias = root.path().join("projects-alias");
        std::os::unix::fs::symlink(&host, &alias).unwrap();
        let mount = MountSpec {
            host_path: host.clone(),
            guest_path: "/home/lingxi/.lingxi/projects".into(),
            read_only: false,
            purpose: MountPurpose::Workspace,
        };
        assert!(is_persistent_user_workspace(root.path(), &profile, &mount));
        assert!(!is_persistent_user_workspace(
            root.path(),
            &profile,
            &MountSpec {
                host_path: alias,
                ..mount
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_from_user_home() {
        let root = tempfile::tempdir().unwrap();
        let home = guest_paths::home_host_path(root.path());
        std::fs::create_dir_all(home.join(".lingxi")).unwrap();
        let outside = root.path().join("alpine-rootfs");
        std::fs::create_dir_all(&outside).unwrap();
        let host = home.join(".lingxi/projects");
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
        std::fs::create_dir_all(outside.join(".lingxi/projects")).unwrap();
        std::os::unix::fs::symlink(outside, &home).unwrap();
        let mount = MountSpec {
            host_path: home.join(".lingxi/projects"),
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
