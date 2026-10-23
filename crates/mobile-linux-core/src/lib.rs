//! Rootfs inventory, verification, staging and atomic activation.
#![forbid(unsafe_code)]
#![allow(missing_docs)]

mod error;
mod filesystem;
mod guest_path;
mod manifest;
mod store;
mod user_workspace;

pub use error::{RootfsManifestError, RootfsStoreError};
pub use guest_path::{
    normalized_guest_path, path_is_within_guest_path, resolve_guest_path, validate_guest_path,
    GuestPathError,
};
pub use manifest::{
    RootfsArchive, RootfsEntryKind, RootfsImmutableEntry, RootfsImmutableKind, RootfsManifest,
    RootfsManifestEntry, RootfsPackage,
};
pub use store::{RootfsStore, RootfsVerificationIssue, RootfsVerificationReport};
pub use user_workspace::is_persistent_user_workspace;

#[cfg(test)]
mod tests;
