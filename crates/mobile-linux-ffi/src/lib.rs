//! Standalone Mobile Linux FFI. No application, Harness, or client protocol dependency.

uniffi::setup_scaffolding!("mobile_linux_runtime");

mod types;
pub use types::{
    EnforcementReceiptFfi, MobileLinuxApiErrorFfi, MobileLinuxCapabilityFfi,
    MobileLinuxCommandRequestFfi, MobileLinuxCommandResultFfi, MobileLinuxEnvEntryFfi,
    MobileLinuxEventFfi, MobileLinuxEventKindFfi, MobileLinuxMountPurposeFfi,
    MobileLinuxMountSpecFfi, MobileLinuxProcessHandleFfi, MobileLinuxPtyOpenRequestFfi,
    MobileLinuxPtySessionHandleFfi, MobileLinuxPtySizeFfi, MobileLinuxRootfsStateFfi,
    MobileLinuxRuntimeModeFfi, MobileLinuxStatusFfi, MobileLinuxTaskKindFfi,
    MobileLinuxTaskSnapshotFfi, MobileLinuxTaskStateFfi, NetworkPolicyFfi, RawStdioHandle,
    RawStdioOutput, ResourceLimitsFfi, RuntimeConfig, RuntimePlatform,
};
mod conversion;
mod events;
pub use events::{RuntimeEventSink, RuntimeStreamSink};
mod runtime;
pub use runtime::{create_runtime, RuntimeHandle};

#[cfg(test)]
mod tests;
