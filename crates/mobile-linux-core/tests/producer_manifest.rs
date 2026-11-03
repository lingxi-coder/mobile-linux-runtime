//! Producer metadata regression using a historical inventory as test input.
use mobile_linux_api::{MobileLinuxRuntimeMode, SandboxBackend};
use mobile_linux_core::{RootfsManifest, RootfsManifestError, RootfsStore};

const HISTORICAL_MANIFEST: &str =
    include_str!("../../../docs/mobile-linux/releases/3.24.2-user1/arm64-v8a/rootfs-manifest.json");

fn current_fixture() -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_str(HISTORICAL_MANIFEST).unwrap();
    value["brand_profile"] =
        serde_json::to_value(mobile_linux_api::BrandRuntimeProfile::default()).unwrap();
    // This immutable historical release used the former workspace root. Only
    // adapt the test fixture's brand and writable-path contract; it is not a new asset.
    let paths = value["writable_paths"].as_array_mut().unwrap();
    let old = paths
        .iter_mut()
        .find(|path| path.as_str() == Some("/home/lingxi/lingxi"))
        .expect("historical workspace root");
    *old = mobile_linux_api::guest_paths::WORKSPACE_ROOT.into();
    value
}

fn fixture_manifest() -> RootfsManifest {
    serde_json::from_value(current_fixture()).expect("deserialize producer metadata fixture")
}

#[test]
fn historical_workspace_root_is_rejected() {
    let mut value = current_fixture();
    value["writable_paths"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|path| path.as_str() == Some(mobile_linux_api::guest_paths::WORKSPACE_ROOT))
        .map(|path| *path = "/home/lingxi/lingxi".into())
        .unwrap();
    let historical: RootfsManifest = serde_json::from_value(value).unwrap();
    assert!(matches!(
        historical.validate(),
        Err(RootfsManifestError::WritablePathNotAllowed(path))
            if path == "/home/lingxi/lingxi"
    ));
}

#[test]
fn brand_profile_is_required_and_does_not_accept_unknown_fields() {
    let mut value = current_fixture();
    value.as_object_mut().unwrap().remove("brand_profile");
    assert!(serde_json::from_value::<RootfsManifest>(value).is_err());
    let mut value = current_fixture();
    value["brand_profile"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<RootfsManifest>(value).is_err());
    let historical: Result<RootfsManifest, _> = serde_json::from_str(HISTORICAL_MANIFEST);
    assert!(historical.is_err());
}

#[test]
fn producer_metadata_fixture_validates_and_retains_every_field() {
    let manifest = fixture_manifest();
    manifest
        .validate()
        .expect("candidate inventory and metadata");
    assert_eq!(manifest.immutable_files.len(), 6095);
    assert_eq!(
        manifest.calculated_content_sha256().unwrap(),
        "6735a1ed9d575f743f4fdbc542677721e52124c044096e1fb9be1720b9a485e9"
    );
    assert_eq!(serde_json::to_value(&manifest).unwrap(), current_fixture());
}

#[test]
fn legacy_manifest_without_producer_metadata_remains_compatible() {
    let mut value = current_fixture();
    for field in ["content_sha256", "sbom_filename", "source_pins_filename"] {
        value.as_object_mut().unwrap().remove(field);
    }
    let manifest: RootfsManifest = serde_json::from_value(value.clone()).unwrap();
    manifest.validate().unwrap();
    assert_eq!(serde_json::to_value(manifest).unwrap(), value);
}

