// The Linux viewer's GPU conversion: a decoded picture as the decoder made
// it (NV12 or I420 in system memory, or NV12 in dma-bufs) into the RGBA
// texture Flutter draws, by one shader pass in the GL ES context that
// presents it. It uses only EGL and GL ES through libepoxy, so the clip
// harness (ui/linux/test/desktop_video_test.cc) runs it as the runner does.
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

// Converts one texture's pictures. Every call but the destructor runs on
// the thread whose GL ES context draws the texture (Flutter's raster
// thread), with that context current.
class VideoGl {
 public:
  // `es2_paths`, for the clip harness only: GL ES 2's paths (LUMINANCE
  // planes, padded rows, no vertex array objects) in whatever context.
  explicit VideoGl(const PbVideoModule* module, bool es2_paths = false);
  VideoGl(const VideoGl&) = delete;
  VideoGl& operator=(const VideoGl&) = delete;
  // Any thread, with any context or none current: pictures still on the
  // GPU are released, and the GL objects are deleted in the context's
  // share group, on this thread when that context is current here, else
  // in a context of their own on a thread of their own. Where EGL cannot
  // make one, the next call of any converter in the context deletes them.
  ~VideoGl();

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
  bool ready_ = false;
  std::string setup_error_;
  // What the context offers, read at the first call.
  bool caps_read_ = false, es3_ = false, texture_rg_ = false, row_length_ = false, vertex_arrays_ = false;
  bool egl_image_ = false, fences_ = false, modifiers_ = false;
  EGLDisplay display_ = EGL_NO_DISPLAY;
  // The context the objects belong to (Flutter's raster context).
  EGLContext context_ = EGL_NO_CONTEXT;
  Program programs_[kVariants];
  GLuint quad_ = 0, black_ = 0;
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

}  // namespace pb_video

#endif  // RUNNER_DESKTOP_VIDEO_GL_H_
