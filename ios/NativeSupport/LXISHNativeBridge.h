//
//  LXISHNativeBridge.h
//  LingxiCode
//
//  Minimal C ABI for the OpenMinis-derived iSH bridge.
//  This bridge is intended for Rust/host callers that prefer a JSON envelope
//  over direct Objective-C/Swift bindings.
//
//  JSON request keys:
//    config:
//      managed_root, workspace_host_path, stable_workspace_id, abi,
//      rootfs_version, archive_sha256?, authorization_file?
//    mount:
//      host_path, guest_path, read_only, purpose
//    run request:
//      command, args, cwd?, env, stdin?, timeout_ms?, mounts?
//    pty open request:
//      command, args, cwd?, env, cols, rows, mounts?
//    poll request:
//      after_sequence?, limit?
//
//  All string-returning functions return UTF-8 JSON allocated with `strdup`.
//  Callers must release them with `mlr_ish_free_string`.
//

#pragma once

#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void mlr_ish_set_overlay_bundle(const char *path);
bool mlr_ish_is_available(void);

char *mlr_ish_availability_json(void);
char *mlr_ish_install_rootfs_json(const char *config_json);
char *mlr_ish_repair_rootfs_json(const char *config_json);
char *mlr_ish_reset_rootfs_json(const char *config_json);
char *mlr_ish_boot_json(const char *config_json);
char *mlr_ish_configure_mounts_json(const char *config_json, const char *mounts_json);
char *mlr_ish_run_sync_json(const char *config_json, const char *request_json);
char *mlr_ish_background_spawn_json(const char *config_json, const char *request_json);
char *mlr_ish_background_kill_json(const char *config_json, const char *request_json);
char *mlr_ish_background_poll_json(const char *config_json, const char *request_json);
char *mlr_ish_raw_stdio_open_json(const char *config_json, const char *request_json);
char *mlr_ish_raw_stdio_write_json(const char *config_json, const char *request_json);
char *mlr_ish_raw_stdio_read_json(const char *config_json, const char *request_json);
char *mlr_ish_raw_stdio_close_json(const char *config_json, const char *request_json);
char *mlr_ish_raw_stdio_dispose_json(const char *config_json, const char *request_json);
char *mlr_ish_probe_loopback_json(const char *config_json, const char *request_json);
char *mlr_ish_pty_open_json(const char *config_json, const char *request_json);
char *mlr_ish_pty_write_json(const char *config_json, const char *request_json);
char *mlr_ish_pty_resize_json(const char *config_json, const char *request_json);
char *mlr_ish_pty_close_json(const char *config_json, const char *request_json);
char *mlr_ish_poll_output_json(const char *config_json, const char *request_json);

void mlr_ish_free_string(char *value);

#ifdef __cplusplus
}
#endif
