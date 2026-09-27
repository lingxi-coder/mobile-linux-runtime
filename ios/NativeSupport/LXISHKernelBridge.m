//
//  LXISHKernelBridge.m
//  LingxiCode
//
//  Derived from the OpenMinis iSH integration surface (`ISHKernel`) and kept
//  intentionally small for LingXi's mobile-linux runtime bridge.
//
//  SPDX-License-Identifier: GPL-3.0-only
//

#import "LXISHKernelBridge.h"

#import <TargetConditionals.h>
#import <objc/message.h>

static NSString *const LXISHBridgeErrorDomain = @"org.mobile-linux.ish-native.kernel";

@interface LXISHKernelBridge ()
@property (nonatomic, strong, nullable) id kernel;
@property (nonatomic, copy, nullable) LXISHOutputSink sink;
@property (nonatomic, strong) NSMutableArray<NSString *> *mountedGuestPaths;
@property (nonatomic, assign) BOOL interactiveShellOpen;
@end

@implementation LXISHKernelBridge

+ (BOOL)isDeviceBridgeAvailable {
#if TARGET_OS_SIMULATOR
    return NO;
#else
    Class kernelClass = NSClassFromString(@"ISHKernel");
    return kernelClass != Nil && [kernelClass respondsToSelector:@selector(shared)];
#endif
}

+ (NSString *)availabilityReason {
#if TARGET_OS_SIMULATOR
    return @"iSH bridge is disabled in the iOS Simulator";
#else
    if (![self isDeviceBridgeAvailable]) {
        return @"OpenMinis ISHKernel is not linked into the app target";
    }
    return @"";
#endif
}

- (instancetype)init {
    self = [super init];
    if (self) {
        _mountedGuestPaths = [NSMutableArray array];
    }
    return self;
}

- (BOOL)bootWithRootPath:(NSString *)rootPath error:(NSError * _Nullable __autoreleasing *)error {
    id kernel = [self resolveKernel:error];
    if (!kernel) {
        return NO;
    }
    SEL selector = @selector(bootWithRootPath:);
    if (![kernel respondsToSelector:selector]) {
        return [self fail:@"ISHKernel is missing bootWithRootPath:" code:2 error:error];
    }
    int (*fn)(id, SEL, NSString *) = (int (*)(id, SEL, NSString *))[kernel methodForSelector:selector];
    int result = fn(kernel, selector, rootPath);
    if (result < 0) {
        return [self fail:[NSString stringWithFormat:@"ISHKernel boot failed: %d", result] code:3 error:error];
    }
    self.kernel = kernel;
    return YES;
}

- (BOOL)configureMounts:(NSArray<NSDictionary<NSString *,id> *> *)mounts
                  error:(NSError * _Nullable __autoreleasing *)error {
    id kernel = [self resolveKernel:error];
    if (!kernel) {
        return NO;
    }

    SEL unmountSelector = @selector(bindUnmountPath:);
    if ([kernel respondsToSelector:unmountSelector]) {
        int (*unmountFn)(id, SEL, NSString *) = (int (*)(id, SEL, NSString *))[kernel methodForSelector:unmountSelector];
        for (NSString *guestPath in [self.mountedGuestPaths reverseObjectEnumerator]) {
            (void)unmountFn(kernel, unmountSelector, guestPath);
        }
    }
    [self.mountedGuestPaths removeAllObjects];

    SEL mountSelector = @selector(bindMountPath:toHostPath:readOnly:);
    if (![kernel respondsToSelector:mountSelector]) {
        return [self fail:@"ISHKernel is missing bindMountPath:toHostPath:readOnly:" code:4 error:error];
    }
    int (*mountFn)(id, SEL, NSString *, NSString *, BOOL) =
        (int (*)(id, SEL, NSString *, NSString *, BOOL))[kernel methodForSelector:mountSelector];

    for (NSDictionary<NSString *, id> *mount in mounts) {
        NSString *guestPath = mount[@"guest_path"];
        NSString *hostPath = mount[@"host_path"];
        NSNumber *readOnly = mount[@"read_only"];
        if (guestPath.length == 0 || hostPath.length == 0) {
            return [self fail:@"Mount entries require guest_path and host_path" code:5 error:error];
        }
        int result = mountFn(kernel, mountSelector, guestPath, hostPath, readOnly.boolValue);
        if (result < 0) {
            return [self fail:[NSString stringWithFormat:@"bind mount failed for %@ -> %@ (%d)", guestPath, hostPath, result]
                         code:6
                        error:error];
        }
        [self.mountedGuestPaths addObject:guestPath];
    }
    return YES;
}

