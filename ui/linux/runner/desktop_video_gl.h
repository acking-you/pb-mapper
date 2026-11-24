// The Linux viewer's GPU conversion: a decoded picture as the decoder made
// it (NV12 or I420 in system memory, or NV12 in dma-bufs) into the RGBA
// texture Flutter draws, by one shader pass in the GL context that presents
// it: Flutter's GL ES context, or the video plane's own (GDK's: desktop GL
// 3.2 or later, or GL ES), which then draws the picture into the window
// (PlaneGl). It uses only EGL and GL through libepoxy, so the clip harness
// (ui/linux/test/desktop_video_test.cc) runs it as the runner does.
#ifndef RUNNER_DESKTOP_VIDEO_GL_H_
#define RUNNER_DESKTOP_VIDEO_GL_H_

#include <epoxy/egl.h>
#include <epoxy/gl.h>

#include <cstdint>
#include <deque>
#include <string>
#include <vector>

#include "desktop_video_module.h"

namespace pb_video {

// What `display` (initialized) samples in place, read without a context: a
// display that imports dma-bufs (EGL_EXT_image_dma_buf_import) and fences
// them (EGL_KHR_fence_sync) takes NV12 pictures whose planes it imports as
// R8 and GR88 textures, linear or with a modifier it lists for both.
void ReadPresenter(EGLDisplay display, PbVideoPresenter* out);

// The DRM device file of the GPU `display` (initialized) renders on, as
// EGL_EXT_device_query names it: the render node where the device lists
// EGL_EXT_device_drm_render_node, else the primary node
// (EGL_EXT_device_drm). Empty when EGL does not say, as for a software
// renderer, which has no DRM device.
std::string PresentingDevice(EGLDisplay display);

// The context a converter draws in.
enum class Owner {
  // Flutter's raster context: EGL, GL ES, shared by every texture's
  // converter.
  kFlutter,
  // A context of the caller's own, which no other converter draws in: the
  // video plane's GDK context, through EGL (Wayland) ...
  kOwnEgl,
  // ... or GLX (X11), where nothing is imported from a dma-buf.
  kOwnGlx,
};

// Converts one texture's pictures. Every call but the destructor runs on
// the thread whose GL context draws the texture (Flutter's raster thread,
// or the GTK thread for the video plane), with that context current.
class VideoGl {
 public:
  // `es2_paths`, for the clip harness only: GL ES 2's paths (LUMINANCE
  // planes, padded rows, no vertex array objects) in whatever context.
  // A desktop GL context of version 3.2 or later (GDK's) takes GLSL 1.50,
  // R8 and RG8 planes and a vertex array object of the converter's own.
  explicit VideoGl(const PbVideoModule* module, bool es2_paths = false, Owner owner = Owner::kFlutter);
  VideoGl(const VideoGl&) = delete;
  VideoGl& operator=(const VideoGl&) = delete;
  // Any thread, with any context or none current: pictures still on the
  // GPU are released. Flutter's converter deletes its GL objects in the
  // context's share group, on this thread when that context is current
  // here, else in a context of their own on a thread of their own; where
  // EGL cannot make one, the next call of any converter in the context
  // deletes them. A converter in a context of its owner's is freed first
  // (Free); what it still holds then is left to its context, never deleted
  // in another.
  ~VideoGl();

  // A converter in a context of its owner's (kOwnEgl, kOwnGlx): deletes its
  // GL objects with that context current, or forgets them when the context
  // is gone (`context_gone`), and releases the pictures it holds. The next
  // call starts again from nothing.
  void Free(bool context_gone = false);

  // Converts `picture` (the caller keeps its reference; a dma-buf picture
  // is retained until the GPU has read it) into the RGBA texture. False,
  // with `error`, when it cannot: the texture keeps the last picture.
  bool Convert(PbVideoPicture* picture, std::string* error);
  // Whether the last Convert that failed was given a dma-buf picture this
  // context refuses (TakeDmabufRefusal says so once): the decoder falls
  // back to system memory, and the failure is no fault to report.
  bool last_refused() const { return last_refused_; }
  // The texture for Flutter: GL_TEXTURE_2D, RGBA8, the last converted
  // picture's size, or one opaque black pixel before the first.
  bool Texture(uint32_t* name, uint32_t* width, uint32_t* height, std::string* error);
  // The last converted picture scaled to at most `max_width` wide (aspect
  // kept), read back as packed RGBA rows, top first.
  bool ReadBack(uint32_t max_width, std::vector<uint8_t>* rgba, uint32_t* width, uint32_t* height);
  // True once after a dma-buf picture could not be sampled here: the
  // decoder should send system memory instead.
  bool TakeDmabufRefusal();
  // Whether the last converted picture was a dma-buf.
  bool last_dmabuf() const { return last_dmabuf_; }

