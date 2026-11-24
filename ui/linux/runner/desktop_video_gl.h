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

// Draws the converted RGBA texture into the window for the video plane, in
// the plane's own context: GL ES 2 or later (GLSL ES 1.00), or desktop GL
// 3.2 or later (GLSL 1.50). It leaves the state it sets: nothing else draws
// in that context.
class PlaneGl {
 public:
  PlaneGl() = default;
  PlaneGl(const PlaneGl&) = delete;
  PlaneGl& operator=(const PlaneGl&) = delete;
  // What it made belongs to the context: Free deletes it first.
  ~PlaneGl() = default;

  // With the context current: builds the program. False, with `error`,
  // when the context cannot run it; it is not tried again.
  bool Setup(std::string* error);
  // Clears the bound framebuffer, `target_width` x `target_height` pixels,
  // to black, then draws `texture` (RGBA, its first row the picture's top)
  // stretched over `picture`, which may reach past the framebuffer,
  // bilinear, cut to `clip`. `grey` puts it through the grey still's
  // matrix: every channel the luma at half brightness.
  void Draw(GLuint texture, const PlaneRect& picture, const PlaneRect& clip, bool grey, int32_t target_width,
            int32_t target_height);
  // With the context current, or once it is gone (`context_gone`).
  void Free(bool context_gone = false);

 private:
  bool tried_ = false, samplers_ = false;
  std::string failure_;
  GLuint program_ = 0, quad_ = 0, vertex_array_ = 0;
  GLint picture_ = -1, bounds_ = -1, tone_ = -1;
};

}  // namespace pb_video

#endif  // RUNNER_DESKTOP_VIDEO_GL_H_
