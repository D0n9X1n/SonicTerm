#import <AppKit/AppKit.h>
#import <CoreText/CoreText.h>
#include <dlfcn.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static FILE *report;
static NSString *cairoPath;

/* Runtime loading keeps the probe executable independent of Homebrew linkage. */
static BOOL probeCairo(NSString *path) {
    void *handle = dlopen(path.fileSystemRepresentation, RTLD_NOW | RTLD_LOCAL);
    if (!handle) {
        fprintf(report, "CAIRO dlopen=FAIL error=%s\n", dlerror());
        return NO;
    }
#define LOAD(ret, name, args) \
    ret (*name) args = (ret (*) args)dlsym(handle, #name); \
    if (!name) { fprintf(report, "CAIRO symbol=FAIL name=%s\n", #name); dlclose(handle); return NO; }
    LOAD(const char *, cairo_version_string, (void));
    LOAD(void *, cairo_image_surface_create, (int, int, int));
    LOAD(void *, cairo_create, (void *));
    LOAD(void *, cairo_pattern_create_linear, (double, double, double, double));
    LOAD(void, cairo_pattern_add_color_stop_rgba, (void *, double, double, double, double, double));
    LOAD(void, cairo_set_source, (void *, void *));
    LOAD(void, cairo_paint, (void *));
    LOAD(void, cairo_surface_flush, (void *));
    LOAD(unsigned char *, cairo_image_surface_get_data, (void *));
    LOAD(int, cairo_image_surface_get_stride, (void *));
    LOAD(int, cairo_status, (void *));
    LOAD(int, cairo_surface_status, (void *));
    LOAD(int, cairo_pattern_status, (void *));
    LOAD(void, cairo_destroy, (void *));
    LOAD(void, cairo_surface_destroy, (void *));
    LOAD(void, cairo_pattern_destroy, (void *));
#undef LOAD
    /* CAIRO_FORMAT_ARGB32 = 0; native-endian words are always 0xAARRGGBB. */
    void *surface = cairo_image_surface_create(0, 32, 32);
    void *context = cairo_create(surface);
    void *gradient = cairo_pattern_create_linear(0, 0, 32, 0);
    cairo_pattern_add_color_stop_rgba(gradient, 0, 1, 0, 0, 1);
    cairo_pattern_add_color_stop_rgba(gradient, 1, 0, 0, 1, 1);
    cairo_set_source(context, gradient);
    cairo_paint(context);
    cairo_surface_flush(surface);
    int contextStatus = cairo_status(context);
    int surfaceStatus = cairo_surface_status(surface);
    int patternStatus = cairo_pattern_status(gradient);
    unsigned char *pixels = cairo_image_surface_get_data(surface);
    int stride = cairo_image_surface_get_stride(surface);
    uint32_t left = 0, right = 0;
    if (pixels && stride >= 128) {
        uint32_t *row = (uint32_t *)(pixels + 16 * stride);
        left = row[0];
        right = row[31];
    }
    BOOL ok = contextStatus == 0 && surfaceStatus == 0 && patternStatus == 0 &&
        left == 0xfffb0004 && right == 0xff0400fb;
    Dl_info provider = {0};
    dladdr((void *)cairo_version_string, &provider);
    fprintf(report, "CAIRO version=%s provider=%s image=32x32 stride=%d status=%d,%d,%d left=%08x right=%08x basic_gradient=%s COLR=NOT_TESTED\n",
            cairo_version_string(), provider.dli_fname ?: "unknown", stride,
            contextStatus, surfaceStatus, patternStatus, left, right, ok ? "PASS" : "FAIL");
    cairo_pattern_destroy(gradient);
    cairo_destroy(context);
    cairo_surface_destroy(surface);
    dlclose(handle);
    return ok;
}

@interface ProbeDelegate : NSObject <NSApplicationDelegate>
@end
@implementation ProbeDelegate
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    NSBundle *bundle = NSBundle.mainBundle;
    NSDictionary *info = bundle.infoDictionary;
    NSArray *specs = info[@"ProbeFonts"];
    NSArray *available = CFBridgingRelease(CTFontManagerCopyAvailablePostScriptNames());
    fprintf(report, "APP bundle=%s ATS=%s launch=applicationDidFinishLaunching pid=%d HOME=%s NO_COLOR=%s\n",
            bundle.bundlePath.UTF8String, [info[@"ATSApplicationFontsPath"] UTF8String],
            getpid(), getenv("HOME") ?: "unset", getenv("NO_COLOR") ?: "unset");
    NSUInteger passed = 0;
    for (NSDictionary *spec in specs) {
        NSString *name = spec[@"PostScriptName"];
        NSString *relative = [info[@"ProbeExpectedFontsPath"] stringByAppendingPathComponent:spec[@"File"]];
        NSString *expected = [[bundle.resourcePath stringByAppendingPathComponent:relative] stringByResolvingSymlinksInPath];
        CTFontRef font = CTFontCreateWithName((__bridge CFStringRef)name, 14, NULL);
        NSString *actualName = CFBridgingRelease(CTFontCopyPostScriptName(font));
        NSURL *url = CFBridgingRelease(CTFontCopyAttribute(font, kCTFontURLAttribute));
        NSString *actualPath = [url.path stringByResolvingSymlinksInPath];
        NSFont *appKitFont = [NSFont fontWithName:name size:14];
        BOOL listed = [available containsObject:name];
        BOOL ok = [actualName isEqualToString:name] && [actualPath isEqualToString:expected] &&
            [appKitFont.fontName isEqualToString:name] && listed;
        passed += ok;
        fprintf(report, "FONT requested=%s actual_ps=%s appkit_ps=%s available=%s url=%s exact_bundle_file=%s\n",
                name.UTF8String, actualName.UTF8String, appKitFont.fontName.UTF8String ?: "nil",
                listed ? "yes" : "no", actualPath.UTF8String ?: "nil", ok ? "PASS" : "FAIL");
        CFRelease(font);
    }
    BOOL cairoOK = !cairoPath || probeCairo(cairoPath);
    BOOL ok = specs.count == 4 && passed == 4 && cairoOK;
    fprintf(report, "RESULT fonts=%lu/%lu cairo=%s verdict=%s\n", (unsigned long)passed,
            (unsigned long)specs.count, cairoPath ? (cairoOK ? "PASS" : "FAIL") : "NOT_REQUESTED", ok ? "PASS" : "FAIL");
    fclose(report);
    exit(ok ? 0 : 1);
}
@end

int main(int argc, const char *argv[]) {
    /* The app is launched by LaunchServices, outside the driver's process group. */
    alarm(15);
    @autoreleasepool {
        if (argc == 3 && strcmp(argv[1], "--cairo-only") == 0) {
            report = stdout;
            setbuf(report, NULL);
            return probeCairo([NSString stringWithUTF8String:argv[2]]) ? 0 : 1;
        }
        if (argc < 2) return 64;
        report = fopen(argv[1], "w");
        if (!report) return 73;
        setbuf(report, NULL);
        fprintf(report, "START pid=%d\n", getpid());
        if (argc > 2) cairoPath = [NSString stringWithUTF8String:argv[2]];
        NSApplication *app = NSApplication.sharedApplication;
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
        ProbeDelegate *delegate = [ProbeDelegate new];
        app.delegate = delegate;
        [app run];
    }
    return 70;
}