 private:
  enum Variant { kNv12, kNv12LuminanceAlpha, kI420, kDownscale, kVariants };
  struct Program {
    GLuint name = 0;
    GLint luma = -1, chroma = -1, chroma_v = -1, luma_scale = -1, chroma_scale = -1;
    GLint offset = -1, scale = -1, cb = -1, cr = -1, spacing = -1;
    // Why it did not build: it is not tried again.
    std::string failure;
  };
  // A source plane texture in system memory's path, and what it holds.
  struct PlaneTexture {
    GLuint name = 0;
    GLenum format = 0;
    uint32_t width = 0, height = 0;
  };
  // One dma-buf plane imported once and kept while its buffer is reused;
  // `used` is the dma-buf conversion that last sampled it.
  struct Image {
    uint64_t device = 0, inode = 0, modifier = 0, used = 0;
    uint32_t offset = 0, stride = 0, width = 0, height = 0, fourcc = 0;
    EGLImageKHR image = EGL_NO_IMAGE_KHR;
    GLuint texture = 0;
  };
  struct InFlight {
    PbVideoPicture* picture = nullptr;
    EGLSyncKHR fence = EGL_NO_SYNC_KHR;
  };

  void ReadCaps();
  // A desktop GL context has no default vertex array object: the
  // converter's own, made the first time; 0 in GL ES.
  GLuint OwnVertexArray();
  bool Setup(std::string* error);
  const Program* UseProgram(Variant variant, std::string* error);
  bool Upload(const PbVideoPlanes& planes, GLuint names[3], GLfloat luma_scale[2], GLfloat chroma_scale[2],
              Variant* variant, std::string* error);
  bool Import(const PbVideoPlanes& planes, GLuint names[2], std::string* error);
  GLuint ImportPlane(const PbVideoPlane& plane, uint32_t fourcc, uint64_t modifier);
  // Deletes the imported planes: all of them, or those idle for a while.
  void DropImages();
  void DropIdleImages();
  bool Target(uint32_t width, uint32_t height, std::string* error);
  void Draw();
  // Releases the dma-buf pictures the GPU has read; `wait` waits for the
  // oldest until no more than two are left.
  void Reap(bool wait);

