// Device-only OpenMinis current-root support used by ISHKernel.
// SPDX-License-Identifier: GPL-3.0-only

#import <TargetConditionals.h>

#if TARGET_OS_IOS && !TARGET_OS_SIMULATOR
#import "CurrentRoot.m"
#endif
