#[cfg(all(target_os = "ios", not(target_abi = "sim")))]
// The device bridge is the sole unsafe boundary in this crate: it validates
// Rust strings before crossing the C ABI and immediately copies/frees every
// owned string returned by the native iSH shim.
#[allow(unsafe_code)]
mod device {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    unsafe extern "C" {
        fn mlr_ish_is_available() -> bool;
        fn mlr_ish_availability_json() -> *mut c_char;
        fn mlr_ish_install_rootfs_json(config_json: *const c_char) -> *mut c_char;
        fn mlr_ish_repair_rootfs_json(config_json: *const c_char) -> *mut c_char;
        fn mlr_ish_reset_rootfs_json(config_json: *const c_char) -> *mut c_char;
        fn mlr_ish_boot_json(config_json: *const c_char) -> *mut c_char;
        fn mlr_ish_configure_mounts_json(
            config_json: *const c_char,
            mounts_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_background_spawn_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_background_kill_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_background_poll_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_raw_stdio_open_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_raw_stdio_write_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_raw_stdio_read_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_raw_stdio_close_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_raw_stdio_dispose_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_probe_loopback_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_pty_open_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_pty_write_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_pty_resize_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_pty_close_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_poll_output_json(
            config_json: *const c_char,
            request_json: *const c_char,
        ) -> *mut c_char;
        fn mlr_ish_free_string(value: *mut c_char);
    }

    pub fn is_available() -> bool {
        // SAFETY: pure availability probe with no arguments or aliasing.
        unsafe { mlr_ish_is_available() }
    }

    pub fn availability_json() -> Result<String, String> {
        // SAFETY: no arguments or aliasing; ownership is transferred via the returned string pointer.
        unsafe { take_owned_string(mlr_ish_availability_json()) }
    }

    pub fn install_rootfs_json(config_json: &str) -> Result<String, String> {
        call_unary(config_json, mlr_ish_install_rootfs_json)
    }

    pub fn repair_rootfs_json(config_json: &str) -> Result<String, String> {
        call_unary(config_json, mlr_ish_repair_rootfs_json)
    }

    pub fn reset_rootfs_json(config_json: &str) -> Result<String, String> {
        call_unary(config_json, mlr_ish_reset_rootfs_json)
    }

    pub fn boot_json(config_json: &str) -> Result<String, String> {
        call_unary(config_json, mlr_ish_boot_json)
    }

    pub fn configure_mounts_json(config_json: &str, mounts_json: &str) -> Result<String, String> {
        call_binary(config_json, mounts_json, mlr_ish_configure_mounts_json)
    }

    pub fn spawn_background_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_background_spawn_json)
    }

    pub fn kill_background_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_background_kill_json)
    }

    pub fn poll_background_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_background_poll_json)
    }

    pub fn raw_stdio_open_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_raw_stdio_open_json)
    }

    pub fn raw_stdio_write_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_raw_stdio_write_json)
    }

    pub fn raw_stdio_read_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_raw_stdio_read_json)
    }

    pub fn raw_stdio_close_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_raw_stdio_close_json)
    }

    pub fn raw_stdio_dispose_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_raw_stdio_dispose_json)
    }

    pub fn probe_loopback_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_probe_loopback_json)
    }

    pub fn pty_open_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_pty_open_json)
    }

    pub fn pty_write_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_pty_write_json)
    }

    pub fn pty_resize_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_pty_resize_json)
    }

    pub fn pty_close_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_pty_close_json)
    }

    pub fn poll_output_json(config_json: &str, request_json: &str) -> Result<String, String> {
        call_binary(config_json, request_json, mlr_ish_poll_output_json)
    }

    fn call_unary(
        value: &str,
        function: unsafe extern "C" fn(*const c_char) -> *mut c_char,
    ) -> Result<String, String> {
        let value = CString::new(value).map_err(|_| "payload contains NUL".to_string())?;
        // SAFETY: the CString is NUL-terminated and lives for the duration of the call.
        unsafe { take_owned_string(function(value.as_ptr())) }
    }

    fn call_binary(
        left: &str,
        right: &str,
        function: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_char,
    ) -> Result<String, String> {
        let left = CString::new(left).map_err(|_| "payload contains NUL".to_string())?;
        let right = CString::new(right).map_err(|_| "payload contains NUL".to_string())?;
        // SAFETY: both CStrings are NUL-terminated and live for the duration of the call.
        unsafe { take_owned_string(function(left.as_ptr(), right.as_ptr())) }
    }

    unsafe fn take_owned_string(value: *mut c_char) -> Result<String, String> {
        if value.is_null() {
            return Err("native bridge returned a null string".to_string());
        }
        let owned = CStr::from_ptr(value).to_string_lossy().into_owned();
        mlr_ish_free_string(value);
        Ok(owned)
    }
}

#[cfg(not(all(target_os = "ios", not(target_abi = "sim"))))]
mod device {
    fn unavailable() -> String {
        if cfg!(target_os = "ios") {
            "native iSH runtime is unavailable on the iOS simulator".to_string()
        } else {
            "native iSH runtime is only available on physical iOS devices".to_string()
        }
    }

    pub fn is_available() -> bool {
        false
    }

    pub fn availability_json() -> Result<String, String> {
        Err(unavailable())
    }

    pub fn raw_stdio_open_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn raw_stdio_write_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn raw_stdio_read_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn raw_stdio_close_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn raw_stdio_dispose_json(
        _config_json: &str,
        _request_json: &str,
    ) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn install_rootfs_json(_config_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn repair_rootfs_json(_config_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn reset_rootfs_json(_config_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn boot_json(_config_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn configure_mounts_json(_config_json: &str, _mounts_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn spawn_background_json(
        _config_json: &str,
        _request_json: &str,
    ) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn kill_background_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn poll_background_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn probe_loopback_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn pty_open_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn pty_write_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn pty_resize_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn pty_close_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }

    pub fn poll_output_json(_config_json: &str, _request_json: &str) -> Result<String, String> {
        Err(unavailable())
    }
}

pub use device::*;
