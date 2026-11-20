#pragma once
#import <AppKit/AppKit.h>

NS_ASSUME_NONNULL_BEGIN

@interface CaptureOverlay : NSView
- (instancetype)initWithFrame:(NSRect)frame preview:(id)preview scale:(CGFloat)scale;
- (void)showGuideIn:(NSRect)visibleBounds cancelTarget:(id)target action:(SEL)action;
- (void)showSelection:(NSRect)selection sizeLabel:(NSString * _Nullable)sizeLabel pointer:(NSPoint)pointer;
@end

NS_ASSUME_NONNULL_END
