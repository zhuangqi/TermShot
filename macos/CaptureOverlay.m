#import "CaptureOverlay.h"
#import <Availability.h>
#import <QuartzCore/QuartzCore.h>

static const CGFloat guideMargin = 18;
static const CGFloat badgeMargin = 10;
static const CGFloat selectionGap = 10;

static NSVisualEffectView *hudMaterial(NSView *content, CGFloat radius) {
    NSVisualEffectView *material = [NSVisualEffectView new];
    material.material = NSVisualEffectMaterialHUDWindow;
    material.blendingMode = NSVisualEffectBlendingModeWithinWindow;
    material.state = NSVisualEffectStateActive;
    material.wantsLayer = YES;
    material.layer.cornerRadius = radius;
    material.layer.masksToBounds = YES;
    [material addSubview:content];
    content.translatesAutoresizingMaskIntoConstraints = NO;
    [NSLayoutConstraint activateConstraints:@[
        [content.leadingAnchor constraintEqualToAnchor:material.leadingAnchor],
        [content.trailingAnchor constraintEqualToAnchor:material.trailingAnchor],
        [content.topAnchor constraintEqualToAnchor:material.topAnchor],
        [content.bottomAnchor constraintEqualToAnchor:material.bottomAnchor]
    ]];
    return material;
}

static NSView *guideMaterial(NSView *content, CGFloat radius) {
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 260000
    if (@available(macOS 26.0, *)) {
        NSGlassEffectView *glass = [NSGlassEffectView new];
        glass.cornerRadius = radius;
        glass.contentView = content;
        return glass;
    }
#endif
    return hudMaterial(content, radius);
}

static CAShapeLayer *outlineLayer(NSRect frame, CGFloat scale, NSColor *color, CGFloat width) {
    CAShapeLayer *outline = [CAShapeLayer layer];
    outline.frame = frame;
    outline.contentsScale = scale;
    outline.fillColor = NULL;
    outline.strokeColor = color.CGColor;
    outline.lineWidth = width;
    return outline;
}

static NSRect placeSizeLabel(NSRect bounds, NSRect selection, NSPoint pointer, NSSize size) {
    NSRect fit = NSInsetRect(bounds, badgeMargin, badgeMargin);
    if (size.width > NSWidth(fit) || size.height > NSHeight(fit)) return NSZeroRect;

    CGFloat x = MAX(NSMinX(fit), MIN(pointer.x - size.width / 2, NSMaxX(fit) - size.width));
    CGFloat y = MAX(NSMinY(fit), MIN(pointer.y - size.height / 2, NSMaxY(fit) - size.height));
    CGFloat above = NSMaxY(selection) + selectionGap;
    CGFloat below = NSMinY(selection) - selectionGap - size.height;
    CGFloat right = NSMaxX(selection) + selectionGap;
    CGFloat left = NSMinX(selection) - selectionGap - size.width;
    BOOL up = pointer.y >= NSMidY(selection);
    BOOL towardRight = pointer.x >= NSMidX(selection);
    NSRect candidates[] = {
        NSMakeRect(x, up ? above : below, size.width, size.height),
        NSMakeRect(towardRight ? right : left, y, size.width, size.height),
        NSMakeRect(x, up ? below : above, size.width, size.height),
        NSMakeRect(towardRight ? left : right, y, size.width, size.height)
    };
    for (NSUInteger i = 0; i < sizeof(candidates) / sizeof(candidates[0]); ++i) {
        if (NSContainsRect(fit, candidates[i])) return candidates[i];
    }
    return NSZeroRect;
}

@interface CaptureOverlay ()
@property(nonatomic, strong) CAShapeLayer *shade;
@property(nonatomic, strong) CAShapeLayer *darkOutline;
@property(nonatomic, strong) CAShapeLayer *lightOutline;
@property(nonatomic, strong) NSView *guide;
@property(nonatomic, strong) NSButton *cancelButton;
@property(nonatomic, strong) NSView *sizeBadge;
@property(nonatomic, strong) NSTextField *sizeLabel;
@end

@implementation CaptureOverlay
- (instancetype)initWithFrame:(NSRect)frame preview:(id)preview scale:(CGFloat)scale {
    self = [super initWithFrame:frame];
    if (!self) return nil;

    self.wantsLayer = YES;
    self.layer.masksToBounds = YES;
    CALayer *desktop = [CALayer layer];
    desktop.frame = self.bounds;
    desktop.contents = preview;
    desktop.contentsGravity = kCAGravityResize;
    desktop.contentsScale = scale;
    [self.layer addSublayer:desktop];

    BOOL highContrast = NSWorkspace.sharedWorkspace.accessibilityDisplayShouldIncreaseContrast;
    self.shade = [CAShapeLayer layer];
    self.shade.frame = self.bounds;
    self.shade.fillRule = kCAFillRuleEvenOdd;
    self.shade.fillColor = [NSColor colorWithWhite:0 alpha:highContrast ? 0.52 : 0.34].CGColor;
    [self.layer addSublayer:self.shade];

    CGFloat stroke = (highContrast ? 2 : 1) / scale;
    self.darkOutline = outlineLayer(self.bounds, scale, NSColor.blackColor, stroke * 3);
    self.lightOutline = outlineLayer(self.bounds, scale, NSColor.whiteColor, stroke);
    [self.layer addSublayer:self.darkOutline];
    [self.layer addSublayer:self.lightOutline];
    [self showSelection:NSZeroRect sizeLabel:nil pointer:NSZeroPoint];
    return self;
}

