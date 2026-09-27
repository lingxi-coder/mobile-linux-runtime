// Explicit symbol references retain dynamically discovered Objective-C classes in static SDK consumers.
#import <Foundation/Foundation.h>
#import <TargetConditionals.h>
#if TARGET_OS_IOS && !TARGET_OS_SIMULATOR
#import "ISHKernel.h"
#import "ISHShellExecutor.h"
#endif
bool mlr_ish_classes_linked(void) {
#if TARGET_OS_IOS && !TARGET_OS_SIMULATOR
    return [ISHKernel class] != Nil && [ISHShellExecutor class] != Nil;
#else
    return false;
#endif
}
