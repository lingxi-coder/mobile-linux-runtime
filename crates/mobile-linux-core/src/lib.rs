//! Rootfs inventory, verification, staging and atomic activation.
#![forbid(unsafe_code)]
#![allow(missing_docs)]

mod error;
mod filesystem;
pub mod guest_path;
mod manifest;
mod store;

pub use error::{RootfsManifestError, RootfsStoreError};
pub use guest_path::{normalized_guest_path, path_is_within_guest_path, validate_guest_path};
pub use manifest::{
    RootfsArchive, RootfsEntryKind, RootfsImmutableEntry, RootfsImmutableKind, RootfsManifest,
    RootfsManifestEntry, RootfsPackage,
};
pub use store::{RootfsStore, RootfsVerificationIssue, RootfsVerificationReport};

#[cfg(test)]
mod tests;