- (BOOL)openInteractiveShellWithCommand:(NSArray<NSString *> *)command
                                   cols:(uint16_t)cols
                                   rows:(uint16_t)rows
                                   sink:(LXISHOutputSink)sink
                                  error:(NSError * _Nullable __autoreleasing *)error {
    id kernel = [self resolveKernel:error];
    if (!kernel) {
        return NO;
    }
    SEL sinkSelector = @selector(setOutputCallback:);
    if ([kernel respondsToSelector:sinkSelector]) {
        void (*setSink)(id, SEL, id) = (void (*)(id, SEL, id))[kernel methodForSelector:sinkSelector];
        setSink(kernel, sinkSelector, [sink copy]);
        self.sink = [sink copy];
    }

    SEL resizeSelector = @selector(setTerminalSize:rows:);
    if ([kernel respondsToSelector:resizeSelector]) {
        void (*resizeFn)(id, SEL, int, int) = (void (*)(id, SEL, int, int))[kernel methodForSelector:resizeSelector];
        resizeFn(kernel, resizeSelector, cols, rows);
    }

    SEL executeSelector = @selector(executeCommand:);
    if (![kernel respondsToSelector:executeSelector]) {
        return [self fail:@"ISHKernel is missing executeCommand:" code:7 error:error];
    }
    int (*executeFn)(id, SEL, NSArray<NSString *> *) =
        (int (*)(id, SEL, NSArray<NSString *> *))[kernel methodForSelector:executeSelector];
    int result = executeFn(kernel, executeSelector, command);
    if (result < 0) {
        return [self fail:[NSString stringWithFormat:@"interactive shell launch failed: %d", result] code:8 error:error];
    }
    self.interactiveShellOpen = YES;
    return YES;
}

- (BOOL)writeInputData:(NSData *)data error:(NSError * _Nullable __autoreleasing *)error {
    id kernel = [self resolveKernel:error];
    if (!kernel) {
        return NO;
    }
    if (!self.interactiveShellOpen) {
        return [self fail:@"interactive shell is not open" code:9 error:error];
    }
    SEL selector = @selector(sendInput:);
    if (![kernel respondsToSelector:selector]) {
        return [self fail:@"ISHKernel is missing sendInput:" code:10 error:error];
    }
    void (*fn)(id, SEL, NSData *) = (void (*)(id, SEL, NSData *))[kernel methodForSelector:selector];
    fn(kernel, selector, data);
    return YES;
}

- (BOOL)resizeColumns:(uint16_t)cols rows:(uint16_t)rows error:(NSError * _Nullable __autoreleasing *)error {
    id kernel = [self resolveKernel:error];
    if (!kernel) {
        return NO;
    }
    if (!self.interactiveShellOpen) {
        return [self fail:@"interactive shell is not open" code:11 error:error];
    }
    SEL selector = @selector(setTerminalSize:rows:);
    if (![kernel respondsToSelector:selector]) {
        return [self fail:@"ISHKernel is missing setTerminalSize:rows:" code:12 error:error];
    }
    void (*fn)(id, SEL, int, int) = (void (*)(id, SEL, int, int))[kernel methodForSelector:selector];
    fn(kernel, selector, cols, rows);
    return YES;
}

- (BOOL)closeInteractiveShell:(NSError * _Nullable __autoreleasing *)error {
    if (!self.interactiveShellOpen) {
        return YES;
    }
    NSData *exitData = [@"exit\n" dataUsingEncoding:NSUTF8StringEncoding];
    BOOL ok = [self writeInputData:exitData error:error];
    self.interactiveShellOpen = NO;
    return ok;
}

- (id)resolveKernel:(NSError * _Nullable __autoreleasing *)error {
    if (self.kernel) {
        return self.kernel;
    }
    if (!LXISHKernelBridge.isDeviceBridgeAvailable) {
        [self fail:LXISHKernelBridge.availabilityReason code:1 error:error];
        return nil;
    }
    Class kernelClass = NSClassFromString(@"ISHKernel");
    SEL sharedSelector = @selector(shared);
    id (*fn)(id, SEL) = (id (*)(id, SEL))[kernelClass methodForSelector:sharedSelector];
    id kernel = fn(kernelClass, sharedSelector);
    self.kernel = kernel;
    return kernel;
}

- (BOOL)fail:(NSString *)message code:(NSInteger)code error:(NSError * _Nullable __autoreleasing *)error {
    if (error) {
        *error = [NSError errorWithDomain:LXISHBridgeErrorDomain
                                     code:code
                                 userInfo:@{NSLocalizedDescriptionKey: message}];
    }
    return NO;
}

@end
