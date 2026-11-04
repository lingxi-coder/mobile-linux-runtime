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

#[test]
fn explicit_brand_profile_preserves_identity_and_rejects_unsafe_components() {
    let selected = BrandRuntimeProfileFfi {
        brand_id: "atlas".into(),
        linux_user: "atlas_guest".into(),
        config_dir_name: ".atlas".into(),
    };
    assert_eq!(
        selected.validated().unwrap(),
        platform_api::BrandRuntimeProfile::new("atlas", "atlas_guest", ".atlas")
    );

    for invalid in [
        BrandRuntimeProfileFfi {
            brand_id: "../atlas".into(),
            ..selected.clone()
        },
        BrandRuntimeProfileFfi {
            linux_user: "root".into(),
            ..selected.clone()
        },
        BrandRuntimeProfileFfi {
            config_dir_name: "../escape".into(),
            ..selected.clone()
        },
    ] {
        assert!(matches!(
            invalid.validated(),
            Err(MobileLinuxApiErrorFfi::InvalidRequest { .. })
        ));
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        let config = RuntimeConfig {
            platform: RuntimePlatform::Android,
            brand_profile: invalid,
            managed_root: root.clone(),
            app_sandbox_root: root,
            abi: "arm64-v8a".into(),
            rootfs_version: "test".into(),
            archive_sha256: None,
            native_library_dir: None,
            workspace_host_path: None,
            stable_workspace_id: None,
            authorization_file: None,
            rootfs_archive_path: None,
            rootfs_patch_path: None,
            default_mount_path: None,
            protected_host_roots: vec![],
            allowed_mount_roots: vec![],
            allowed_guest_roots: vec![],
        };
        assert!(matches!(
            create_runtime(config),
            Err(MobileLinuxApiErrorFfi::InvalidRequest { .. })
        ));
    }
}
