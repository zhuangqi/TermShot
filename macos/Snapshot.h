#pragma once
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>

typedef struct { char *text; uint64_t expires_ms; } SaveResult;
extern SaveResult termshot_save(const char *, const uint8_t *, size_t, uint32_t, uint32_t, size_t);
extern size_t termshot_budget(size_t, size_t);
extern void termshot_free(char *);
extern uint64_t termshot_clean(const char *);

@interface Snapshot : NSObject
@property(nonatomic) CGRect frame;
@property(nonatomic, strong) id buffer;
@property(nonatomic) BOOL srgb;
@end

BOOL directOrigin(Snapshot *shot, CGRect crop, CGFloat scale, CGPoint *origin);
SaveResult saveDirect(NSURL *root, Snapshot *shot, CGPoint origin, size_t width, size_t height);
SaveResult saveComposite(NSURL *root, NSMutableArray<Snapshot *> *shots, CGRect crop,
                         CGFloat scale, size_t width, size_t height);
