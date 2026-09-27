use super::*;
use crate::conversion::{
    event_to_ffi, mobile_linux_error_to_ffi, process_handle_to_ffi, process_handle_to_traits,
};
use mobile_linux_api as platform_api;
#[test]
fn event_adapter_preserves_non_utf8_and_nul_bytes() {
    let bytes = vec![0, 255, 128, 10];
    let value = event_to_ffi(platform_api::MobileLinuxEvent {
        sequence: 9,
        task_id: Some("task".into()),
        kind: platform_api::MobileLinuxEventKind::PtyOutput {
            session_id: "pty".into(),
            data: bytes.clone(),
        },
    });
    assert_eq!(value.data, Some(bytes));
    assert_eq!(value.sequence, 9);
}
#[test]
fn process_handle_keeps_enforcement_receipt() {
    let original = platform_api::LinuxProcessHandle {
        id: "task".into(),
        enforcement: platform_api::LinuxEnforcementReceipt {
            network_policy_enforced: true,
            memory_limit_enforced: true,
        },
    };
    assert_eq!(
        process_handle_to_traits(process_handle_to_ffi(original.clone())).unwrap(),
        original
    );
}
#[test]
fn restart_required_is_not_a_generic_io_failure() {
    assert!(
        matches!(mobile_linux_error_to_ffi(platform_api::MobileLinuxError::RestartRequired("kernel".into())),MobileLinuxApiErrorFfi::RestartRequired {detail} if detail=="kernel")
    );
}
