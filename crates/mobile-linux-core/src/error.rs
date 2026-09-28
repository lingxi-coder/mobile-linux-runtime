//! Rootfs manifest and managed-store failures.
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RootfsManifestError {
    #[error("rootfs manifest schema_version must be exactly 2")]
    InvalidSchemaVersion,
    #[error(
        "rootfs producer metadata must include content_sha256, sbom_filename and source_pins_filename together"
    )]
    IncompleteProducerMetadata,
    #[error("invalid rootfs evidence filename {field}: {value}")]
    InvalidEvidenceFilename { field: String, value: String },
    #[error("rootfs immutable inventory must be ordered by path")]
    NonCanonicalImmutableInventory,
    #[error("rootfs content digest mismatch: expected {expected}, got {actual}")]
    ContentDigestMismatch { expected: String, actual: String },
    #[error("cannot encode rootfs content digest: {0}")]
    ContentDigestEncoding(String),
    #[error("rootfs manifest is missing a required field")]
    MissingRequiredField,
    #[error("rootfs version and archive filename must be single safe path components")]
    UnsafeArtifactName,
    #[error("invalid or duplicate rootfs package: {0}")]
    InvalidPackage(String),
    #[error("required fixed-toolset package is missing: {0}")]
    MissingRequiredPackage(String),
    #[error("unsupported mobile Linux target: runtime={runtime}, platform={platform}, abi={abi}")]
    UnsupportedTarget {
        runtime: String,
        platform: String,
        abi: String,
    },
    #[error("invalid sha256: {0}")]
    InvalidSha256(String),
    #[error("duplicate manifest path: {0}")]
    DuplicatePath(String),
    #[error("invalid guest path: {0}")]
    InvalidGuestPath(String),
    #[error("writable guest path is not allowed: {0}")]
    WritablePathNotAllowed(String),
    #[error("required writable guest path is missing: {0}")]
    MissingWritablePath(String),
    #[error("verified executable or dynamic library lives in a writable guest path: {0}")]
    ExecutableInWritablePath(String),
    #[error("immutable rootfs file lives in a writable guest path: {0}")]
    ImmutableFileInWritablePath(String),
    #[error("executable allowlist entry does not match immutable inventory: {0}")]
    ExecutableInventoryMismatch(String),
}

#[derive(Debug, Error)]
pub enum RootfsStoreError {
    #[error(transparent)]
    Manifest(#[from] RootfsManifestError),
    #[error("io error for {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("archive size mismatch: expected {expected}, got {actual}")]
    ArchiveSizeMismatch { expected: u64, actual: u64 },
    #[error("archive hash mismatch: expected {expected}, got {actual}")]
    ArchiveHashMismatch { expected: String, actual: String },
    #[error("rootfs manifest target {manifest} does not match store target {store}")]
    TargetMismatch { store: String, manifest: String },
    #[error("unsafe reset path: {0}")]
    UnsafeResetPath(PathBuf),
    #[error("unsafe managed path or symlink component: {0}")]
    UnsafeManagedPath(PathBuf),
    #[error("rootfs integrity failure: {0}")]
    Integrity(String),
    #[error("execution denied for {path}: {reason}")]
    ExecutionDenied { path: String, reason: String },
}
