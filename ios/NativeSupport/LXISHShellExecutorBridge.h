//
//  LXISHShellExecutorBridge.h
//  LingxiCode
//
//  Derived from the OpenMinis iSH integration surface (`ISHShellExecutor`) and
//  reduced to the synchronous command path needed by the Rust-facing C ABI.
//
//  SPDX-License-Identifier: GPL-3.0-only
//

#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

@interface LXISHShellExecutionResult : NSObject
@property (nonatomic, assign) int exitCode;
@property (nonatomic, assign) int errorCode;
@property (nonatomic, copy) NSString *stdoutText;
@property (nonatomic, copy) NSString *stderrText;
@property (nonatomic, assign) double durationSeconds;
@end

@interface LXISHShellExecutorBridge : NSObject

+ (BOOL)isDeviceBridgeAvailable;
+ (NSString *)availabilityReason;

- (nullable LXISHShellExecutionResult *)runCommand:(NSString *)command
                                           timeout:(NSTimeInterval)timeout
                                             error:(NSError * _Nullable * _Nullable)error;

@end

NS_ASSUME_NONNULL_END
