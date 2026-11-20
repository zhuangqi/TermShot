#import "Snapshot.h"
#import <VideoToolbox/VideoToolbox.h>

@implementation Snapshot
@end

// AppKit uses bottom-left desktop points. Pixel buffers start at the top-left.
BOOL directOrigin(Snapshot *shot, CGRect crop, CGFloat scale, CGPoint *origin) {
    CVPixelBufferRef pixels = (__bridge CVPixelBufferRef)shot.buffer;
    if (!shot.srgb || !CGRectContainsRect(shot.frame, crop) ||
        CVPixelBufferGetPixelFormatType(pixels) != kCVPixelFormatType_32BGRA ||
        CVPixelBufferGetHeight(pixels) != shot.frame.size.height * scale ||
        CVPixelBufferGetWidth(pixels) != shot.frame.size.width * scale) return NO;
    CGFloat x = (CGRectGetMinX(crop) - CGRectGetMinX(shot.frame)) * scale;
    CGFloat y = (CGRectGetMaxY(shot.frame) - CGRectGetMaxY(crop)) * scale;
    if (x != floor(x) || y != floor(y) || crop.size.width * scale != floor(crop.size.width * scale) ||
        crop.size.height * scale != floor(crop.size.height * scale)) return NO;
    *origin = CGPointMake(x, y);
    return YES;
}

SaveResult saveComposite(NSURL *root, NSMutableArray<Snapshot *> *shots, CGRect crop,
                                CGFloat scale, size_t width, size_t height) {
    SaveResult result = {0};
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef ctx = CGBitmapContextCreate(NULL, width, height, 8, width * 4, space,
        kCGImageAlphaNoneSkipFirst | kCGBitmapByteOrder32Little);
    CGColorSpaceRelease(space);
    if (!ctx) return result;
    CGContextScaleCTM(ctx, scale, scale);
    CGContextTranslateCTM(ctx, -crop.origin.x, -crop.origin.y);
    for (Snapshot *shot in shots) {
        CGImageRef image = NULL;
        if (VTCreateCGImageFromCVPixelBuffer((__bridge CVPixelBufferRef)shot.buffer, NULL, &image) ||
            !image) { CGContextRelease(ctx); return result; }
        CGContextDrawImage(ctx, shot.frame, image);
        CGImageRelease(image);
    }
    [shots removeAllObjects];
    result = termshot_save(root.fileSystemRepresentation, CGBitmapContextGetData(ctx),
                           width * height * 4, width, height, width * 4);
    CGContextRelease(ctx);
    return result;
}

SaveResult saveDirect(NSURL *root, Snapshot *shot, CGPoint origin, size_t width, size_t height) {
    SaveResult result = {0};
    CVPixelBufferRef pixels = (__bridge CVPixelBufferRef)shot.buffer;
    if (CVPixelBufferLockBaseAddress(pixels, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess) {
        const uint8_t *base = CVPixelBufferGetBaseAddress(pixels);
        size_t stride = CVPixelBufferGetBytesPerRow(pixels);
        size_t offset = (size_t)origin.y * stride + (size_t)origin.x * 4;
        size_t length = CVPixelBufferGetDataSize(pixels);
        if (base && offset < length) {
            result = termshot_save(root.fileSystemRepresentation, base + offset,
                                   length - offset, width, height, stride);
        }
        CVPixelBufferUnlockBaseAddress(pixels, kCVPixelBufferLock_ReadOnly);
    }
    return result;
}
