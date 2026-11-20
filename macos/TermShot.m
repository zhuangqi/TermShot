#import <AppKit/AppKit.h>
#import <VideoToolbox/VideoToolbox.h>
#import <Carbon/Carbon.h>
#import <dispatch/dispatch.h>
#import <QuartzCore/QuartzCore.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>

#import "CaptureOverlay.h"
#import "Snapshot.h"

@class Controller;
@interface Overlay : CaptureOverlay
@property(nonatomic, weak) Controller *controller;
@property(nonatomic, strong) Snapshot *snapshot;
@end

@interface Panel : NSWindow
@end
@implementation Panel
- (BOOL)canBecomeKeyWindow { return YES; }
@end

@interface Controller : NSObject <NSApplicationDelegate>
@property(nonatomic, strong) NSStatusItem *status;
@property(nonatomic, strong) NSMutableArray<Panel *> *panels;
@property(nonatomic, strong) NSURL *root;
@property(nonatomic, strong) NSTimer *cleanup;
@property(nonatomic, strong) dispatch_queue_t worker;
@property(nonatomic) CGPoint anchor;
@property(nonatomic) CGRect selection;
@property(nonatomic) BOOL capturing;
@property(nonatomic) NSUInteger jobs;
@property(nonatomic) size_t reserved;
@property(nonatomic) BOOL quitting;
@property(nonatomic) EventHotKeyRef hotkey;
- (void)capture;
- (void)cancel;
- (void)drag:(CGPoint)point;
- (void)finish;
@end

@implementation Overlay
- (BOOL)acceptsFirstResponder { return YES; }
- (void)keyDown:(NSEvent *)event {
    if (event.keyCode == kVK_Escape) [self.controller cancel];
    else [super keyDown:event];
}
- (CGPoint)desktopPoint:(NSEvent *)event {
    return [self.window convertPointToScreen:event.locationInWindow];
}
- (void)mouseDown:(NSEvent *)event {
    self.controller.anchor = [self desktopPoint:event];
    [self.controller drag:self.controller.anchor];
}
- (void)mouseDragged:(NSEvent *)event { [self.controller drag:[self desktopPoint:event]]; }
- (void)mouseUp:(NSEvent *)event {
    [self.controller drag:[self desktopPoint:event]];
    [self.controller finish];
}
@end

static OSStatus hotkeyPressed(EventHandlerCallRef handler, EventRef event, void *context) {
    [(__bridge Controller *)context capture];
    return noErr;
}

static NSImage *statusIcon(void) {
    NSURL *url = [NSBundle.mainBundle URLForResource:@"TermShot" withExtension:@"icns"];
    NSImage *image = url ? [[NSImage alloc] initWithContentsOfURL:url] : nil;
    if (!image) {
        image = [NSImage imageWithSystemSymbolName:@"photo.on.rectangle" accessibilityDescription:@"TermShot"];
    }
    image.size = NSMakeSize(18, 18);
    image.template = YES;
    return image;
}

static void report(NSString *title, NSString *message) {
    NSAlert *alert = [NSAlert new];
    alert.alertStyle = NSAlertStyleWarning;
    alert.messageText = title;
    alert.informativeText = message;
    [NSApp activateIgnoringOtherApps:YES];
    [alert runModal];
}

static CGFloat snapshotScale(Snapshot *shot) {
    return CVPixelBufferGetWidth((__bridge CVPixelBufferRef)shot.buffer) / shot.frame.size.width;
}

