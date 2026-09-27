//
//  LXISHShellExecutorBridge.m
//  LingxiCode
//
//  Derived from the OpenMinis iSH integration surface (`ISHShellExecutor`) and
//  reduced to the synchronous command path needed by the Rust-facing C ABI.
//
//  SPDX-License-Identifier: GPL-3.0-only
//

#import "LXISHShellExecutorBridge.h"

#import <TargetConditionals.h>

static NSString *const LXISHShellBridgeErrorDomain = @"org.mobile-linux.ish-native.shell";

@implementation LXISHShellExecutionResult
@end

@implementation LXISHShellExecutorBridge

+ (BOOL)isDeviceBridgeAvailable {
#if TARGET_OS_SIMULATOR
    return NO;
#else
    Class executorClass = NSClassFromString(@"ISHShellExecutor");
    return executorClass != Nil && [executorClass respondsToSelector:@selector(executeCommandSync:timeout:lineCallback:)];
#endif
}

+ (NSString *)availabilityReason {
#if TARGET_OS_SIMULATOR
    return @"iSH shell executor is disabled in the iOS Simulator";
#else
    if (![self isDeviceBridgeAvailable]) {
        return @"OpenMinis ISHShellExecutor is not linked into the app target";
    }
    return @"";
#endif
}

- (nullable LXISHShellExecutionResult *)runCommand:(NSString *)command
                                           timeout:(NSTimeInterval)timeout
                                             error:(NSError * _Nullable __autoreleasing *)error {
    if (!LXISHShellExecutorBridge.isDeviceBridgeAvailable) {
        if (error) {
            *error = [NSError errorWithDomain:LXISHShellBridgeErrorDomain
                                         code:1
                                     userInfo:@{NSLocalizedDescriptionKey: LXISHShellExecutorBridge.availabilityReason}];
        }
        return nil;
    }

    Class executorClass = NSClassFromString(@"ISHShellExecutor");
    SEL selector = @selector(executeCommandSync:timeout:lineCallback:);
    id (*fn)(id, SEL, NSString *, double, id) =
        (id (*)(id, SEL, NSString *, double, id))[executorClass methodForSelector:selector];
    id rawResult = fn(executorClass, selector, command, timeout, nil);
    if (!rawResult) {
        if (error) {
            *error = [NSError errorWithDomain:LXISHShellBridgeErrorDomain
                                         code:2
                                     userInfo:@{NSLocalizedDescriptionKey: @"ISHShellExecutor returned no result"}];
        }
        return nil;
    }

    LXISHShellExecutionResult *result = [[LXISHShellExecutionResult alloc] init];
    result.exitCode = [LXISHShellExecutorBridge intValueFromObject:rawResult selector:@selector(exitCode)];
    result.errorCode = [LXISHShellExecutorBridge intValueFromObject:rawResult selector:@selector(error)];
    result.stdoutText = [LXISHShellExecutorBridge stringValueFromObject:rawResult selector:@selector(output)] ?: @"";
    result.stderrText = [LXISHShellExecutorBridge stringValueFromObject:rawResult selector:@selector(errorOutput)] ?: @"";
    result.durationSeconds = [LXISHShellExecutorBridge doubleValueFromObject:rawResult selector:@selector(duration)];
    return result;
}

+ (int)intValueFromObject:(id)object selector:(SEL)selector {
    if (![object respondsToSelector:selector]) {
        return 0;
    }
    int (*fn)(id, SEL) = (int (*)(id, SEL))[object methodForSelector:selector];
    return fn(object, selector);
}

+ (double)doubleValueFromObject:(id)object selector:(SEL)selector {
    if (![object respondsToSelector:selector]) {
        return 0;
    }
    double (*fn)(id, SEL) = (double (*)(id, SEL))[object methodForSelector:selector];
    return fn(object, selector);
}

+ (NSString *)stringValueFromObject:(id)object selector:(SEL)selector {
    if (![object respondsToSelector:selector]) {
        return nil;
    }
    id (*fn)(id, SEL) = (id (*)(id, SEL))[object methodForSelector:selector];
    id value = fn(object, selector);
    return [value isKindOfClass:[NSString class]] ? value : nil;
}

@end
