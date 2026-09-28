use crate::filesystem::path_present;
use crate::manifest::{ALLOWED_WRITABLE_GUEST_PATHS, REQUIRED_ROOTFS_PACKAGES};
use mobile_linux_api::{MobileLinuxRuntimeMode, RootfsState, SandboxBackend};
use std::{fs, path::Path};

use super::*;

fn manifest() -> RootfsManifest {
    RootfsManifest {
        schema_version: 2,
        runtime: "android-proot".to_string(),
        platform: "android".to_string(),
        abi: "arm64".to_string(),
        rootfs_version: "1.0.0".to_string(),
        content_sha256: None,
        sbom_filename: None,
        source_pins_filename: None,
        archive: RootfsArchive {
            filename: "rootfs.tar.zst".to_string(),
            sha256: "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881".to_string(),
            size_bytes: 1,
        },
        packages: REQUIRED_ROOTFS_PACKAGES
            .iter()
            .map(|name| RootfsPackage {
                name: (*name).to_string(),
                version: "1.0-r0".to_string(),
                license: "MIT".to_string(),
                architecture: "arm64".to_string(),
                origin: (*name).to_string(),
            })
            .collect(),
        executable_allowlist: vec![RootfsManifestEntry {
            path: "/usr/bin/python3".to_string(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            kind: RootfsEntryKind::Interpreter,
            size_bytes: Some(0),
        }],
        immutable_files: vec![RootfsImmutableEntry {
            path: "/usr/bin/python3".to_string(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            kind: RootfsImmutableKind::RegularFile,
            size_bytes: 0,
        }],
        writable_paths: ALLOWED_WRITABLE_GUEST_PATHS
            .iter()
            .map(|path| (*path).to_string())
            .collect(),
    }
}

fn write_valid_root(path: &Path) {
    let executable = path.join("usr/bin/python3");
    fs::create_dir_all(executable.parent().expect("parent")).expect("rootfs dirs");
    fs::write(executable, b"").expect("rootfs executable");
    for writable in ALLOWED_WRITABLE_GUEST_PATHS {
        fs::create_dir_all(path.join(writable.trim_start_matches('/')))
            .expect("writable rootfs dir");
    }
}

#[test]
fn manifest_rejects_duplicate_paths() {
    let mut manifest = manifest();
    manifest.executable_allowlist.push(RootfsManifestEntry {
        path: "/usr/bin/python3".to_string(),
        sha256: manifest.executable_allowlist[0].sha256.clone(),
        kind: RootfsEntryKind::SharedLibrary,
        size_bytes: Some(0),
    });
    let err = manifest.validate().expect_err("duplicate path");
    assert!(matches!(err, RootfsManifestError::DuplicatePath(_)));
}

#[test]
fn manifest_rejects_unknown_schema_versions() {
    let mut manifest = manifest();
    manifest.schema_version = 3;
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::InvalidSchemaVersion)
    ));

    manifest.schema_version = 1;
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::InvalidSchemaVersion)
    ));
}

#[test]
fn manifest_requires_the_fixed_toolset_packages() {
    let mut manifest = manifest();
    manifest.packages.retain(|package| package.name != "git");
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::MissingRequiredPackage(package)) if package == "git"
    ));
}

#[test]
fn manifest_deserialization_rejects_unknown_fields() {
    let mut value = serde_json::to_value(manifest()).expect("serialize manifest");
    value
        .as_object_mut()
        .expect("manifest object")
        .insert("future_policy".to_string(), serde_json::json!(true));
    assert!(serde_json::from_value::<RootfsManifest>(value).is_err());
}

#[test]
fn manifest_rejects_unapproved_writable_paths() {
    let mut manifest = manifest();
    manifest.writable_paths = vec!["/usr/local".to_string()];
    let err = manifest
        .validate()
        .expect_err("writable path must be rejected");
    assert!(matches!(
        err,
        RootfsManifestError::WritablePathNotAllowed(path) if path == "/usr/local"
    ));
}

#[test]
fn manifest_requires_the_complete_fixed_writable_path_set() {
    let mut manifest = manifest();
    manifest.writable_paths.retain(|path| path != "/workspace");
    let err = manifest
        .validate()
        .expect_err("missing fixed writable path must be rejected");
    assert!(matches!(
        err,
        RootfsManifestError::MissingWritablePath(path) if path == "/workspace"
    ));
}

