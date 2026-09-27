//! iOS iSH-backed implementation of the shared mobile-linux runtime seam.
//!
//! The native integration is intentionally isolated to `native` so ABI drift is
//! contained to one module. Host and simulator builds compile a safe
//! unavailable stub rather than trying to link the device-only symbols.

mod runtime;

pub use runtime::{linked_runtime, IosIshRuntime, IosIshRuntimeConfig};