@implementation Controller
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    self.worker = dispatch_queue_create("TermShot.files", DISPATCH_QUEUE_SERIAL);
    self.root = [[[NSFileManager defaultManager] URLsForDirectory:NSApplicationSupportDirectory
                                                        inDomains:NSUserDomainMask].firstObject URLByAppendingPathComponent:@"TermShot"];
    self.panels = [NSMutableArray array];
    self.status = [[NSStatusBar systemStatusBar] statusItemWithLength:NSSquareStatusItemLength];
    self.status.button.image = statusIcon();
    self.status.button.toolTip = @"TermShot";
    [self.status.button setAccessibilityLabel:@"TermShot"];
    NSMenu *menu = [NSMenu new];
    NSMenuItem *capture = [menu addItemWithTitle:@"截图" action:@selector(capture) keyEquivalent:@"s"];
    capture.target = self;
    capture.keyEquivalentModifierMask = NSEventModifierFlagOption | NSEventModifierFlagShift;
    capture.image = [NSImage imageWithSystemSymbolName:@"viewfinder" accessibilityDescription:nil];
    [menu addItem:NSMenuItem.separatorItem];
    NSMenuItem *quit = [menu addItemWithTitle:@"退出 TermShot" action:@selector(terminate:) keyEquivalent:@"q"];
    quit.target = NSApp;
    self.status.menu = menu;
    EventTypeSpec type = {kEventClassKeyboard, kEventHotKeyPressed};
    EventHotKeyID identifier = {'TShT', 1};
    EventHotKeyRef key = NULL;
    OSStatus status = InstallApplicationEventHandler(hotkeyPressed, 1, &type, (__bridge void *)self, NULL);
    if (status == noErr) status = RegisterEventHotKey(kVK_ANSI_S, optionKey | shiftKey, identifier,
                                                     GetApplicationEventTarget(), 0, &key);
    self.hotkey = key;
    if (status != noErr) report(@"截图快捷键不可用", @"你仍可从菜单栏启动截图。");
    [self scanCaptures];
}
- (void)armCleanup:(NSDate *)deadline {
    if (self.cleanup.valid && [self.cleanup.fireDate compare:deadline] != NSOrderedDescending) return;
    [self.cleanup invalidate];
    self.cleanup = [[NSTimer alloc] initWithFireDate:deadline interval:0 repeats:NO
        block:^(NSTimer *timer) { self.cleanup = nil; [self scanCaptures]; }];
    [[NSRunLoop mainRunLoop] addTimer:self.cleanup forMode:NSRunLoopCommonModes];
}
- (void)scanCaptures {
    dispatch_async(self.worker, ^{
        NSDate *start = [NSDate date];
        uint64_t delay = termshot_clean(self.root.fileSystemRepresentation);
        if (delay == UINT64_MAX) return;
        dispatch_async(dispatch_get_main_queue(), ^{
            [self armCleanup:[start dateByAddingTimeInterval:delay / 1000.0]];
        });
    });
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    if (!self.jobs) return NSTerminateNow;
    self.quitting = YES;
    return NSTerminateLater;
}
- (void)capture {
    if (self.capturing || self.quitting) return;
    if (!termshot_budget(self.jobs, self.reserved)) {
        report(@"截图仍在保存", @"请等待当前截图保存完成，再试一次。");
        return;
    }
    self.capturing = YES;
    self.selection = CGRectZero;
    if (!CGPreflightScreenCaptureAccess() && !CGRequestScreenCaptureAccess()) {
        self.capturing = NO;
        report(@"需要屏幕录制权限", @"请在「系统设置」的「隐私与安全性」中允许 TermShot 录制屏幕，然后重新打开应用。");
        return;
    }
    [SCShareableContent getShareableContentExcludingDesktopWindows:NO onScreenWindowsOnly:YES
        completionHandler:^(SCShareableContent *content, NSError *error) {
            dispatch_async(dispatch_get_main_queue(), ^{
                if (!content || error) {
                    self.capturing = NO;
                    report(@"无法读取屏幕", error.localizedDescription ?: @"请重试。");
                    return;
                }
                [self snapshotDisplays:content.displays];
            });
        }];
}
- (void)snapshotDisplays:(NSArray<SCDisplay *> *)displays {
    dispatch_group_t group = dispatch_group_create();
    __block NSString *failure = nil;
    for (NSScreen *screen in NSScreen.screens) {
        CGDirectDisplayID screenID = [screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
        SCDisplay *display = nil;
        for (SCDisplay *candidate in displays) {
            if (candidate.displayID == screenID) { display = candidate; break; }
        }
        if (!display) { failure = @"显示器配置已改变，请重试。"; break; }
        CGRect frame = screen.frame;
        CGFloat scale = screen.backingScaleFactor;
        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
        SCStreamConfiguration *config = [SCStreamConfiguration new];
        config.width = llround(frame.size.width * scale);
        config.height = llround(frame.size.height * scale);
        config.showsCursor = NO;
        config.pixelFormat = kCVPixelFormatType_32BGRA;
        config.colorSpaceName = kCGColorSpaceSRGB;
        dispatch_group_enter(group);
        [SCScreenshotManager captureSampleBufferWithFilter:filter configuration:config
            completionHandler:^(CMSampleBufferRef sample, NSError *error) {
                CVPixelBufferRef pixels = sample ? CMSampleBufferGetImageBuffer(sample) : NULL;
                CGImageRef preview = NULL;
                if (pixels) VTCreateCGImageFromCVPixelBuffer(pixels, NULL, &preview);
                id image = CFBridgingRelease(preview);
                Snapshot *shot = [Snapshot new];
                shot.frame = frame;
                shot.buffer = (__bridge id)pixels;
                CGColorSpaceRef colors = preview ? CGImageGetColorSpace(preview) : NULL;
                CFStringRef name = colors ? CGColorSpaceGetName(colors) : NULL;
                shot.srgb = name && CFEqual(name, kCGColorSpaceSRGB);
                dispatch_async(dispatch_get_main_queue(), ^{
                    if (image) [self addPanel:shot preview:image scale:scale];
                    else failure = error.localizedDescription ?: @"无法截取屏幕。";
                    dispatch_group_leave(group);
                });
            }];
    }
    dispatch_group_notify(group, dispatch_get_main_queue(), ^{
        if (failure || self.panels.count == 0) {
            [self cancel];
            report(@"无法启动截图", failure ?: @"没有可用的显示器。");
            return;
        }
        [NSApp activateIgnoringOtherApps:YES];
        for (Panel *panel in self.panels) [panel orderFrontRegardless];
        Panel *panel = [self panelAtPoint:NSEvent.mouseLocation] ?: self.panels.firstObject;
        [panel makeKeyWindow];
        NSRect visible = panel.screen ? [panel convertRectFromScreen:panel.screen.visibleFrame] : panel.contentView.bounds;
        [(Overlay *)panel.contentView showGuideIn:visible cancelTarget:self action:@selector(cancelFromGuide:)];
    });
}
- (Panel *)panelAtPoint:(NSPoint)point {
    for (Panel *panel in self.panels) {
        if (NSPointInRect(point, panel.frame)) return panel;
    }
    return nil;
}
- (void)addPanel:(Snapshot *)snapshot preview:(id)image scale:(CGFloat)scale {
    CGRect frame = snapshot.frame;
    Panel *panel = [[Panel alloc] initWithContentRect:frame styleMask:NSWindowStyleMaskBorderless
        backing:NSBackingStoreBuffered defer:NO];
    panel.releasedWhenClosed = NO;
    panel.level = NSScreenSaverWindowLevel;
    panel.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary;
    NSRect bounds = NSMakeRect(0, 0, NSWidth(frame), NSHeight(frame));
    Overlay *view = [[Overlay alloc] initWithFrame:bounds preview:image scale:scale];
    view.controller = self;
    view.snapshot = snapshot;
    panel.contentView = view;
    [panel makeFirstResponder:view];
    [self.panels addObject:panel];
}
- (void)drag:(CGPoint)point {
    CGRect crop = CGRectMake(MIN(point.x, self.anchor.x), MIN(point.y, self.anchor.y),
                             fabs(point.x - self.anchor.x), fabs(point.y - self.anchor.y));
    self.selection = crop;
    Panel *underPointer = [self panelAtPoint:point];
    NSString *sizeLabel = nil;
    if (underPointer && !CGRectIsEmpty(crop)) {
        CGFloat scale = 1;
        for (Panel *panel in self.panels) {
            Snapshot *shot = ((Overlay *)panel.contentView).snapshot;
            if (CGRectIntersectsRect(crop, shot.frame)) scale = MAX(scale, snapshotScale(shot));
        }
        sizeLabel = [NSString stringWithFormat:@"%llu × %llu 像素",
                     (unsigned long long)ceil(crop.size.width * scale),
                     (unsigned long long)ceil(crop.size.height * scale)];
    }
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    for (Panel *panel in self.panels) {
        Overlay *view = (Overlay *)panel.contentView;
        NSRect localCrop = [panel convertRectFromScreen:crop];
        [view showSelection:localCrop sizeLabel:panel == underPointer ? sizeLabel : nil
                    pointer:[panel convertPointFromScreen:point]];
    }
    [CATransaction commit];
}
- (void)closePanels {
    for (Panel *panel in self.panels) [panel orderOut:nil];
    [self.panels removeAllObjects];
}
- (void)cancel { [self closePanels]; self.capturing = NO; }
- (void)cancelFromGuide:(id)sender { [self cancel]; }
- (void)finish {
    CGRect crop = self.selection;
    if (CGRectIsEmpty(crop)) return;
    NSMutableArray<Snapshot *> *shots = [NSMutableArray array];
    CGFloat scale = 1;
    size_t retained = 0;
    for (Panel *panel in self.panels) {
        Snapshot *shot = ((Overlay *)panel.contentView).snapshot;
        if (!CGRectIntersectsRect(crop, shot.frame)) continue;
        [shots addObject:shot];
        CVPixelBufferRef buffer = (__bridge CVPixelBufferRef)shot.buffer;
        retained += CVPixelBufferGetDataSize(buffer);
        scale = MAX(scale, snapshotScale(shot));
    }
    if (!shots.count) return;
    size_t width = ceil(crop.size.width * scale), height = ceil(crop.size.height * scale);
    if (!width || !height || width > UINT32_MAX || height > UINT32_MAX ||
        width > (SIZE_MAX - retained) / 4 / height) {
        report(@"选区过大", @"请缩小截图区域后重试。");
        return;
    }
    CGPoint origin = CGPointZero;
    BOOL direct = shots.count == 1 && directOrigin(shots.firstObject, crop, scale, &origin);
    size_t cost = retained + (direct ? 0 : width * height * 4);
    if (cost > termshot_budget(self.jobs, self.reserved)) {
        report(@"截图保存队列已满", @"等待上一张截图保存完成后，可以重新框选。");
        return;
    }
    self.jobs++;
    self.reserved += cost;
    [self closePanels];
    self.capturing = NO;
    dispatch_async(self.worker, ^{
        SaveResult result = {0};
        if (direct) {
            result = saveDirect(self.root, shots.firstObject, origin, width, height);
        } else {
            result = saveComposite(self.root, shots, crop, scale, width, height);
        }
        [shots removeAllObjects];
        NSString *value = result.text ? [NSString stringWithUTF8String:result.text] : @"无法读取截图像素。";
        if (result.text) termshot_free(result.text);
        uint64_t expires = result.expires_ms;
        dispatch_async(dispatch_get_main_queue(), ^{
            self.reserved -= cost;
            self.jobs--;
            if (expires) {
                [self armCleanup:[NSDate dateWithTimeIntervalSince1970:expires / 1000.0]];
                [NSPasteboard.generalPasteboard clearContents];
                if (![NSPasteboard.generalPasteboard setString:value forType:NSPasteboardTypeString])
                    report(@"截图已保存", [@"无法复制路径到剪贴板。\n" stringByAppendingString:value]);
            } else report(@"无法保存截图", value);
            if (self.quitting && !self.jobs) [NSApp replyToApplicationShouldTerminate:YES];
        });
    });
}
@end

void termshot_app(void) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
        Controller * __attribute__((objc_precise_lifetime)) controller = [Controller new];
        app.delegate = controller;
        [app run];
    }
}