#[test]
fn manifest_rejects_non_lowercase_hashes() {
    let mut manifest = manifest();
    manifest.archive.sha256 =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string();
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::InvalidSha256(_))
    ));
}

#[test]
fn guest_containment_uses_linux_separators_on_every_host() {
    assert_eq!(
        normalized_guest_path("/workspace/bin/python3").unwrap(),
        "/workspace/bin/python3"
    );
    assert!(path_is_within_guest_path(
        "/workspace/bin/python3",
        "/workspace"
    ));
    assert!(!path_is_within_guest_path(
        "/workspace-other/bin/python3",
        "/workspace"
    ));
    assert!(!path_is_within_guest_path(
        "/workspace/../usr/bin/python3",
        "/workspace"
    ));
}

#[test]
fn manifest_rejects_executables_in_writable_paths() {
    let mut manifest = manifest();
    manifest.executable_allowlist[0].path = "/workspace/bin/python3".to_string();
    let err = manifest
        .validate()
        .expect_err("writable executable must be rejected");
    assert!(matches!(
        err,
        RootfsManifestError::ExecutableInWritablePath(path)
            if path == "/workspace/bin/python3"
    ));
}

#[test]
fn manifest_rejects_path_like_versions_and_archive_names() {
    let mut invalid_version = manifest();
    invalid_version.rootfs_version = "../outside".to_string();
    assert!(matches!(
        invalid_version.validate(),
        Err(RootfsManifestError::UnsafeArtifactName)
    ));

    let mut invalid_archive = manifest();
    invalid_archive.archive.filename = "nested/rootfs.tar.zst".to_string();
    assert!(matches!(
        invalid_archive.validate(),
        Err(RootfsManifestError::UnsafeArtifactName)
    ));
}

#[test]
fn verify_archive_hash_and_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("rootfs.tar.zst");
    fs::write(&archive, b"x").expect("write archive");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    store
        .verify_archive(&archive, &manifest())
        .expect("archive ok");
}

#[test]
fn store_rejects_manifest_for_a_different_runtime_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("rootfs.tar.zst");
    fs::write(&archive, b"x").expect("write archive");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::IosIsh,
        MobileLinuxRuntimeMode::MobileLinux,
        "ios",
        "arm64",
    );

    let err = store
        .verify_archive(&archive, &manifest())
        .expect_err("Android manifest must not be accepted by the iOS store");
    assert!(err.to_string().contains("does not match"));
}

#[cfg(unix)]
#[test]
fn verify_archive_rejects_symlinked_assets() {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_archive = temp.path().join("real-rootfs.tar.zst");
    let archive_link = temp.path().join("rootfs.tar.zst");
    fs::write(&real_archive, b"x").expect("write archive");
    unix_fs::symlink(&real_archive, &archive_link).expect("archive symlink");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );

    assert!(matches!(
        store.verify_archive(&archive_link, &manifest()),
        Err(RootfsStoreError::UnsafeManagedPath(path)) if path == archive_link
    ));
}

#[test]
fn verify_active_root_reports_missing_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    let report = store
        .verify_active_root(&manifest())
        .expect("verify report");
    assert!(!report.ok);
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn status_is_ready_only_after_integrity_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    fs::create_dir_all(store.active_root()).expect("active root");
    let corrupt = store.status(&manifest());
    assert_eq!(corrupt.state, RootfsState::Corrupt);
    assert!(
        corrupt.last_error.is_some(),
        "corrupt status must surface the integrity failure"
    );

    let executable = store.active_root().join("usr/bin/python3");
    write_valid_root(&store.active_root());
    assert_eq!(store.status(&manifest()).state, RootfsState::Ready);
    assert_eq!(
        store
            .authorize_manifest_file("/usr/bin/python3", &manifest())
            .expect("allowlisted executable"),
        executable
    );
}

