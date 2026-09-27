//! Regression against the actual published producer output, including all metadata.
use mobile_linux_api::{MobileLinuxRuntimeMode, SandboxBackend};
use mobile_linux_core::{RootfsManifest, RootfsManifestError, RootfsStore};

const PUBLISHED_MANIFEST: &str =
    include_str!("../../../docs/mobile-linux/releases/3.24.2/arm64-v8a/rootfs-manifest.json");

fn published() -> RootfsManifest {
    serde_json::from_str(PUBLISHED_MANIFEST).expect("strictly deserialize actual producer manifest")
}

#[test]
fn published_producer_manifest_validates_and_retains_every_field() {
    let manifest = published();
    manifest
        .validate()
        .expect("published inventory and metadata");
    assert_eq!(manifest.immutable_files.len(), 6093);
    assert_eq!(
        manifest.calculated_content_sha256().unwrap(),
        "81c1da1352f650418a6277d9b5fd13e74266e820d39ef636b3f4bbfdb00b77c2"
    );
    assert_eq!(
        serde_json::to_value(&manifest).unwrap(),
        serde_json::from_str::<serde_json::Value>(PUBLISHED_MANIFEST).unwrap()
    );
}

#[test]
fn legacy_manifest_without_producer_metadata_remains_compatible() {
    let mut value: serde_json::Value = serde_json::from_str(PUBLISHED_MANIFEST).unwrap();
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
        let mut value: serde_json::Value = serde_json::from_str(PUBLISHED_MANIFEST).unwrap();
        value[field] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<RootfsManifest>(value).is_err(),
            "null {field}"
        );
        let mut value: serde_json::Value = serde_json::from_str(PUBLISHED_MANIFEST).unwrap();
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
            let mut value: serde_json::Value = serde_json::from_str(PUBLISHED_MANIFEST).unwrap();
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
    let mut value: serde_json::Value = serde_json::from_str(PUBLISHED_MANIFEST).unwrap();
    value["unexpected_metadata"] = true.into();
    assert!(serde_json::from_value::<RootfsManifest>(value).is_err());
}

#[test]
fn split_ssh_client_requires_both_real_alpine_components() {
    for missing in ["openssh-client-default", "openssh-client-common"] {
        let mut manifest = published();
        manifest.packages.retain(|package| package.name != missing);
        assert!(
            matches!(manifest.validate(), Err(RootfsManifestError::MissingRequiredPackage(name)) if name == "openssh-client")
        );
    }
}

#[test]
fn digest_attests_inventory_bytes_and_producer_order() {
    let mut manifest = published();
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
    let mut manifest = published();
    manifest.immutable_files.swap(0, 1);
    manifest.content_sha256 = Some(manifest.calculated_content_sha256().unwrap());
    assert!(matches!(
        manifest.validate(),
        Err(RootfsManifestError::NonCanonicalImmutableInventory)
    ));
}

#[test]
#[ignore = "requires MOBILE_LINUX_TEST_ARCHIVE pointing to the pinned published archive"]
fn published_archive_stages_verifies_and_activates_real_tree() {
    let archive = std::path::PathBuf::from(
        std::env::var_os("MOBILE_LINUX_TEST_ARCHIVE")
            .expect("set MOBILE_LINUX_TEST_ARCHIVE to the published archive"),
    );
    let temporary = tempfile::tempdir().unwrap();
    let prepared = temporary.path().join("prepared");
    std::fs::create_dir(&prepared).unwrap();
    let manifest = published();
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
    assert_eq!(report.verified_files, 6093);
    // Activation must not erase attestation when the runtime persists a manifest.
    let roundtrip: RootfsManifest =
        serde_json::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    roundtrip.validate().unwrap();
    assert_eq!(roundtrip.content_sha256, manifest.content_sha256);
}