- (void)resetCursorRects {
    [super resetCursorRects];
    [self addCursorRect:self.bounds cursor:NSCursor.crosshairCursor];
    if (self.guide && !self.guide.hidden) {
        NSRect button = [self.cancelButton convertRect:self.cancelButton.bounds toView:self];
        [self addCursorRect:button cursor:NSCursor.arrowCursor];
    }
}

- (NSView *)hitTest:(NSPoint)point {
    NSView *hit = [super hitTest:point];
    if (!hit) return nil;
    if (self.guide && !self.guide.hidden &&
        (hit == self.cancelButton || [hit isDescendantOf:self.cancelButton])) return hit;
    return self;
}

- (void)showGuideIn:(NSRect)visibleBounds cancelTarget:(id)target action:(SEL)action {
    NSTextField *instruction = [NSTextField labelWithString:@"拖动框选截图区域"];
    instruction.font = [NSFont systemFontOfSize:13 weight:NSFontWeightMedium];
    NSImage *symbol = [NSImage imageWithSystemSymbolName:@"viewfinder" accessibilityDescription:nil];
    NSImageView *icon = [NSImageView imageViewWithImage:symbol];
    icon.symbolConfiguration = [NSImageSymbolConfiguration configurationWithPointSize:15
                                                                             weight:NSFontWeightMedium];

    self.cancelButton = [NSButton buttonWithTitle:@"取消" target:target action:action];
    self.cancelButton.bezelStyle = NSBezelStyleRounded;
    self.cancelButton.controlSize = NSControlSizeSmall;
    self.cancelButton.toolTip = @"取消截图（Esc）";
    [self.cancelButton setAccessibilityLabel:@"取消截图"];

    NSStackView *content = [NSStackView stackViewWithViews:@[icon, instruction, self.cancelButton]];
    content.orientation = NSUserInterfaceLayoutOrientationHorizontal;
    content.alignment = NSLayoutAttributeCenterY;
    content.spacing = 10;
    content.edgeInsets = NSEdgeInsetsMake(8, 14, 8, 8);
    NSSize size = content.fittingSize;
    if (size.width > NSWidth(visibleBounds) || size.height + guideMargin > NSHeight(visibleBounds)) return;
    self.guide = guideMaterial(content, size.height / 2);
    self.guide.frame = NSMakeRect(round(NSMidX(visibleBounds) - size.width / 2),
                                  NSMaxY(visibleBounds) - guideMargin - size.height,
                                  size.width, size.height);
    [self addSubview:self.guide];
    [self.window invalidateCursorRectsForView:self];
}

- (void)showSelection:(NSRect)selection sizeLabel:(NSString *)sizeLabel pointer:(NSPoint)pointer {
    BOOL hasSelection = !NSIsEmptyRect(selection);
    NSRect opening = NSIntersectionRect(selection, self.bounds);
    CGMutablePathRef mask = CGPathCreateMutable();
    CGPathAddRect(mask, NULL, self.bounds);
    if (!NSIsEmptyRect(opening)) CGPathAddRect(mask, NULL, opening);
    // Clip the complete outline at the display edge so a cross-display selection has no seam.
    CGPathRef outline = NSIsEmptyRect(opening) ? NULL : CGPathCreateWithRect(selection, NULL);
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    self.shade.path = mask;
    self.darkOutline.path = outline;
    self.lightOutline.path = outline;
    [CATransaction commit];
    CGPathRelease(mask);
    if (outline) CGPathRelease(outline);

    if (self.guide && self.guide.hidden != hasSelection) {
        self.guide.hidden = hasSelection;
        [self.window invalidateCursorRectsForView:self];
    }
    if (!sizeLabel) {
        self.sizeBadge.hidden = YES;
        return;
    }
    if (!self.sizeBadge) {
        self.sizeLabel = [NSTextField labelWithString:@""];
        self.sizeLabel.font = [NSFont monospacedDigitSystemFontOfSize:12 weight:NSFontWeightMedium];
        self.sizeLabel.alignment = NSTextAlignmentCenter;
        NSStackView *content = [NSStackView stackViewWithViews:@[self.sizeLabel]];
        content.edgeInsets = NSEdgeInsetsMake(5, 10, 5, 10);
        self.sizeBadge = hudMaterial(content, 9);
        [self addSubview:self.sizeBadge];
    }
    if (![self.sizeLabel.stringValue isEqualToString:sizeLabel]) self.sizeLabel.stringValue = sizeLabel;
    NSSize size = self.sizeBadge.fittingSize;
    NSRect position = placeSizeLabel(self.bounds, selection, pointer,
                                    NSMakeSize(ceil(size.width), ceil(size.height)));
    self.sizeBadge.hidden = NSIsEmptyRect(position);
    if (!self.sizeBadge.hidden) self.sizeBadge.frame = position;
}
@end
