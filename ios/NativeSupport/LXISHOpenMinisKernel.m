// OpenMinis' iSH kernel wrapper is compiled only for physical iOS devices.
// Keeping the upstream source included here makes the pinned implementation
// auditable while leaving simulator builds on the explicit unavailable path.
// SPDX-License-Identifier: GPL-3.0-only

#import <TargetConditionals.h>

#if TARGET_OS_IOS && !TARGET_OS_SIMULATOR
#import "ISHKernel.m"
#endif
