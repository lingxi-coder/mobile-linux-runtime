//! Manifest schema and validation of the immutable rootfs contract.
use crate::filesystem::sha256_bytes;
use crate::{path_is_within_guest_path, validate_guest_path, RootfsManifestError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

pub(crate) const ALLOWED_WRITABLE_GUEST_PATHS: &[&str] = &[
    mobile_linux_api::mobile_linux::guest_paths::HOME,
    mobile_linux_api::mobile_linux::guest_paths::SCRATCH[0],
    mobile_linux_api::mobile_linux::guest_paths::SCRATCH[1],
    mobile_linux_api::mobile_linux::guest_paths::WORKSPACE_ROOT,
];

pub(crate) const REQUIRED_ROOTFS_PACKAGES: &[&str] = &[
    "busybox",
    "git",
    "openssh-client",
    "python3",
    "ca-certificates",
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootfsManifest {
    pub schema_version: u32,
    pub runtime: String,
    pub platform: String,
    pub abi: String,
    pub rootfs_version: String,
    /// Digest of the producer's canonical, ordered immutable inventory.
    /// Absent only for legacy manifests emitted before producer attestation.
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_sha256: Option<String>,
    /// Sibling evidence filename, never a path inside the rootfs.
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub sbom_filename: Option<String>,
    /// Sibling source-pin evidence filename.
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_pins_filename: Option<String>,
    pub archive: RootfsArchive,
    pub packages: Vec<RootfsPackage>,
    pub executable_allowlist: Vec<RootfsManifestEntry>,
    pub immutable_files: Vec<RootfsImmutableEntry>,
    pub writable_paths: Vec<String>,
}

fn present_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    <String as serde::Deserialize>::deserialize(deserializer).map(Some)
}

impl RootfsManifest {
    pub fn validate(&self) -> Result<(), RootfsManifestError> {
        if self.schema_version != 2 {
            return Err(RootfsManifestError::InvalidSchemaVersion);
        }
        if self.runtime.trim().is_empty()
            || self.platform.trim().is_empty()
            || self.abi.trim().is_empty()
            || self.rootfs_version.trim().is_empty()
            || self.archive.filename.trim().is_empty()
        {
            return Err(RootfsManifestError::MissingRequiredField);
        }
        let runtime_matches_platform = matches!(
            (self.runtime.as_str(), self.platform.as_str()),
            ("android-proot", "android") | ("ios-ish", "ios")
        );
        let abi_is_supported = matches!(self.abi.as_str(), "arm64" | "x86_64")
            && !(self.platform == "ios" && self.abi != "arm64");
        if !runtime_matches_platform || !abi_is_supported {
            return Err(RootfsManifestError::UnsupportedTarget {
                runtime: self.runtime.clone(),
                platform: self.platform.clone(),
                abi: self.abi.clone(),
            });
        }
        if !is_single_safe_component(&self.rootfs_version)
            || !is_single_safe_component(&self.archive.filename)
        {
            return Err(RootfsManifestError::UnsafeArtifactName);
        }
        if self.packages.is_empty()
            || self.executable_allowlist.is_empty()
            || self.immutable_files.is_empty()
        {
            return Err(RootfsManifestError::MissingRequiredField);
        }
        if self.archive.size_bytes == 0 || !is_lowercase_sha256(&self.archive.sha256) {
            return Err(RootfsManifestError::InvalidSha256(
                self.archive.sha256.clone(),
            ));
        }

        let mut package_names = BTreeSet::new();
        for package in &self.packages {
            if package.name.trim().is_empty()
                || package.version.trim().is_empty()
                || package.license.trim().is_empty()
                || package.architecture.trim().is_empty()
                || package.origin.trim().is_empty()
                || !package_names.insert(package.name.as_str())
            {
                return Err(RootfsManifestError::InvalidPackage(package.name.clone()));
            }
        }
        for required in REQUIRED_ROOTFS_PACKAGES {
            // Alpine's real client is split into these two packages. Keep the
            // legacy aggregate spelling valid for existing schema-v2 fixtures.
            let split_ssh_client = *required == "openssh-client"
                && package_names.contains("openssh-client-default")
                && package_names.contains("openssh-client-common");
            if !package_names.contains(required) && !split_ssh_client {
                return Err(RootfsManifestError::MissingRequiredPackage(
                    (*required).to_string(),
                ));
            }
        }

        let mut seen = BTreeSet::new();
        for entry in &self.executable_allowlist {
            validate_guest_path(&entry.path)?;
            if !seen.insert(entry.path.clone()) {
                return Err(RootfsManifestError::DuplicatePath(entry.path.clone()));
            }
            if !is_lowercase_sha256(&entry.sha256) {
                return Err(RootfsManifestError::InvalidSha256(entry.sha256.clone()));
            }
        }

        let mut writable_paths = BTreeSet::new();
        for path in &self.writable_paths {
            validate_guest_path(path)?;
            if !writable_paths.insert(path.as_str()) {
                return Err(RootfsManifestError::DuplicatePath(path.clone()));
            }
            if !ALLOWED_WRITABLE_GUEST_PATHS.contains(&path.as_str()) {
                return Err(RootfsManifestError::WritablePathNotAllowed(path.clone()));
            }
        }
        for required in ALLOWED_WRITABLE_GUEST_PATHS {
            if !writable_paths.contains(required) {
                return Err(RootfsManifestError::MissingWritablePath(
                    (*required).to_string(),
                ));
            }
        }

        self.validate_integrity_inventory()?;
        self.validate_producer_metadata()
    }

    /// Reproduce rootfs_tool.py canonical_json_bytes(immutable_files): array
    /// order is retained, object keys are alphabetical, strings are UTF-8, and
    /// there is no whitespace or trailing newline. A dedicated serialization
    /// record avoids relying on serde_json's workspace map-order features.
    pub fn calculated_content_sha256(&self) -> Result<String, RootfsManifestError> {
        #[derive(serde::Serialize)]
        struct CanonicalEntry<'a> {
            kind: RootfsImmutableKind,
            path: &'a str,
            sha256: &'a str,
            size_bytes: u64,
        }
        let inventory = self
            .immutable_files
            .iter()
            .map(|entry| CanonicalEntry {
                kind: entry.kind,
                path: &entry.path,
                sha256: &entry.sha256,
                size_bytes: entry.size_bytes,
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&inventory)
            .map_err(|error| RootfsManifestError::ContentDigestEncoding(error.to_string()))?;
        Ok(sha256_bytes(&bytes))
    }

    fn validate_producer_metadata(&self) -> Result<(), RootfsManifestError> {
        let (digest, sbom, pins) = match (
            self.content_sha256.as_deref(),
            self.sbom_filename.as_deref(),
            self.source_pins_filename.as_deref(),
        ) {
            (None, None, None) => return Ok(()), // Original schema-v2 manifests.
            (Some(digest), Some(sbom), Some(pins)) => (digest, sbom, pins),
            _ => return Err(RootfsManifestError::IncompleteProducerMetadata),
        };
        if !is_lowercase_sha256(digest) {
            return Err(RootfsManifestError::InvalidSha256(digest.to_string()));
        }
        // These are const values in the published schema. Exact matching also
        // forbids traversal, absolute paths, Windows separators and NUL bytes.
        for (field, value, expected) in [
            ("sbom_filename", sbom, "rootfs.spdx.json"),
            ("source_pins_filename", pins, "mobile-linux-pins.json"),
        ] {
            if value != expected {
                return Err(RootfsManifestError::InvalidEvidenceFilename {
                    field: field.to_string(),
                    value: value.to_string(),
                });
            }
        }
        if self
            .immutable_files
            .windows(2)
            .any(|pair| pair[0].path >= pair[1].path)
        {
            return Err(RootfsManifestError::NonCanonicalImmutableInventory);
        }
        let actual = self.calculated_content_sha256()?;
        if digest != actual {
            return Err(RootfsManifestError::ContentDigestMismatch {
                expected: digest.to_string(),
                actual,
            });
        }
        Ok(())
    }

    fn validate_integrity_inventory(&self) -> Result<(), RootfsManifestError> {
        let mut immutable_by_path = BTreeMap::new();
        for entry in &self.immutable_files {
            validate_guest_path(&entry.path)?;
            if immutable_by_path
                .insert(entry.path.as_str(), entry)
                .is_some()
            {
                return Err(RootfsManifestError::DuplicatePath(entry.path.clone()));
            }
            if !is_lowercase_sha256(&entry.sha256) {
                return Err(RootfsManifestError::InvalidSha256(entry.sha256.clone()));
            }
            if ALLOWED_WRITABLE_GUEST_PATHS
                .iter()
                .any(|writable| path_is_within_guest_path(&entry.path, writable))
            {
                return Err(RootfsManifestError::ImmutableFileInWritablePath(
                    entry.path.clone(),
                ));
            }
        }

        for entry in &self.executable_allowlist {
            if ALLOWED_WRITABLE_GUEST_PATHS
                .iter()
                .any(|writable| path_is_within_guest_path(&entry.path, writable))
            {
                return Err(RootfsManifestError::ExecutableInWritablePath(
                    entry.path.clone(),
                ));
            }
            let Some(immutable) = immutable_by_path.get(entry.path.as_str()) else {
                return Err(RootfsManifestError::ExecutableInventoryMismatch(
                    entry.path.clone(),
                ));
            };
            if immutable.kind != RootfsImmutableKind::RegularFile
                || immutable.sha256 != entry.sha256
                || entry
                    .size_bytes
                    .is_some_and(|size| size != immutable.size_bytes)
            {
                return Err(RootfsManifestError::ExecutableInventoryMismatch(
                    entry.path.clone(),
                ));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootfsArchive {
    pub filename: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootfsPackage {
    pub name: String,
    pub version: String,
    pub license: String,
    pub architecture: String,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootfsManifestEntry {
    pub path: String,
    pub sha256: String,
    pub kind: RootfsEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootfsEntryKind {
    Elf,
    SharedLibrary,
    Interpreter,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootfsImmutableEntry {
    pub path: String,
    pub sha256: String,
    pub kind: RootfsImmutableKind,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootfsImmutableKind {
    RegularFile,
    Symlink,
}

fn is_single_safe_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