  const PbVideoModule* const module_;
  const bool es2_paths_;
  const Owner owner_;
  bool ready_ = false;
  std::string setup_error_;
  // What the context offers, read at the first call. `desktop_`: desktop
  // GL 3.2 or later; `modern_`: that or GL ES 3, whose state adds read
  // framebuffers, pixel buffers and more pixel storage; `samplers_`:
  // sampler objects and attribute divisors (GL ES 3, desktop GL 3.3).
  bool caps_read_ = false, es3_ = false, desktop_ = false, modern_ = false, samplers_ = false;
  bool texture_rg_ = false, row_length_ = false, vertex_arrays_ = false;
  bool egl_image_ = false, fences_ = false, modifiers_ = false;
  EGLDisplay display_ = EGL_NO_DISPLAY;
  // The context the objects belong to (Flutter's raster context).
  EGLContext context_ = EGL_NO_CONTEXT;
  Program programs_[kVariants];
  GLuint quad_ = 0, black_ = 0, vertex_array_ = 0;
  PlaneTexture planes_[3];
  // The RGBA texture Flutter draws and the framebuffer that renders it.
  GLuint output_ = 0, framebuffer_ = 0;
  uint32_t width_ = 0, height_ = 0;
  // ReadBack's small texture and framebuffer.
  GLuint small_ = 0, small_framebuffer_ = 0;
  uint32_t small_width_ = 0, small_height_ = 0;
  bool converted_ = false, last_dmabuf_ = false, refused_ = false, refusal_taken_ = false, last_refused_ = false;
  // The imported planes, all of one picture size and modifier.
  std::vector<Image> images_;
  uint32_t images_width_ = 0, images_height_ = 0;
  uint64_t images_modifier_ = 0;
  uint64_t dmabuf_conversions_ = 0;
  std::deque<InFlight> in_flight_;
};

// A rectangle in a framebuffer's pixels, from its top left.
struct PlaneRect {
  int32_t x = 0, y = 0, width = 0, height = 0;
};

// How much the plane sharpens a picture it shows at least its own size:
// the viewer's "Sharpen enlarged picture" (`enhance`'s level).
enum class Enhance { kOff, kLow, kMedium, kHigh };

// AMD FidelityFX CAS's peak at `level`, as airhand weighs it
// (client/src/lib/enhance.ts): how far a pixel's luma moves from its four
// neighbours' mean where they hardly differ. 0 at kOff.
float EnhancePeak(Enhance level);

// How the plane scales the picture into its rectangle, by the scale s from
// the picture's pixels to the rectangle's.
enum class PlaneFilter {
  // Nothing of the picture shows.
  kNone,
  // s >= 1 on both axes with no enhancement, and the black pixel before
  // the first picture: bilinear, which at s = 1 is a one-to-one copy.
  kBilinear,
  // s < 1 on either axis: nine bilinear taps spread over the source pixels
  // each pixel covers, averaged ...
  kArea,
  // ... and below s = 1/3, where the context mipmaps the picture, from its
  // mipmaps (made once a picture), trilinear, each tap a third of the span.
  kAreaMipmapped,
  // s = 1 with an enhancement: luma sharpened, CAS.
  kSharpen,
  // s >= 1 on both axes and more on one, with an enhancement: Catmull-Rom
  // from nine bilinear taps, clamped to the four nearest pixels (no
  // ringing), into a texture of the visible region, then luma sharpened.
  kUpscaleSharpen,
};

// Its name in the plane's stats.
const char* PlaneFilterName(PlaneFilter filter);

// How a picture `width` x `height` placed at `picture`, cut to `clip`, goes
// into a framebuffer `target_width` x `target_height`. `mipmaps`: whether
// the context can mipmap a texture of the picture's size.
struct PlanePlan {
  PlaneFilter filter = PlaneFilter::kNone;
  // What of the picture shows: inside the clip and the framebuffer.
  PlaneRect visible;
  // kUpscaleSharpen: the part of the picture enlarged first, the visible
  // part and a pixel around it within the picture, so that the sharpening
  // at its edge sees the picture's pixels beyond.
  PlaneRect region;
};
PlanePlan PlanPlane(uint32_t width, uint32_t height, const PlaneRect& picture, const PlaneRect& clip,
                    Enhance enhance, bool mipmaps, int32_t target_width, int32_t target_height);

// One draw of the video plane.
struct PlaneFrame {
  // The converted picture (RGBA, its first row the picture's top, clamped
  // to its edge) and its size.
  GLuint texture = 0;
  uint32_t width = 0, height = 0;
  // Changes whenever `texture` holds another picture: mipmaps are made once
  // for each.
  uint64_t serial = 0;
  // Where it goes, which may reach past the framebuffer, and the part of
  // the framebuffer it may cover.
  PlaneRect picture, clip;
  // Through the grey still's matrix: every channel the luma at half
  // brightness.
  bool grey = false;
  Enhance enhance = Enhance::kOff;
  // The bound framebuffer's size.
  int32_t target_width = 0, target_height = 0;
};

// The GPU time of the plane's picture passes, while timed.
struct PlaneGpuTimes {
  size_t samples = 0;
  double p50_ms = 0, p95_ms = 0;
};

// Draws the converted RGBA texture into the window for the video plane, in
// the plane's own context: GL ES 2 or later (GLSL ES 1.00), or desktop GL
// 3.2 or later (GLSL 1.50). It leaves the state it sets, the framebuffer
// bound before excepted: nothing else draws in that context. It sets the
// minifying filter of the textures it is given, and leaves it linear.
class PlaneGl {
 public:
  PlaneGl() = default;
  PlaneGl(const PlaneGl&) = delete;
  PlaneGl& operator=(const PlaneGl&) = delete;
  // What it made belongs to the context: Free deletes it first.
  ~PlaneGl() = default;