#[test]
fn verify_active_root_detects_non_executable_tampering_and_unlisted_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    write_valid_root(&store.active_root());
    let stdlib = store.active_root().join("usr/lib/python3.12/site.py");
    fs::create_dir_all(stdlib.parent().expect("stdlib parent")).expect("stdlib dirs");
    fs::write(&stdlib, b"trusted").expect("stdlib fixture");

    let mut expected = manifest();
    expected.immutable_files.push(RootfsImmutableEntry {
        path: "/usr/lib/python3.12/site.py".to_string(),
        sha256: "a9a089195c68d2adeee23beaa2c3a93b1d4cdf09046e7a9e520b3b166dff3e6a".to_string(),
        kind: RootfsImmutableKind::RegularFile,
        size_bytes: 7,
    });
    assert!(
        store
            .verify_active_root(&expected)
            .expect("trusted root")
            .ok
    );

    fs::write(&stdlib, b"hostile").expect("tamper stdlib");
    let tampered = store
        .verify_active_root(&expected)
        .expect("tampered report");
    assert!(!tampered.ok);
    assert!(tampered
        .issues
        .iter()
        .any(|issue| issue.path == "/usr/lib/python3.12/site.py"
            && issue.reason.contains("hash mismatch")));

    fs::write(&stdlib, b"trusted").expect("restore stdlib");
    fs::write(
        store.active_root().join("usr/lib/python3.12/injected.py"),
        b"payload",
    )
    .expect("unlisted stdlib file");
    let unlisted = store
        .verify_active_root(&expected)
        .expect("unlisted report");
    assert!(!unlisted.ok);
    assert!(unlisted
        .issues
        .iter()
        .any(|issue| issue.path == "/usr/lib/python3.12/injected.py"
            && issue.reason.contains("absent from immutable inventory")));
}

#[test]
fn staged_activation_and_interruption_recovery_are_bounded() {
    let temp = tempfile::tempdir().expect("tempdir");
    let managed = temp.path().join("managed");
    let store = RootfsStore::new(
        managed.clone(),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    let prepared = temp.path().join("prepared");
    write_valid_root(&prepared);
    store
        .stage_prepared_rootfs(&prepared, &manifest())
        .expect("stage");
    let status = store.activate_staged_rootfs(&manifest()).expect("activate");
    assert_eq!(status.state, RootfsState::Ready);

    fs::rename(store.active_root(), managed.join("active.rollback"))
        .expect("simulate interrupted swap");
    let recovered = store
        .recover_interrupted_activation(&manifest())
        .expect("recover");
    assert_eq!(recovered.state, RootfsState::Ready);
    assert!(store.active_root().is_dir());
    assert!(!managed.join("active.rollback").exists());
}

#[cfg(unix)]
#[test]
fn staging_rejects_a_symlinked_staged_directory() {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let managed = temp.path().join("managed");
    let outside = temp.path().join("outside");
    let prepared = temp.path().join("prepared");
    fs::create_dir_all(&managed).expect("managed");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("keep.txt"), b"keep").expect("outside data");
    unix_fs::symlink(&outside, managed.join("staged")).expect("staged symlink");
    write_valid_root(&prepared);
    let store = RootfsStore::new(
        managed,
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );

    assert!(matches!(
        store.stage_prepared_rootfs(&prepared, &manifest()),
        Err(RootfsStoreError::UnsafeManagedPath(_))
    ));
    assert_eq!(
        fs::read(outside.join("keep.txt")).expect("outside preserved"),
        b"keep"
    );
}

#[test]
fn recovery_never_promotes_an_invalid_rollback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let managed = temp.path().join("managed");
    fs::create_dir_all(managed.join("active.rollback")).expect("invalid rollback");
    let store = RootfsStore::new(
        managed,
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );

    let status = store
        .recover_interrupted_activation(&manifest())
        .expect("recovery status");
    assert_eq!(status.state, RootfsState::Missing);
    assert!(!path_present(&store.active_root()));
}

