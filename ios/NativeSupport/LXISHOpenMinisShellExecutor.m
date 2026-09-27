// Device-only OpenMinis shell executor used by LXISHNativeBridge.
// SPDX-License-Identifier: GPL-3.0-only

#import <TargetConditionals.h>

#if TARGET_OS_IOS && !TARGET_OS_SIMULATOR
#import "ISHShellExecutor.m"
#endif
