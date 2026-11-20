#import "Snapshot.h"
#import <CoreVideo/CoreVideo.h>
#import <assert.h>

// Observe the same RGB bytes Rust receives, ignoring row padding and the alpha byte.
static NSMutableData *output;
SaveResult termshot_save(const char *root, const uint8_t *pixels, size_t length,
                         uint32_t width, uint32_t height, size_t stride) {
    assert(length >= stride * (height - 1) + width * 4);
    output = [NSMutableData data];
    for (size_t y = 0; y < height; ++y)
        for (size_t x = 0; x < width; ++x)
            [output appendBytes:pixels + y * stride + x * 4 length:3];
    return (SaveResult){NULL, 1};
}

int main(void) {
    @autoreleasepool {
        CVPixelBufferRef pixels = NULL;
        assert(CVPixelBufferCreate(NULL, 8, 8, kCVPixelFormatType_32BGRA,
            (__bridge CFDictionaryRef)@{(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{}},
            &pixels) == kCVReturnSuccess);
        CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        CVBufferSetAttachment(pixels, kCVImageBufferCGColorSpaceKey, space, kCVAttachmentMode_ShouldPropagate);
        CGColorSpaceRelease(space);
        assert(CVPixelBufferLockBaseAddress(pixels, 0) == kCVReturnSuccess);
        for (NSUInteger y = 0; y < 8; ++y) {
            uint8_t *row = (uint8_t *)CVPixelBufferGetBaseAddress(pixels) + y * CVPixelBufferGetBytesPerRow(pixels);
            for (NSUInteger x = 0; x < 8; ++x) {
                row[x*4] = x < 4 ? 0 : 255;
                row[x*4+1] = y < 4 ? 0 : 255;
                row[x*4+2] = 255;
                row[x*4+3] = 255;
            }
        }
        CVPixelBufferUnlockBaseAddress(pixels, 0);
        Snapshot *shot = [Snapshot new];
        shot.buffer = CFBridgingRelease(pixels);
        shot.frame = CGRectMake(-4, 10, 4, 4);
        shot.srgb = YES;
        CGRect crop = CGRectMake(-3, 11.5, 2, 2);
        CGPoint origin;
        assert(directOrigin(shot, crop, 2, &origin));
        assert(origin.x == 2 && origin.y == 1);
        NSURL *root = [NSURL fileURLWithPath:NSTemporaryDirectory()];
        assert(saveDirect(root, shot, origin, 4, 4).expires_ms);
        NSData *expected = output;
        assert(saveComposite(root, [@[shot] mutableCopy], crop, 2, 4, 4).expires_ms);
        assert([output isEqualToData:expected]);
        assert(!directOrigin(shot, CGRectOffset(crop, 0.25, 0), 2, &origin));
        shot.srgb = NO;
        assert(!directOrigin(shot, crop, 2, &origin));
    }
    return 0;
}