#[test]
fn reset_clears_only_scratch_and_preserves_projects_and_keys() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    write_valid_root(&store.active_root());
    fs::create_dir_all(store.active_root().join("tmp")).expect("tmp");
    fs::create_dir_all(store.active_root().join("root/.ssh")).expect("ssh");
    fs::create_dir_all(store.active_root().join("workspace/project")).expect("workspace");
    fs::write(store.active_root().join("tmp/delete.txt"), b"temp").expect("temp data");
    fs::write(store.active_root().join("root/.ssh/id_ed25519"), b"key").expect("key");
    fs::write(
        store.active_root().join("workspace/project/keep.txt"),
        b"project",
    )
    .expect("project data");

    store
        .reset_writable_state(&manifest())
        .expect("scratch reset");
    assert!(!store.active_root().join("tmp/delete.txt").exists());
    assert!(store.active_root().join("root/.ssh/id_ed25519").exists());
    assert!(store
        .active_root()
        .join("workspace/project/keep.txt")
        .exists());
}

#[cfg(unix)]
#[test]
fn verify_active_root_rejects_symlinked_executables() {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    let active = store.active_root().join("usr/bin");
    fs::create_dir_all(&active).expect("active tree");
    let outside = temp.path().join("outside-python3");
    fs::write(&outside, b"payload").expect("outside file");
    unix_fs::symlink(&outside, active.join("python3")).expect("symlink");

    let mut manifest = manifest();
    manifest.executable_allowlist[0].size_bytes = Some(7);
    manifest.executable_allowlist[0].sha256 =
        "239f59ed55e737c77147cf55ad0c1b03b8c4fa0193f6c82b53f6ba356b4a8044".to_string();
    manifest.immutable_files[0].size_bytes = 7;
    manifest.immutable_files[0].sha256 =
        "239f59ed55e737c77147cf55ad0c1b03b8c4fa0193f6c82b53f6ba356b4a8044".to_string();
    let report = store.verify_active_root(&manifest).expect("report");
    assert!(!report.ok);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.reason.contains("symlink")));
}

#[cfg(unix)]
#[test]
fn verify_and_authorize_reject_symlinked_parent_directories() {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    let active_usr = store.active_root().join("usr");
    let outside_bin = temp.path().join("outside-bin");
    fs::create_dir_all(&active_usr).expect("active tree");
    fs::create_dir_all(&outside_bin).expect("outside bin");
    fs::write(outside_bin.join("python3"), b"").expect("outside executable");
    unix_fs::symlink(&outside_bin, active_usr.join("bin")).expect("parent symlink");

    let report = store.verify_active_root(&manifest()).expect("report");
    assert!(!report.ok);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.reason.contains("symlink")));
    assert!(matches!(
        store.authorize_manifest_file("/usr/bin/python3", &manifest()),
        Err(RootfsStoreError::ExecutionDenied { .. })
    ));
}

#[cfg(unix)]
#[test]
fn reset_rejects_symlinked_active_root() {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let managed = temp.path().join("managed");
    let outside = temp.path().join("outside");
    fs::create_dir_all(outside.join("root")).expect("outside");
    fs::write(outside.join("root/keep.txt"), b"keep").expect("outside data");
    fs::create_dir_all(&managed).expect("managed");
    unix_fs::symlink(&outside, managed.join("active")).expect("active symlink");
    let store = RootfsStore::new(
        managed,
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );

    assert!(matches!(
        store.reset_writable_state(&manifest()),
        Err(RootfsStoreError::UnsafeResetPath(_))
    ));
    assert_eq!(
        fs::read(outside.join("root/keep.txt")).expect("outside preserved"),
        b"keep"
    );
}

#[cfg(unix)]
#[test]
fn reset_rejects_symlinked_parent_directories() {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let store = RootfsStore::new(
        temp.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    let outside_var = temp.path().join("outside-var");
    fs::create_dir_all(store.active_root()).expect("active root");
    fs::create_dir_all(outside_var.join("tmp")).expect("outside tmp");
    fs::write(outside_var.join("tmp/keep.txt"), b"keep").expect("outside data");
    unix_fs::symlink(&outside_var, store.active_root().join("var")).expect("parent symlink");

    assert!(matches!(
        store.reset_writable_state(&manifest()),
        Err(RootfsStoreError::UnsafeResetPath(_)) | Err(RootfsStoreError::UnsafeManagedPath(_))
    ));
    assert_eq!(
        fs::read(outside_var.join("tmp/keep.txt")).expect("outside preserved"),
        b"keep"
    );
}
