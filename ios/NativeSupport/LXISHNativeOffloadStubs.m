// LingXi exposes the Linux userspace runtime without OpenMinis' Apple-service
// native command offloads. These no-op registrations keep the upstream kernel
// wrapper focused on Linux execution and avoid pulling unrelated entitlements.
// SPDX-License-Identifier: GPL-3.0-only

#import <TargetConditionals.h>

#if TARGET_OS_IOS && !TARGET_OS_SIMULATOR
void ffmpeg_offload_register(void) {}
void calendar_offload_register(void) {}
void location_offload_register(void) {}
void weather_offload_register(void) {}
void vision_offload_register(void) {}
void open_offload_register(void) {}
void clipboard_offload_register(void) {}
void healthkit_offload_register(void) {}
void photos_offload_register(void) {}
void maps_offload_register(void) {}
void nlp_offload_register(void) {}
void alarm_offload_register(void) {}
void media_offload_register(void) {}
void speak_offload_register(void) {}
void speech_offload_register(void) {}
void device_offload_register(void) {}
void homekit_offload_register(void) {}
void notification_offload_register(void) {}
void player_offload_register(void) {}
void model_use_offload_register(void) {}
void reminders_offload_register(void) {}
void bluetooth_offload_register(void) {}
void nfc_offload_register(void) {}
void sessions_offload_register(void) {}
void config_offload_register(void) {}
void browser_use_offload_register(void) {}
void debug_offload_register(void) {}
#else
void mlr_ish_set_overlay_bundle(const char *path) { (void)path; }
#endif
