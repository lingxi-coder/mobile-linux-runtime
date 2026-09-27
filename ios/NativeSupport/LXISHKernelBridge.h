//
//  LXISHKernelBridge.h
//  LingxiCode
//
//  Derived from the OpenMinis iSH integration surface (`ISHKernel`) and kept
//  intentionally small for LingXi's mobile-linux runtime bridge.
//
//  SPDX-License-Identifier: GPL-3.0-only
//

#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

typedef void (^LXISHOutputSink)(NSData *data);

@interface LXISHKernelBridge : NSObject

+ (BOOL)isDeviceBridgeAvailable;
+ (NSString *)availabilityReason;

- (BOOL)bootWithRootPath:(NSString *)rootPath error:(NSError * _Nullable * _Nullable)error;
- (BOOL)configureMounts:(NSArray<NSDictionary<NSString *, id> *> *)mounts
                  error:(NSError * _Nullable * _Nullable)error;
- (BOOL)openInteractiveShellWithCommand:(NSArray<NSString *> *)command
                                   cols:(uint16_t)cols
                                   rows:(uint16_t)rows
                                   sink:(LXISHOutputSink)sink
                                  error:(NSError * _Nullable * _Nullable)error;
- (BOOL)writeInputData:(NSData *)data error:(NSError * _Nullable * _Nullable)error;
- (BOOL)resizeColumns:(uint16_t)cols rows:(uint16_t)rows error:(NSError * _Nullable * _Nullable)error;
- (BOOL)closeInteractiveShell:(NSError * _Nullable * _Nullable)error;

@end

NS_ASSUME_NONNULL_END
