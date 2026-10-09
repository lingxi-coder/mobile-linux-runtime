//! Android PRoot implementation of the shared mobile-Linux runtime seam.
//!
//! PRoot is a compatibility layer, not a security boundary. Admission remains
//! the responsibility of `MobileLinuxSandbox`; this module owns process and
//! rootfs lifecycle and fails closed when its native/runtime payload is absent.

#![forbid(unsafe_code)]

mod runtime;

pub use runtime::{AndroidProotRuntime, AndroidProotRuntimeConfig};