#[test]
fn producer_metadata_cannot_be_partial_null_unknown_or_redirected() {
    for field in ["content_sha256", "sbom_filename", "source_pins_filename"] {
        let mut value = current_fixture();
        value[field] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<RootfsManifest>(value).is_err(),
            "null {field}"
        );
        let mut value = current_fixture();
        value.as_object_mut().unwrap().remove(field);
        let manifest: RootfsManifest = serde_json::from_value(value).unwrap();
        assert!(matches!(
            manifest.validate(),
            Err(RootfsManifestError::IncompleteProducerMetadata)
        ));
    }
    for bad in [
        "../outside.json",
        "/tmp/outside.json",
        "..\\outside.json",
        "C:\\outside.json",
        "bad\0name",
        "other.json",
    ] {
        for field in ["sbom_filename", "source_pins_filename"] {
            let mut value = current_fixture();
            value[field] = bad.into();
            let manifest: RootfsManifest = serde_json::from_value(value).unwrap();
            assert!(
                matches!(
                    manifest.validate(),
                    Err(RootfsManifestError::InvalidEvidenceFilename { .. })
                ),
                "{field}={bad:?}"
            );
        }
    }
    let mut value = current_fixture();
    value["unexpected_metadata"] = true.into();
    assert!(serde_json::from_value::<RootfsManifest>(value).is_err());
}

#[test]
fn split_ssh_client_requires_both_real_alpine_components() {
    for missing in ["openssh-client-default", "openssh-client-common"] {
        let mut manifest = fixture_manifest();
        manifest.packages.retain(|package| package.name != missing);
        assert!(
            matches!(manifest.validate(), Err(RootfsManifestError::MissingRequiredPackage(name)) if name == "openssh-client")
        );
    }
}

#[test]
fn digest_attests_inventory_bytes_and_producer_order() {
    let mut manifest = fixture_manifest();
    manifest.content_sha256 = Some("f".repeat(64));
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::ContentDigestMismatch { .. })
    ));
    manifest.content_sha256 = Some("A".repeat(64));
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::InvalidSha256(_))
    ));
    let mut manifest = fixture_manifest();
    manifest.immutable_files.swap(0, 1);
    manifest.content_sha256 = Some(manifest.calculated_content_sha256().unwrap());
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::NonCanonicalImmutableInventory)
    ));
}

#[test]
#[ignore = "requires MOBILE_LINUX_TEST_ARCHIVE and MOBILE_LINUX_TEST_MANIFEST for an actual producer release"]
fn candidate_archive_stages_verifies_and_activates_real_tree() {
    let archive = std::path::PathBuf::from(
        std::env::var_os("MOBILE_LINUX_TEST_ARCHIVE")
            .expect("set MOBILE_LINUX_TEST_ARCHIVE to the candidate archive"),
    );
    let temporary = tempfile::tempdir().unwrap();
    let prepared = temporary.path().join("prepared");
    std::fs::create_dir(&prepared).unwrap();
    let manifest_path = std::env::var_os("MOBILE_LINUX_TEST_MANIFEST")
        .expect("set MOBILE_LINUX_TEST_MANIFEST to the matching producer manifest");
    let manifest: RootfsManifest =
        serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    manifest.validate().unwrap();
    let store = RootfsStore::new(
        temporary.path().join("managed"),
        SandboxBackend::AndroidProot,
        MobileLinuxRuntimeMode::MobileLinux,
        "android",
        "arm64-v8a",
    );
    // Pin the bytes before extraction; this test never unpacks arbitrary input.
    store.verify_archive(&archive, &manifest).unwrap();
    // macOS bsdtar normalizes Unicode archive names and symlink targets, which
    // changes attested Linux bytes. Python preserves the original UTF-8 here.
    let extracted = std::process::Command::new("python3")
        .args(["-c", "import sys, tarfile; tarfile.open(sys.argv[1], 'r:gz').extractall(sys.argv[2], filter='data')"])
        .arg(&archive)
        .arg(&prepared)
        .status()
        .unwrap();
    assert!(extracted.success());
    store.stage_prepared_rootfs(&prepared, &manifest).unwrap();
    let status = store.activate_staged_rootfs(&manifest).unwrap();
    assert_eq!(status.state, mobile_linux_api::RootfsState::Ready);
    let report = store.verify_active_root(&manifest).unwrap();
    assert!(report.ok, "{:?}", report.issues);
    assert_eq!(report.verified_files, manifest.immutable_files.len());
    // Activation must not erase attestation when the runtime persists a manifest.
    let roundtrip: RootfsManifest =
        serde_json::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    roundtrip.validate().unwrap();
    assert_eq!(roundtrip.content_sha256, manifest.content_sha256);
}