  // With the context current: builds the bilinear program. False, with
  // `error`, when the context cannot run it; it is not tried again. The
  // other filters' programs are built when first used.
  bool Setup(std::string* error);
  // Clears the bound framebuffer to black, then draws `frame`'s texture
  // stretched over its rectangle, cut to its clip, through the filter
  // PlanPlane chooses. A filter the context cannot run is not tried again,
  // and the picture goes bilinear instead; `fallback` then says why, once
  // for each filter. What it drew with is `last_filter`.
  void Draw(const PlaneFrame& frame, std::string* fallback);
  PlaneFilter last_filter() const { return last_filter_; }
  // While on, the GPU time of each draw's picture passes, from the clear's
  // end to the last pass's, is measured where the context has timer
  // queries (desktop GL 3.3 or later); off drops the figures.
  void Time(bool on);
  // p50 and p95 of the last few hundred times measured. Any time, with no
  // context current: draws read the GPU's answers without waiting, so the
  // newest draws are not in them yet.
  PlaneGpuTimes GpuTimes();
  // With the context current: deletes what only an enhancement needs, the
  // enlargement's texture, and forgets a size of it that could not be
  // made, which the next enhancement tries again. Draw does at
  // Enhance::kOff.
  void Trim();
  // With the context current, or once it is gone (`context_gone`).
  void Free(bool context_gone = false);

 private:
  // A program of the plane's quad and one of its fragment shaders.
  struct Program {
    GLuint name = 0;
    GLint picture = -1, bounds = -1, window = -1, tone = -1;
    GLint spacing = -1, bias = -1, size = -1, texel = -1, limit = -1, peak = -1;
    // Why it did not build: it is not tried again.
    std::string failure;
  };
  struct RunningQuery {
    GLuint name = 0;
    uint64_t epoch = 0;
  };
  enum Kind { kPlain, kAreaProgram, kUpscale, kSharpenProgram, kKinds };

  // The program of `kind`, built the first time; null, with `error` the
  // first time it fails, when it does not build.
  const Program* Use(Kind kind, std::string* error);
  // The texture the enlargement draws into, at least `width` x `height`,
  // bound to its framebuffer; false when it cannot be.
  bool Scaled(int32_t width, int32_t height);
  // Draws the quad over `rect` of a framebuffer `target_width` x
  // `target_height` (from its top left; GL's rows count from the bottom),
  // its texture coordinates running from (s0, t0) at the rect's bottom
  // left to (s1, t1) at its top right.
  void Quad(const Program& program, const PlaneRect& rect, int32_t target_width, int32_t target_height, float s0,
            float t0, float s1, float t1);
  void PollQueries();

  bool tried_ = false, samplers_ = false, desktop_ = false, mipmaps_ = false, timer_queries_ = false;
  GLint max_texture_ = 0;
  std::string failure_;
  Program programs_[kKinds];
  GLuint quad_ = 0, vertex_array_ = 0;
  // kUpscaleSharpen's texture and framebuffer, grown in 256-pixel steps;
  // the size that could not be made, which no draw asks for again until
  // Trim (a smaller one is tried); whether a failure was said, once.
  GLuint scaled_ = 0, scaled_framebuffer_ = 0;
  int32_t scaled_width_ = 0, scaled_height_ = 0;
  int32_t refused_width_ = 0, refused_height_ = 0;
  bool scaled_failed_ = false;
  // The texture and picture whose mipmaps were made last.
  GLuint mipmapped_texture_ = 0;
  uint64_t mipmapped_serial_ = 0;
  PlaneFilter last_filter_ = PlaneFilter::kNone;
  // Timer queries: those still running on the GPU, and the times read.
  bool timing_ = false;
  uint64_t timing_epoch_ = 0;
  std::vector<GLuint> queries_;
  std::deque<RunningQuery> running_;
  std::deque<double> gpu_ms_;
};

}  // namespace pb_video

#endif  // RUNNER_DESKTOP_VIDEO_GL_H_
