// Runs the Linux viewer's video plane drawing (PlaneGl in
// ui/linux/runner/desktop_video_gl.cc, compiled unchanged through the epoxy
// stand-in in macos-gl/) on this Mac's OpenGL 4.1 core profile, the desktop
// GL path a GDK context takes, against the CPU references in
// ui/linux/test/plane_reference.h, and measures what it can here: the
// filters' output against a Lanczos-3 reference, the sharpness they keep,
// and the GPU time of their passes through the plane's own timer queries.
// Built and run by run.sh. Apple's GL runs on Metal: its GPU times are this
// Mac's, not a Linux driver's.
#define GL_SILENCE_DEPRECATION
#include <ApplicationServices/ApplicationServices.h>
#include <CoreText/CoreText.h>
#include <ImageIO/ImageIO.h>
#include <OpenGL/OpenGL.h>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

#include "desktop_video_gl.h"
#include "plane_reference.h"

namespace {

using plane_reference::Filter;
using plane_reference::Image;

int g_failed = 0;

void Check(bool ok, const std::string& what) {
  printf("  %s %s\n", ok ? "ok  " : "FAIL", what.c_str());
  if (!ok) ++g_failed;
}

// ---------------------------------------------------------------------------
// Test pictures: text as a desktop shows it, drawn with Core Text at one
// point a pixel, regular and monospaced from 9 to 14 pixels, dark on light
// and light on dark, with one- and two-pixel rules, a coloured line and a
// grey ramp.

const char* const kLines[] = {
    "The quick brown fox jumps over the lazy dog; 0123456789 (){}[] <=> +-*/",
    "fn plan(width: u32, height: u32) -> Option<Rect> { width.checked_mul(3)? }",
    "Settings  >  Display  >  Scale 125%   |   File  Edit  View  Window  Help",
    "error[E0308]: mismatched types --> src/main.rs:42:17 expected `u64`",
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod",
    "iIlL1| oO0 rnm wWvV ,.;:'\" -- __ == ~~ ## @@ %% && ^^ !! ?? $$ ** ++",
};

void DrawText(CGContextRef context, const char* text, const char* font_name, double size, double x, double y,
              CGFloat r, CGFloat g, CGFloat b) {
  CFStringRef name = CFStringCreateWithCString(nullptr, font_name, kCFStringEncodingUTF8);
  CTFontRef font = CTFontCreateWithName(name, size, nullptr);
  CGColorRef color = CGColorCreateSRGB(r, g, b, 1);
  const void* keys[] = {kCTFontAttributeName, kCTForegroundColorAttributeName};
  const void* values[] = {font, color};
  CFDictionaryRef attributes =
      CFDictionaryCreate(nullptr, keys, values, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
  CFStringRef string = CFStringCreateWithCString(nullptr, text, kCFStringEncodingUTF8);
  CFAttributedStringRef attributed = CFAttributedStringCreate(nullptr, string, attributes);
  CTLineRef line = CTLineCreateWithAttributedString(attributed);
  CGContextSetTextPosition(context, x, y);
  CTLineDraw(line, context);
  CFRelease(line);
  CFRelease(attributed);
  CFRelease(string);
  CFRelease(attributes);
  CGColorRelease(color);
  CFRelease(font);
  CFRelease(name);
}

// A `width` x `height` desktop of text; the same layout at every size, in
// pixels, so a bigger picture holds more of it.
Image TextPicture(int width, int height) {
  Image image(width, height);
  CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGContextRef context = CGBitmapContextCreate(image.rgba.data(), static_cast<size_t>(width),
                                               static_cast<size_t>(height), 8, static_cast<size_t>(width) * 4, space,
                                               kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
  CGContextSetRGBFillColor(context, 0.97, 0.97, 0.96, 1);
  CGContextFillRect(context, CGRectMake(0, 0, width, height));
  // The right third dark, as an editor beside a document.
  CGContextSetRGBFillColor(context, 0.12, 0.13, 0.15, 1);
  CGContextFillRect(context, CGRectMake(width * 2 / 3, 0, width - width * 2 / 3, height));
  const double sizes[] = {9, 10, 11, 12, 13, 14};
  int line = 0;
  // Core Graphics counts rows from the bottom; the buffer's first row is
  // the picture's top.
  for (double y = height - 20; y > 24; y -= 19, ++line) {
    const double size = sizes[line % 6];
    const char* text = kLines[line % 6];
    const bool mono = (line / 6) % 2 == 1;
    const char* font = mono ? "Menlo" : "Helvetica";
    for (double x = 8; x < width; x += 640) {
      const bool dark = x >= width * 2 / 3;
      const bool red = line % 11 == 5;
      DrawText(context, text, font, size, x, y, dark ? 0.86 : (red ? 0.8 : 0.1), dark ? 0.87 : (red ? 0.1 : 0.1),
               dark ? 0.88 : (red ? 0.1 : 0.12));
    }
    if (line % 9 == 4) {
      CGContextSetRGBFillColor(context, 0.2, 0.2, 0.25, 1);
      CGContextFillRect(context, CGRectMake(8, y - 5, width - 16, line % 2 ? 1 : 2));
    }
  }
  // A grey ramp along the bottom.
  for (int x = 0; x < width; ++x) {
    const CGFloat v = static_cast<CGFloat>(x) / (width - 1);
    CGContextSetRGBFillColor(context, v, v, v, 1);
    CGContextFillRect(context, CGRectMake(x, 0, 1, 16));
  }
  CGContextRelease(context);
  CGColorSpaceRelease(space);
  for (size_t i = 3; i < image.rgba.size(); i += 4) image.rgba[i] = 255;
  return image;
}

bool WritePng(const Image& image, const std::string& path) {
  CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGContextRef context =
      CGBitmapContextCreate(const_cast<uint8_t*>(image.rgba.data()), static_cast<size_t>(image.width),
                            static_cast<size_t>(image.height), 8, static_cast<size_t>(image.width) * 4, space,
                            kCGImageAlphaNoneSkipLast | kCGBitmapByteOrder32Big);
  CGImageRef cg = CGBitmapContextCreateImage(context);
  CFStringRef file = CFStringCreateWithCString(nullptr, path.c_str(), kCFStringEncodingUTF8);
  CFURLRef url = CFURLCreateWithFileSystemPath(nullptr, file, kCFURLPOSIXPathStyle, false);
  CGImageDestinationRef destination = CGImageDestinationCreateWithURL(url, CFSTR("public.png"), 1, nullptr);
  bool ok = false;
  if (destination) {
    CGImageDestinationAddImage(destination, cg, nullptr);
    ok = CGImageDestinationFinalize(destination);
    CFRelease(destination);
  }
  CFRelease(url);
  CFRelease(file);
  CGImageRelease(cg);
  CGContextRelease(context);
  CGColorSpaceRelease(space);
  return ok;
}

// ---------------------------------------------------------------------------
// The decoder's picture as a host sends it: BT.709, video range, chroma the
// mean of each 2x2, in system memory, through the runner's converter.

struct FakePicture {
  uint32_t width = 0, height = 0;
  std::vector<uint8_t> luma, chroma;
};

int32_t Describe(const PbVideoPicture* picture, PbVideoPlanes* out) {
  const auto* fake = reinterpret_cast<const FakePicture*>(picture);
  *out = PbVideoPlanes{};
  out->width = fake->width;
  out->height = fake->height;
  out->format = PB_VIDEO_FORMAT_NV12;
  out->plane_count = 2;
  out->kr = 0.2126f;
  out->kb = 0.0722f;
  out->planes[0].data = fake->luma.data();
  out->planes[0].fd = -1;
  out->planes[0].stride = fake->width;
  out->planes[0].width = fake->width;
  out->planes[0].height = fake->height;
  out->planes[1].data = fake->chroma.data();
  out->planes[1].fd = -1;
  out->planes[1].stride = (fake->width + 1) / 2 * 2;
  out->planes[1].width = (fake->width + 1) / 2;
  out->planes[1].height = (fake->height + 1) / 2;
  return 0;
}
PbVideoPicture* Retain(PbVideoPicture* picture) { return picture; }
void Release(PbVideoPicture*) {}

FakePicture Nv12(const Image& image) {
  FakePicture picture;
  picture.width = static_cast<uint32_t>(image.width);
  picture.height = static_cast<uint32_t>(image.height);
  const int cw = (image.width + 1) / 2, ch = (image.height + 1) / 2;
  picture.luma.resize(static_cast<size_t>(image.width) * image.height);
  picture.chroma.resize(static_cast<size_t>(cw) * ch * 2);
  const double kr = 0.2126, kb = 0.0722, kg = 1 - kr - kb;
  for (int y = 0; y < image.height; ++y) {
    for (int x = 0; x < image.width; ++x) {
      const double l = kr * image.At(x, y, 0) + kg * image.At(x, y, 1) + kb * image.At(x, y, 2);
      picture.luma[static_cast<size_t>(y) * image.width + x] = static_cast<uint8_t>(std::lround(16 + 219 * l));
    }
  }
  for (int y = 0; y < ch; ++y) {
    for (int x = 0; x < cw; ++x) {
      double cb = 0, cr = 0;
      for (int j = 0; j < 2; ++j) {
        for (int i = 0; i < 2; ++i) {
          const int sx = 2 * x + i, sy = 2 * y + j;
          const double r = image.At(sx, sy, 0), g = image.At(sx, sy, 1), b = image.At(sx, sy, 2);
          const double l = kr * r + kg * g + kb * b;
          cb += (b - l) / (2 * (1 - kb)) / 4;
          cr += (r - l) / (2 * (1 - kr)) / 4;
        }
      }
      picture.chroma[(static_cast<size_t>(y) * cw + x) * 2] = static_cast<uint8_t>(std::lround(128 + 224 * cb));
      picture.chroma[(static_cast<size_t>(y) * cw + x) * 2 + 1] = static_cast<uint8_t>(std::lround(128 + 224 * cr));
    }
  }
  return picture;
}

// A texture's pixels, rows top first as the picture's.
Image ReadTexture(GLuint name, int width, int height) {
  Image image(width, height);
  GLuint framebuffer = 0;
  glGenFramebuffers(1, &framebuffer);
  glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
  glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, name, 0);
  glPixelStorei(GL_PACK_ALIGNMENT, 1);
  glReadPixels(0, 0, width, height, GL_RGBA, GL_UNSIGNED_BYTE, image.rgba.data());
  glBindFramebuffer(GL_FRAMEBUFFER, 0);
  glDeleteFramebuffers(1, &framebuffer);
  return image;
}

// A framebuffer the plane draws into, as GTK's for the area.
struct Target {
  GLuint texture = 0, framebuffer = 0;
  int width = 0, height = 0;
  Target(int w, int h) : width(w), height(h) {
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
  }
  ~Target() {
    glDeleteFramebuffers(1, &framebuffer);
    glDeleteTextures(1, &texture);
  }
  // Its pixels, rows top first: GL's rows count from the bottom.
  Image Read() const {
    Image bottom_up(width, height);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glReadPixels(0, 0, width, height, GL_RGBA, GL_UNSIGNED_BYTE, bottom_up.rgba.data());
    Image image(width, height);
    const size_t row = static_cast<size_t>(width) * 4;
    for (int y = 0; y < height; ++y) {
      std::copy(bottom_up.rgba.begin() + static_cast<ptrdiff_t>((height - 1 - y) * row),
                bottom_up.rgba.begin() + static_cast<ptrdiff_t>((height - y) * row),
                image.rgba.begin() + static_cast<ptrdiff_t>(y * row));
    }
    return image;
  }
};

pb_video::PlaneRect Rect(int x, int y, int width, int height) {
  pb_video::PlaneRect rect;
  rect.x = x;
  rect.y = y;
  rect.width = width;
  rect.height = height;
  return rect;
}

const char* Name(pb_video::Enhance level) {
  switch (level) {
    case pb_video::Enhance::kOff:
      return "off";
    case pb_video::Enhance::kLow:
      return "low";
    case pb_video::Enhance::kMedium:
      return "medium";
    case pb_video::Enhance::kHigh:
      return "high";
  }
  return "?";
}

// A converted picture in the plane's context.
struct Converted {
  FakePicture source;
  pb_video::VideoGl gl;
  GLuint name = 0;
  uint32_t width = 0, height = 0;
  Image rgba;
  explicit Converted(const PbVideoModule* module, const Image& picture)
      : source(Nv12(picture)), gl(module, false, pb_video::Owner::kOwnGlx) {}
  bool Convert() {
    std::string failure;
    if (!gl.Convert(reinterpret_cast<PbVideoPicture*>(&source), &failure) ||
        !gl.Texture(&name, &width, &height, &failure)) {
      printf("  conversion failed: %s\n", failure.c_str());
      return false;
    }
    rgba = ReadTexture(name, static_cast<int>(width), static_cast<int>(height));
    return true;
  }
};

// One draw of `picture` and the visible part of what it drew.
struct Drawn {
  pb_video::PlanePlan plan;
  pb_video::PlaneFilter filter = pb_video::PlaneFilter::kNone;
  Image whole, visible;
  std::string fallback;
  size_t outside = 0;  // pixels outside the visible part that are not black
};

Drawn Draw(pb_video::PlaneGl* plane, const Converted& picture, const Target& target, const pb_video::PlaneRect& placed,
           const pb_video::PlaneRect& clip, pb_video::Enhance enhance, bool grey, uint64_t serial) {
  Drawn drawn;
  pb_video::PlaneFrame frame;
  frame.texture = picture.name;
  frame.width = picture.width;
  frame.height = picture.height;
  frame.serial = serial;
  frame.picture = placed;
  frame.clip = clip;
  frame.grey = grey;
  frame.enhance = enhance;
  frame.target_width = target.width;
  frame.target_height = target.height;
  glBindFramebuffer(GL_FRAMEBUFFER, target.framebuffer);
  plane->Draw(frame, &drawn.fallback);
  drawn.filter = plane->last_filter();
  drawn.plan = pb_video::PlanPlane(picture.width, picture.height, placed, clip, enhance, true, target.width,
                                   target.height);
  drawn.whole = target.Read();
  const pb_video::PlaneRect& v = drawn.plan.visible;
  if (v.width > 0 && v.height > 0) drawn.visible = drawn.whole.Crop(v.x, v.y, v.width, v.height);
  for (int y = 0; y < target.height; ++y) {
    for (int x = 0; x < target.width; ++x) {
      if (x >= v.x && x < v.x + v.width && y >= v.y && y < v.y + v.height) continue;
      const uint8_t* p = &drawn.whole.rgba[(static_cast<size_t>(y) * target.width + x) * 4];
      if (p[0] > 2 || p[1] > 2 || p[2] > 2) ++drawn.outside;
    }
  }
  return drawn;
}

Filter ReferenceOf(pb_video::PlaneFilter filter) {
  switch (filter) {
    case pb_video::PlaneFilter::kArea:
      return Filter::kArea;
    case pb_video::PlaneFilter::kAreaMipmapped:
      return Filter::kBox;
    case pb_video::PlaneFilter::kSharpen:
      return Filter::kSharpen;
    case pb_video::PlaneFilter::kUpscaleSharpen:
      return Filter::kUpscaleSharpen;
    default:
      return Filter::kBilinear;
  }
}

Image ExpectedOf(const Converted& picture, const Drawn& drawn, const pb_video::PlaneRect& placed,
                 pb_video::Enhance enhance, bool grey) {
  const pb_video::PlaneRect& v = drawn.plan.visible;
  return plane_reference::Expected(picture.rgba, ReferenceOf(drawn.plan.filter), placed.width, placed.height,
                                   v.x - placed.x, v.y - placed.y, v.width, v.height, pb_video::EnhancePeak(enhance),
                                   grey);
}

GLenum TakeErrors() {
  GLenum first = GL_NO_ERROR;
  for (int i = 0; i < 16; ++i) {
    const GLenum error = glGetError();
    if (error == GL_NO_ERROR) break;
    if (first == GL_NO_ERROR) first = error;
  }
  return first;
}

// ---------------------------------------------------------------------------

void Checks(const PbVideoModule* module, const std::string& crops) {
  printf("checks: the plane's filters against the CPU references, a 1280x720 text picture\n");
  Converted picture(module, TextPicture(1280, 720));
  Check(picture.Convert(), "the runner's converter converts the NV12 picture in desktop GL");
  const int w = static_cast<int>(picture.width), h = static_cast<int>(picture.height);
  Target target(w + 48, h + 40);
  pb_video::PlaneGl plane;
  std::string failure;
  Check(plane.Setup(&failure), "the plane builds its bilinear program in desktop GL (GLSL 1.50) " + failure);
  const pb_video::PlaneRect everything = Rect(0, 0, target.width, target.height);
  uint64_t serial = 1;

  struct Case {
    const char* what;
    pb_video::PlaneRect placed, clip;
    pb_video::Enhance enhance;
    bool grey;
    pb_video::PlaneFilter filter;
    double min_psnr;
  };
  const Case cases[] = {
      {"1:1, cut to a clip", Rect(16, 24, w, h), Rect(80, 72, w - 128, h - 96), pb_video::Enhance::kOff, false,
       pb_video::PlaneFilter::kBilinear, 50},
      {"0.5x, area", Rect(16, 24, w / 2, h / 2), everything, pb_video::Enhance::kOff, false,
       pb_video::PlaneFilter::kArea, 40},
      {"0.75x with an enhancement, area and no sharpening", Rect(5, 7, w * 3 / 4, h * 3 / 4), everything,
       pb_video::Enhance::kHigh, false, pb_video::PlaneFilter::kArea, 40},
      {"0.375x, area", Rect(16, 24, w * 3 / 8, h * 3 / 8), everything, pb_video::Enhance::kOff, false,
       pb_video::PlaneFilter::kArea, 40},
      {"0.25x, mipmapped area (against the exact area average)", Rect(16, 24, w / 4, h / 4), everything,
       pb_video::Enhance::kOff, false, pb_video::PlaneFilter::kAreaMipmapped, 30},
      {"1.5x, bilinear with no enhancement", Rect(-100, -50, w * 3 / 2, h * 3 / 2), everything,
       pb_video::Enhance::kOff, false, pb_video::PlaneFilter::kBilinear, 45},
      {"1:1 sharpened (medium)", Rect(16, 24, w, h), everything, pb_video::Enhance::kMedium, false,
       pb_video::PlaneFilter::kSharpen, 40},
      {"2x zoomed past the window, enlarged and sharpened (high), the picture's corner in view",
       Rect(-(w * 2 - (w + 48) + 40), -(h * 2 - (h + 40) + 30), w * 2, h * 2), everything, pb_video::Enhance::kHigh,
       false, pb_video::PlaneFilter::kUpscaleSharpen, 38},
      {"1.5x enlarged and sharpened (low), cut to a clip", Rect(-300, -200, w * 3 / 2, h * 3 / 2),
       Rect(40, 30, w - 100, h - 120), pb_video::Enhance::kLow, false, pb_video::PlaneFilter::kUpscaleSharpen, 38},
      {"1.25x enlarged and sharpened (medium), grey", Rect(-40, -60, w * 5 / 4, h * 5 / 4), everything,
       pb_video::Enhance::kMedium, true, pb_video::PlaneFilter::kUpscaleSharpen, 38},
  };
  for (const Case& c : cases) {
    TakeErrors();
    const Drawn drawn = Draw(&plane, picture, target, c.placed, c.clip, c.enhance, c.grey, serial);
    const GLenum error = TakeErrors();
    const Image expected = ExpectedOf(picture, drawn, c.placed, c.enhance, c.grey);
    const double psnr = plane_reference::Psnr(drawn.visible, expected);
    const int most = plane_reference::MaxDifference(drawn.visible, expected);
    printf("  %s: %s, %dx%d visible, PSNR %.1f dB, largest difference %d, %zu pixels lit outside\n", c.what,
           pb_video::PlaneFilterName(drawn.filter), drawn.plan.visible.width, drawn.plan.visible.height, psnr, most,
           drawn.outside);
    Check(drawn.filter == c.filter && drawn.plan.filter == c.filter && drawn.fallback.empty(),
          std::string("it draws with ") + pb_video::PlaneFilterName(c.filter));
    Check(psnr >= c.min_psnr && drawn.outside == 0 && error == GL_NO_ERROR,
          "it matches the reference, black around, no GL error");
  }

  // The mipmapped area filter against bilinear, both against the exact
  // area average at 0.25x: what the filter is for.
  {
    const pb_video::PlaneRect placed = Rect(16, 24, w / 4, h / 4);
    const Drawn drawn = Draw(&plane, picture, target, placed, everything, pb_video::Enhance::kOff, false, serial);
    const Image box =
        plane_reference::Box(picture.rgba, placed.width, placed.height, 0, 0, placed.width, placed.height);
    const Image bilinear =
        plane_reference::Stretch(picture.rgba, placed.width, placed.height, 0, 0, placed.width, placed.height);
    const double filtered = plane_reference::Psnr(drawn.visible, box), plain = plane_reference::Psnr(bilinear, box);
    printf("  0.25x against the area average: %.1f dB mipmapped area, %.1f dB bilinear\n", filtered, plain);
    Check(filtered >= plain + 6, "the mipmapped area filter is far closer to the area average than bilinear");
    // The converter's own reads take its base level again.
    GLint filter = 0;
    glBindTexture(GL_TEXTURE_2D, picture.name);
    glGetTexParameteriv(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, &filter);
    Check(filter == GL_LINEAR, "the picture's minifying filter is linear again after the draw");
    std::vector<uint8_t> still;
    uint32_t still_width = 0, still_height = 0;
    picture.gl.ReadBack(320, &still, &still_width, &still_height);
    Image read(static_cast<int>(still_width), static_cast<int>(still_height));
    read.rgba = still;
    // ReadBack's own 16-tap reduction, flipped: its rows come bottom first.
    Image upright(read.width, read.height);
    for (int y = 0; y < read.height; ++y) {
      std::copy(read.rgba.begin() + static_cast<ptrdiff_t>(y) * read.width * 4,
                read.rgba.begin() + static_cast<ptrdiff_t>(y + 1) * read.width * 4,
                upright.rgba.begin() + static_cast<ptrdiff_t>(read.height - 1 - y) * read.width * 4);
    }
    const Image reference = plane_reference::Box(picture.rgba, read.width, read.height, 0, 0, read.width, read.height);
    const double a = plane_reference::Psnr(read, reference), b = plane_reference::Psnr(upright, reference);
    printf("  the converter's still after the draw: %.1f dB against the area average\n", std::max(a, b));
    Check(std::max(a, b) >= 30, "the converter's still is read from the picture, not from stale mipmaps");
  }

  // The timer queries: on desktop GL 3.3 or later the plane times its
  // passes while asked.
  {
    plane.Time(true);
    for (int i = 0; i < 20; ++i) {
      Draw(&plane, picture, target, Rect(16, 24, w / 2, h / 2), everything, pb_video::Enhance::kOff, false, serial);
    }
    glFinish();
    Draw(&plane, picture, target, Rect(16, 24, w / 2, h / 2), everything, pb_video::Enhance::kOff, false, serial);
    const pb_video::PlaneGpuTimes times = plane.GpuTimes();
    printf("  timed: %zu draws, p50 %.3f ms\n", times.samples, times.p50_ms);
    Check(times.samples >= 19, "the plane times its passes on desktop GL while asked");
    plane.Time(false);
    Check(plane.GpuTimes().samples == 0, "and drops the figures when no longer asked");
  }

  // The enlargement's texture goes with the enhancement.
  {
    Draw(&plane, picture, target, Rect(-10, -10, w * 2, h * 2), everything, pb_video::Enhance::kLow, false, serial);
    Draw(&plane, picture, target, Rect(-10, -10, w * 2, h * 2), everything, pb_video::Enhance::kOff, false, serial);
    Check(TakeErrors() == GL_NO_ERROR, "turning the enhancement off frees its texture with no GL error");
  }

  if (!crops.empty()) {
    // Side by side crops of the same text, 2x enlargement and 0.5x.
    const pb_video::PlaneRect zoom = Rect(-200, -120, w * 2, h * 2);
    for (const pb_video::Enhance level :
         {pb_video::Enhance::kOff, pb_video::Enhance::kLow, pb_video::Enhance::kMedium, pb_video::Enhance::kHigh}) {
      const Drawn drawn = Draw(&plane, picture, target, zoom, everything, level, false, serial);
      WritePng(drawn.whole.Crop(0, 0, 480, 270), crops + "/2x-" + Name(level) + ".png");
    }
    const pb_video::PlaneRect half = Rect(0, 0, w / 2, h / 2);
    const Drawn area = Draw(&plane, picture, target, half, everything, pb_video::Enhance::kOff, false, serial);
    WritePng(area.whole.Crop(0, 0, 320, 180), crops + "/0.5x-area.png");
    WritePng(plane_reference::Stretch(picture.rgba, w / 2, h / 2, 0, 0, 320, 180), crops + "/0.5x-bilinear.png");
    WritePng(plane_reference::Lanczos(picture.rgba, w / 2, h / 2).Crop(0, 0, 320, 180), crops + "/0.5x-lanczos.png");
  }
  plane.Free();
  picture.gl.Free();
}

// ---------------------------------------------------------------------------

// The mean of the luma gradient's magnitude, 0-255 a pixel: how much edge
// contrast a picture keeps.
double Sharpness(const Image& image) {
  double sum = 0;
  for (int y = 0; y + 1 < image.height; ++y) {
    for (int x = 0; x + 1 < image.width; ++x) {
      const double l = plane_reference::Luma(image, x, y);
      sum += std::hypot(plane_reference::Luma(image, x + 1, y) - l, plane_reference::Luma(image, x, y + 1) - l);
    }
  }
  return sum * 255 / ((image.width - 1.0) * (image.height - 1.0));
}

void Quality(const PbVideoModule* module) {
  printf("\nquality: SSIM (luma, 8x8) against a Lanczos-3 reference of the same picture, and mean edge contrast\n");
  // Shrinking a 2560x1440 desktop.
  {
    Converted picture(module, TextPicture(2560, 1440));
    if (!picture.Convert()) return;
    pb_video::PlaneGl plane;
    std::string failure;
    plane.Setup(&failure);
    printf("  2560x1440 shown smaller: scale, size, filter; SSIM against Lanczos-3 and PSNR against the exact area\n"
           "  average, the plane's / bilinear's (the plane before)\n");
    for (const double scale : {0.75, 0.5, 0.375, 0.25}) {
      const int width = static_cast<int>(std::lround(2560 * scale));
      const int height = static_cast<int>(std::lround(1440 * scale));
      Target target(width, height);
      const pb_video::PlaneRect placed = Rect(0, 0, width, height);
      const Drawn drawn = Draw(&plane, picture, target, placed, placed, pb_video::Enhance::kOff, false, 1);
      const Image lanczos = plane_reference::Lanczos(picture.rgba, width, height);
      const Image box = plane_reference::Box(picture.rgba, width, height, 0, 0, width, height);
      const Image bilinear = plane_reference::Stretch(picture.rgba, width, height, 0, 0, width, height);
      printf("  %.3fx %4dx%-4d %-13s SSIM %.4f / %.4f, PSNR %.1f / %.1f dB\n", scale, width, height,
             pb_video::PlaneFilterName(drawn.filter), plane_reference::Ssim(drawn.whole, lanczos),
             plane_reference::Ssim(bilinear, lanczos), plane_reference::Psnr(drawn.whole, box),
             plane_reference::Psnr(bilinear, box));
    }
    plane.Free();
    picture.gl.Free();
  }
  // Enlarging a 1920x1080 desktop.
  {
    Converted picture(module, TextPicture(1920, 1080));
    if (!picture.Convert()) return;
    pb_video::PlaneGl plane;
    std::string failure;
    plane.Setup(&failure);
    printf("  1920x1080 shown larger, the top left 1280x720 of the window: scale, level, filter, SSIM, edge "
           "contrast\n");
    for (const double scale : {1.5, 2.0}) {
      const int width = static_cast<int>(std::lround(1920 * scale));
      const int height = static_cast<int>(std::lround(1080 * scale));
      Target target(1280, 720);
      const pb_video::PlaneRect placed = Rect(0, 0, width, height);
      const Image lanczos = plane_reference::Lanczos(picture.rgba, width, height).Crop(0, 0, 1280, 720);
      for (const pb_video::Enhance level :
           {pb_video::Enhance::kOff, pb_video::Enhance::kLow, pb_video::Enhance::kMedium, pb_video::Enhance::kHigh}) {
        const Drawn drawn = Draw(&plane, picture, target, placed, Rect(0, 0, 1280, 720), level, false, 1);
        printf("  %.1fx %-6s %-14s SSIM %.4f, edge contrast %.2f\n", scale, Name(level),
               pb_video::PlaneFilterName(drawn.filter), plane_reference::Ssim(drawn.whole, lanczos),
               Sharpness(drawn.whole));
      }
      printf("  %.1fx Lanczos-3 reference: edge contrast %.2f\n", scale, Sharpness(lanczos));
    }
    plane.Free();
    picture.gl.Free();
  }
}

// ---------------------------------------------------------------------------

void GpuTimes(const PbVideoModule* module) {
  printf("\nGPU time of the plane's picture passes (its own GL_TIME_ELAPSED queries), 300 draws after 30, ms\n");
  struct Case {
    int source_width, source_height, window_width, window_height;
    pb_video::Enhance enhance;
    // A new picture before each draw (its serial changes), as at the
    // session's frame rate; else redraws of one.
    bool fresh;
  };
  const Case cases[] = {
      {1920, 1080, 1920, 1080, pb_video::Enhance::kOff, true},
      {1920, 1080, 1920, 1080, pb_video::Enhance::kMedium, true},
      {1920, 1080, 2560, 1440, pb_video::Enhance::kOff, true},
      {1920, 1080, 2560, 1440, pb_video::Enhance::kMedium, true},
      {1920, 1080, 3840, 2160, pb_video::Enhance::kOff, true},
      {1920, 1080, 3840, 2160, pb_video::Enhance::kMedium, true},
      {2560, 1440, 3840, 2160, pb_video::Enhance::kHigh, true},
      {2560, 1440, 1920, 1080, pb_video::Enhance::kOff, true},
      {3840, 2160, 1920, 1080, pb_video::Enhance::kOff, true},
      {3840, 2160, 1280, 720, pb_video::Enhance::kOff, true},
      {3840, 2160, 960, 540, pb_video::Enhance::kOff, true},
      {3840, 2160, 960, 540, pb_video::Enhance::kOff, false},
  };
  printf("  %-11s %-11s %-7s %-14s %-9s %8s %8s\n", "picture", "window", "level", "filter", "pictures", "p50",
         "p95");
  for (const Case& c : cases) {
    Converted picture(module, TextPicture(c.source_width, c.source_height));
    if (!picture.Convert()) continue;
    Target target(c.window_width, c.window_height);
    pb_video::PlaneGl plane;
    std::string failure;
    plane.Setup(&failure);
    const pb_video::PlaneRect placed = Rect(0, 0, c.window_width, c.window_height);
    pb_video::PlaneFrame frame;
    frame.texture = picture.name;
    frame.width = picture.width;
    frame.height = picture.height;
    frame.picture = placed;
    frame.clip = placed;
    frame.enhance = c.enhance;
    frame.target_width = c.window_width;
    frame.target_height = c.window_height;
    glBindFramebuffer(GL_FRAMEBUFFER, target.framebuffer);
    std::string fallback;
    for (int i = 0; i < 30; ++i) {
      frame.serial = c.fresh ? 1000 + i : 1;
      plane.Draw(frame, &fallback);
    }
    glFinish();
    plane.Time(true);
    for (int i = 0; i < 300; ++i) {
      frame.serial = c.fresh ? 2000 + i : 1;
      plane.Draw(frame, &fallback);
      // A frame at a time, finished before the next, so that each draw's
      // query is answered by the next draw's poll.
      glFinish();
    }
    // One more draw reads the last answer.
    plane.Draw(frame, &fallback);
    const pb_video::PlaneGpuTimes times = plane.GpuTimes();
    plane.Time(false);
    printf("  %4dx%-6d %4dx%-6d %-7s %-14s %-9s %8.3f %8.3f  (%zu timed)\n", c.source_width, c.source_height,
           c.window_width, c.window_height, Name(c.enhance), pb_video::PlaneFilterName(plane.last_filter()),
           c.fresh ? "new" : "redrawn", times.p50_ms, times.p95_ms, times.samples);
    if (!fallback.empty()) printf("  fallback: %s\n", fallback.c_str());
    plane.Free();
    picture.gl.Free();
  }
}

}  // namespace

int main(int argc, char** argv) {
  const std::string crops = argc > 1 ? argv[1] : "";
  CGLPixelFormatAttribute attributes[] = {kCGLPFAOpenGLProfile,
                                          static_cast<CGLPixelFormatAttribute>(kCGLOGLPVersion_3_2_Core),
                                          kCGLPFAAccelerated, static_cast<CGLPixelFormatAttribute>(0)};
  CGLPixelFormatObj format = nullptr;
  GLint formats = 0;
  CGLContextObj context = nullptr;
  if (CGLChoosePixelFormat(attributes, &format, &formats) != kCGLNoError || !format ||
      CGLCreateContext(format, nullptr, &context) != kCGLNoError) {
    printf("no OpenGL 3.2 core context\n");
    return 1;
  }
  CGLSetCurrentContext(context);
  printf("GL: %s | %s | GLSL %s\n\n", glGetString(GL_VERSION), glGetString(GL_RENDERER),
         glGetString(GL_SHADING_LANGUAGE_VERSION));
  PbVideoModule module{};
  module.abi = PB_VIDEO_MODULE_ABI;
  module.describe = Describe;
  module.retain = Retain;
  module.release = Release;
  Checks(&module, crops);
  Quality(&module);
  GpuTimes(&module);
  printf("\n%s: %d check(s) failed\n", g_failed ? "FAILED" : "PASSED", g_failed);
  CGLSetCurrentContext(nullptr);
  CGLDestroyContext(context);
  CGLReleasePixelFormat(format);
  return g_failed ? 1 : 0;
}
