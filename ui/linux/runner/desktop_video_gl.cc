// The Linux viewer's GPU conversion (desktop_video_gl.h). One shader pass
// turns the decoder's own picture into the RGBA texture Flutter draws:
// planes in system memory are uploaded as one- and two-channel textures
// (1.5 bytes a pixel instead of the 4 an RGBA picture takes), dma-buf
// planes are sampled where the decoder wrote them. Flutter's raster thread
// calls it inside Skia's or Impeller's frame, so every GL binding and switch
// it touches is saved and restored, as Flutter's own compositor does around
// its drawing (fl_compositor_opengl.cc).
#include "desktop_video_gl.h"

#include <sys/stat.h>

#include <algorithm>
#include <cstring>
#include <mutex>
#include <system_error>
#include <thread>
#include <utility>

namespace pb_video {
namespace {

// drm_fourcc.h's codes, spelled out so that the runner needs no libdrm.
constexpr uint32_t Fourcc(char a, char b, char c, char d) {
  return static_cast<uint32_t>(a) | (static_cast<uint32_t>(b) << 8) | (static_cast<uint32_t>(c) << 16) |
         (static_cast<uint32_t>(d) << 24);
}
constexpr uint32_t kDrmR8 = Fourcc('R', '8', ' ', ' ');
constexpr uint32_t kDrmGR88 = Fourcc('G', 'R', '8', '8');
constexpr uint64_t kModifierLinear = 0;
// The largest side of a picture this converts; a session's is 3840.
constexpr uint32_t kMaxSide = 8192;
// dma-buf pictures the GPU may still be reading: the decoder lends them
// from its pool, so the rest go back as soon as their fence signals.
constexpr size_t kMaxInFlight = 2;
// dma-buf planes kept imported, two per pooled buffer: at most this many,
// and none that went unsampled for kImageIdle dma-buf conversions (two
// seconds at 60 fps), whose buffer has left the decoder's pool or sits
// unused in it. An imported plane keeps its buffer alive.
constexpr size_t kMaxImages = 64;
constexpr uint64_t kImageIdle = 120;
constexpr EGLTimeKHR kFenceWaitNs = 1000000000ull;
// A conversion draws to texture units 0 to 2.
constexpr int kUnits = 3;
// EGL_EXT_device_drm_render_node's name, which an older libepoxy's registry
// may not have.
#ifndef EGL_DRM_RENDER_NODE_FILE_EXT
#define EGL_DRM_RENDER_NODE_FILE_EXT 0x3377
#endif

// The shaders are written once for GLSL ES 1.00 and GLSL 1.50 (a desktop
// GL context's) through the names ShaderHeader defines.
const char kVertexShader[] =
    "ATTRIBUTE vec2 position;\n"
    "VARYING vec2 coordinate;\n"
    "void main() {\n"
    "  coordinate = position * 0.5 + 0.5;\n"
    "  gl_Position = vec4(position, 0.0, 1.0);\n"
    "}\n";

// Chroma is sampled nearest, each sample covering its two-by-two luma
// pixels, as GStreamer's converter does without chroma resampling: the
// conversion videoconvert made before, on the GPU.
const char kFragmentShader[] =
    "#ifdef GL_FRAGMENT_PRECISION_HIGH\n"
    "precision highp float;\n"
    "#else\n"
    "precision mediump float;\n"
    "#endif\n"
    "VARYING vec2 coordinate;\n"
    "uniform sampler2D luma;\n"
    "uniform sampler2D chroma;\n"
    "uniform sampler2D chroma_v;\n"
    "uniform vec2 luma_scale;\n"
    "uniform vec2 chroma_scale;\n"
    "uniform vec3 offset;\n"
    "uniform vec3 scale;\n"
    "uniform vec3 cb;\n"
    "uniform vec3 cr;\n"
    "void main() {\n"
    "  vec3 yuv;\n"
    "  yuv.x = TEXTURE(luma, coordinate * luma_scale).r;\n"
    "#if defined(PLANAR)\n"
    "  yuv.y = TEXTURE(chroma, coordinate * chroma_scale).r;\n"
    "  yuv.z = TEXTURE(chroma_v, coordinate * chroma_scale).r;\n"
    "#elif defined(LUMINANCE_ALPHA)\n"
    "  yuv.yz = TEXTURE(chroma, coordinate * chroma_scale).ra;\n"
    "#else\n"
    "  yuv.yz = TEXTURE(chroma, coordinate * chroma_scale).rg;\n"
    "#endif\n"
    "  yuv = (yuv - offset) * scale;\n"
    "  FRAG_COLOR = vec4(clamp(vec3(yuv.x) + yuv.y * cb + yuv.z * cr, 0.0, 1.0), 1.0);\n"
    "}\n";

// The snapshot's reduction: sixteen bilinear taps, each the mean of four
// pixels, spread over the source pixels one target pixel covers.
const char kDownscaleShader[] =
    "#ifdef GL_FRAGMENT_PRECISION_HIGH\n"
    "precision highp float;\n"
    "#else\n"
    "precision mediump float;\n"
    "#endif\n"
    "VARYING vec2 coordinate;\n"
    "uniform sampler2D luma;\n"
    "uniform vec2 spacing;\n"
    "void main() {\n"
    "  vec4 sum = vec4(0.0);\n"
    "  for (int i = 0; i < 4; ++i) {\n"
    "    for (int j = 0; j < 4; ++j) {\n"
    "      sum += TEXTURE(luma, coordinate + (vec2(float(i), float(j)) - 1.5) * spacing);\n"
    "    }\n"
    "  }\n"
    "  FRAG_COLOR = sum / 16.0;\n"
    "}\n";

// The video plane's quad: the corners of `bounds` (left, top, right, bottom
// in clip space), the picture's first row at the top.
const char kPlaneVertexShader[] =
    "ATTRIBUTE vec2 position;\n"
    "uniform vec4 bounds;\n"
    "VARYING vec2 coordinate;\n"
    "void main() {\n"
    "  vec2 corner = position * 0.5 + 0.5;\n"
    "  coordinate = vec2(corner.x, 1.0 - corner.y);\n"
    "  gl_Position = vec4(mix(bounds.x, bounds.z, corner.x), mix(bounds.w, bounds.y, corner.y), 0.0, 1.0);\n"
    "}\n";

// The converted picture through `tone`: the identity, or the grey still's
// matrix.
const char kPlaneFragmentShader[] =
    "#ifdef GL_FRAGMENT_PRECISION_HIGH\n"
    "precision highp float;\n"
    "#else\n"
    "precision mediump float;\n"
    "#endif\n"
    "VARYING vec2 coordinate;\n"
    "uniform sampler2D picture;\n"
    "uniform mat3 tone;\n"
    "void main() {\n"
    "  FRAG_COLOR = vec4(clamp(tone * TEXTURE(picture, coordinate).rgb, 0.0, 1.0), 1.0);\n"
    "}\n";

// GreyStill's ColorFilter (viewer_overlays.dart), column by column: each
// channel takes BT.709's luma weights at half brightness.
const GLfloat kGreyTone[9] = {0.1063f, 0.1063f, 0.1063f, 0.3576f, 0.3576f, 0.3576f, 0.0361f, 0.0361f, 0.0361f};
const GLfloat kIdentityTone[9] = {1, 0, 0, 0, 1, 0, 0, 0, 1};

const GLfloat kQuad[] = {-1.0f, -1.0f, 1.0f, -1.0f, -1.0f, 1.0f, 1.0f, 1.0f};

// What a shader of `type` needs before its source: the version, and the
// names the sources use for what GLSL ES 1.00 and GLSL 1.50 spell apart.
std::string ShaderHeader(bool desktop, GLenum type) {
  if (!desktop) {
    return "#version 100\n"
           "#define ATTRIBUTE attribute\n"
           "#define VARYING varying\n"
           "#define TEXTURE texture2D\n"
           "#define FRAG_COLOR gl_FragColor\n";
  }
  if (type == GL_VERTEX_SHADER) {
    return "#version 150\n"
           "#define ATTRIBUTE in\n"
           "#define VARYING out\n";
  }
  return "#version 150\n"
         "#define VARYING in\n"
         "#define TEXTURE texture\n"
         "out vec4 frag_color;\n"
         "#define FRAG_COLOR frag_color\n";
}

// GL names to delete in a context's share group.
struct Names {
  // Vertex array objects are a desktop GL context's own, never shared.
  std::vector<GLuint> textures, framebuffers, buffers, programs, vertex_arrays;
  static void Add(std::vector<GLuint>* bin, GLuint name) {
    if (name != 0) bin->push_back(name);
  }
  static void Move(std::vector<GLuint>* to, std::vector<GLuint>* from) {
    to->insert(to->end(), from->begin(), from->end());
    from->clear();
  }
  void Take(Names* other) {
    Move(&textures, &other->textures);
    Move(&framebuffers, &other->framebuffers);
    Move(&buffers, &other->buffers);
    Move(&programs, &other->programs);
    Move(&vertex_arrays, &other->vertex_arrays);
  }
  bool empty() const {
    return textures.empty() && framebuffers.empty() && buffers.empty() && programs.empty() && vertex_arrays.empty();
  }
  // With a context of the share group current on this thread.
  void Delete() const {
    if (!textures.empty()) glDeleteTextures(static_cast<GLsizei>(textures.size()), textures.data());
    if (!framebuffers.empty()) glDeleteFramebuffers(static_cast<GLsizei>(framebuffers.size()), framebuffers.data());
    if (!buffers.empty()) glDeleteBuffers(static_cast<GLsizei>(buffers.size()), buffers.data());
    if (!vertex_arrays.empty()) {
      glDeleteVertexArrays(static_cast<GLsizei>(vertex_arrays.size()), vertex_arrays.data());
    }
    for (const GLuint program : programs) glDeleteProgram(program);
  }
};

// GL names a converter left where EGL could not delete them at once,
// deleted by the next call of any converter on the context's thread.
std::mutex g_garbage_mutex;
Names g_garbage;

void Collect() {
  Names names;
  {
    std::lock_guard<std::mutex> lock(g_garbage_mutex);
    if (g_garbage.empty()) return;
    names.Take(&g_garbage);
  }
  names.Delete();
}

// Deletes `names` in a context of its own that shares `share`'s objects,
// made current with no surface on this thread, which has none current.
bool DeleteInOwnContext(EGLDisplay display, EGLContext share, const Names& names) {
  EGLint api = 0, version = 0, config_id = 0;
  if (!eglQueryContext(display, share, EGL_CONTEXT_CLIENT_TYPE, &api) ||
      !eglQueryContext(display, share, EGL_CONTEXT_CLIENT_VERSION, &version) ||
      !eglBindAPI(static_cast<EGLenum>(api))) {
    return false;
  }
  // The share context's own configuration, or none where EGL takes none.
  EGLConfig config = nullptr;
  if (eglQueryContext(display, share, EGL_CONFIG_ID, &config_id) && config_id > 0) {
    const EGLint wanted[] = {EGL_CONFIG_ID, config_id, EGL_NONE};
    EGLint found = 0;
    if (!eglChooseConfig(display, wanted, &config, 1, &found) || found < 1) config = nullptr;
  }
  if (!config && !epoxy_has_egl_extension(display, "EGL_KHR_no_config_context")) return false;
  const EGLint attributes[] = {EGL_CONTEXT_CLIENT_VERSION, version, EGL_NONE};
  const EGLContext context = eglCreateContext(display, config, share, attributes);
  if (context == EGL_NO_CONTEXT) return false;
  const bool current = eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, context) == EGL_TRUE;
  if (current) {
    names.Delete();
    // Done before the context goes, so that the share group sees it.
    glFinish();
    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
  }
  eglDestroyContext(display, context);
  return current;
}

// The same, on a thread of its own: the calling thread may have another
// context current (GDK's, on the GTK thread that finalizes a texture),
// which a context made current here would replace.
bool DeleteInShareGroup(EGLDisplay display, EGLContext share, const Names& names) {
  if (display == EGL_NO_DISPLAY || share == EGL_NO_CONTEXT ||
      !epoxy_has_egl_extension(display, "EGL_KHR_surfaceless_context")) {
    return false;
  }
  bool deleted = false;
  try {
    std::thread worker([&] {
      deleted = DeleteInOwnContext(display, share, names);
      eglReleaseThread();
    });
    worker.join();
  } catch (const std::system_error&) {
    return false;
  }
  return deleted;
}

// Errors another renderer left are not this conversion's.
void ClearErrors() {
  for (int i = 0; i < 16 && glGetError() != GL_NO_ERROR; ++i) {
  }
}

void BindVertexArray(bool es3, GLuint name) {
  if (es3) {
    glBindVertexArray(name);
  } else {
    glBindVertexArrayOES(name);
  }
}

// What of a context's state a conversion saves and sets.
struct StateCaps {
  // GL ES 3 or desktop GL 3.2: read framebuffers, pixel buffers, more pixel
  // storage, rasterizer discard.
  bool modern = false;
  // Sampler objects and attribute divisors: GL ES 3, desktop GL 3.3.
  bool samplers = false;
  // GL ES with vertex array objects: the one bound is saved and the
  // default one used.
  bool vertex_arrays = false;
  // A desktop GL context, which has no default vertex array object: the
  // converter's own is bound instead, and what attribute 0 holds there is
  // the converter's alone.
  GLuint own_vertex_array = 0;
  bool row_length = false;
};

// Every binding and switch a conversion touches, saved and set to what it
// needs, and restored when it goes. Skia resets its own state before an
// external texture's populate; Impeller and the next caller may not.
class SavedState {
 public:
  explicit SavedState(const StateCaps& caps) : caps_(caps) {
    glGetIntegerv(GL_FRAMEBUFFER_BINDING, &framebuffer_);
    if (caps_.modern) glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &read_framebuffer_);
    glGetIntegerv(GL_VIEWPORT, viewport_);
    glGetIntegerv(GL_CURRENT_PROGRAM, &program_);
    glGetIntegerv(GL_ACTIVE_TEXTURE, &active_texture_);
    for (int unit = 0; unit < kUnits; ++unit) {
      glActiveTexture(GL_TEXTURE0 + unit);
      glGetIntegerv(GL_TEXTURE_BINDING_2D, &textures_[unit]);
      if (caps_.samplers) {
        glGetIntegerv(GL_SAMPLER_BINDING, &samplers_[unit]);
        glBindSampler(static_cast<GLuint>(unit), 0);
      }
    }
    glActiveTexture(GL_TEXTURE0);
    glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &array_buffer_);
    if (caps_.own_vertex_array != 0) {
      glGetIntegerv(GL_VERTEX_ARRAY_BINDING, &vertex_array_);
      glBindVertexArray(caps_.own_vertex_array);
    } else {
      if (caps_.vertex_arrays) {
        glGetIntegerv(GL_VERTEX_ARRAY_BINDING, &vertex_array_);
        BindVertexArray(caps_.modern, 0);
      }
      // Attribute 0 of the default vertex array, which the quad uses.
      glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &attribute_enabled_);
      glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_SIZE, &attribute_size_);
      glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_TYPE, &attribute_type_);
      glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_NORMALIZED, &attribute_normalized_);
      glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_STRIDE, &attribute_stride_);
      glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING, &attribute_buffer_);
      glGetVertexAttribPointerv(0, GL_VERTEX_ATTRIB_ARRAY_POINTER, &attribute_pointer_);
      if (caps_.samplers) {
        glGetVertexAttribiv(0, GL_VERTEX_ATTRIB_ARRAY_DIVISOR, &attribute_divisor_);
        glVertexAttribDivisor(0, 0);
      }
    }
    glGetIntegerv(GL_UNPACK_ALIGNMENT, &unpack_alignment_);
    glGetIntegerv(GL_PACK_ALIGNMENT, &pack_alignment_);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    if (caps_.row_length) {
      glGetIntegerv(GL_UNPACK_ROW_LENGTH, &unpack_row_length_);
      glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
    }
    if (caps_.modern) {
      glGetIntegerv(GL_UNPACK_SKIP_ROWS, &unpack_skip_rows_);
      glGetIntegerv(GL_UNPACK_SKIP_PIXELS, &unpack_skip_pixels_);
      glGetIntegerv(GL_PACK_ROW_LENGTH, &pack_row_length_);
      glGetIntegerv(GL_PACK_SKIP_ROWS, &pack_skip_rows_);
      glGetIntegerv(GL_PACK_SKIP_PIXELS, &pack_skip_pixels_);
      glGetIntegerv(GL_PIXEL_UNPACK_BUFFER_BINDING, &unpack_buffer_);
      glGetIntegerv(GL_PIXEL_PACK_BUFFER_BINDING, &pack_buffer_);
      glPixelStorei(GL_UNPACK_SKIP_ROWS, 0);
      glPixelStorei(GL_UNPACK_SKIP_PIXELS, 0);
      glPixelStorei(GL_PACK_ROW_LENGTH, 0);
      glPixelStorei(GL_PACK_SKIP_ROWS, 0);
      glPixelStorei(GL_PACK_SKIP_PIXELS, 0);
      glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0);
      glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    }
    glGetBooleanv(GL_COLOR_WRITEMASK, color_mask_);
    glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
    for (size_t i = 0; i < Switches(); ++i) {
      enabled_[i] = glIsEnabled(kSwitches[i]);
      if (enabled_[i]) glDisable(kSwitches[i]);
    }
  }
  SavedState(const SavedState&) = delete;
  SavedState& operator=(const SavedState&) = delete;
  ~SavedState() {
    for (size_t i = 0; i < Switches(); ++i) {
      if (enabled_[i]) glEnable(kSwitches[i]);
    }
    glColorMask(color_mask_[0], color_mask_[1], color_mask_[2], color_mask_[3]);
    if (caps_.modern) {
      glBindBuffer(GL_PIXEL_PACK_BUFFER, static_cast<GLuint>(pack_buffer_));
      glBindBuffer(GL_PIXEL_UNPACK_BUFFER, static_cast<GLuint>(unpack_buffer_));
      glPixelStorei(GL_PACK_SKIP_PIXELS, pack_skip_pixels_);
      glPixelStorei(GL_PACK_SKIP_ROWS, pack_skip_rows_);
      glPixelStorei(GL_PACK_ROW_LENGTH, pack_row_length_);
      glPixelStorei(GL_UNPACK_SKIP_PIXELS, unpack_skip_pixels_);
      glPixelStorei(GL_UNPACK_SKIP_ROWS, unpack_skip_rows_);
    }
    if (caps_.row_length) glPixelStorei(GL_UNPACK_ROW_LENGTH, unpack_row_length_);
    glPixelStorei(GL_PACK_ALIGNMENT, pack_alignment_);
    glPixelStorei(GL_UNPACK_ALIGNMENT, unpack_alignment_);
    if (caps_.own_vertex_array == 0) {
      // Attribute 0 of the default vertex array, still bound.
      glBindBuffer(GL_ARRAY_BUFFER, static_cast<GLuint>(attribute_buffer_));
      glVertexAttribPointer(0, attribute_size_, static_cast<GLenum>(attribute_type_),
                            attribute_normalized_ ? GL_TRUE : GL_FALSE, attribute_stride_, attribute_pointer_);
      if (caps_.samplers) glVertexAttribDivisor(0, static_cast<GLuint>(attribute_divisor_));
      if (attribute_enabled_) {
        glEnableVertexAttribArray(0);
      } else {
        glDisableVertexAttribArray(0);
      }
    }
    glBindBuffer(GL_ARRAY_BUFFER, static_cast<GLuint>(array_buffer_));
    if (caps_.own_vertex_array != 0) {
      glBindVertexArray(static_cast<GLuint>(vertex_array_));
    } else if (caps_.vertex_arrays) {
      BindVertexArray(caps_.modern, static_cast<GLuint>(vertex_array_));
    }
    for (int unit = 0; unit < kUnits; ++unit) {
      glActiveTexture(GL_TEXTURE0 + unit);
      glBindTexture(GL_TEXTURE_2D, static_cast<GLuint>(textures_[unit]));
      if (caps_.samplers) glBindSampler(static_cast<GLuint>(unit), static_cast<GLuint>(samplers_[unit]));
    }
    glActiveTexture(static_cast<GLenum>(active_texture_));
    glUseProgram(static_cast<GLuint>(program_));
    glViewport(viewport_[0], viewport_[1], viewport_[2], viewport_[3]);
    if (caps_.modern) {
      glBindFramebuffer(GL_DRAW_FRAMEBUFFER, static_cast<GLuint>(framebuffer_));
      glBindFramebuffer(GL_READ_FRAMEBUFFER, static_cast<GLuint>(read_framebuffer_));
    } else {
      glBindFramebuffer(GL_FRAMEBUFFER, static_cast<GLuint>(framebuffer_));
    }
  }

 private:
  static constexpr GLenum kSwitches[] = {GL_BLEND,
                                         GL_SCISSOR_TEST,
                                         GL_DEPTH_TEST,
                                         GL_STENCIL_TEST,
                                         GL_CULL_FACE,
                                         GL_DITHER,
                                         GL_POLYGON_OFFSET_FILL,
                                         GL_SAMPLE_ALPHA_TO_COVERAGE,
                                         GL_SAMPLE_COVERAGE,
                                         GL_RASTERIZER_DISCARD};
  static constexpr size_t kSwitchCount = sizeof(kSwitches) / sizeof(kSwitches[0]);
  // GL_RASTERIZER_DISCARD, the last, is GL ES 3's and desktop GL 3.0's.
  size_t Switches() const { return caps_.modern ? kSwitchCount : kSwitchCount - 1; }

  const StateCaps caps_;
  GLint framebuffer_ = 0, read_framebuffer_ = 0, viewport_[4] = {0, 0, 0, 0}, program_ = 0;
  GLint active_texture_ = GL_TEXTURE0, textures_[kUnits] = {0, 0, 0}, samplers_[kUnits] = {0, 0, 0};
  GLint array_buffer_ = 0, vertex_array_ = 0;
  GLint attribute_enabled_ = 0, attribute_size_ = 4, attribute_type_ = GL_FLOAT, attribute_normalized_ = 0;
  GLint attribute_stride_ = 0, attribute_buffer_ = 0, attribute_divisor_ = 0;
  void* attribute_pointer_ = nullptr;
  GLint unpack_alignment_ = 4, pack_alignment_ = 4, unpack_row_length_ = 0, unpack_skip_rows_ = 0;
  GLint unpack_skip_pixels_ = 0, pack_row_length_ = 0, pack_skip_rows_ = 0, pack_skip_pixels_ = 0;
  GLint unpack_buffer_ = 0, pack_buffer_ = 0;
  GLboolean color_mask_[4] = {GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE};
  GLboolean enabled_[kSwitchCount] = {};
};
constexpr GLenum SavedState::kSwitches[];

GLuint Compile(GLenum type, const std::string& source, std::string* error) {
  const GLuint shader = glCreateShader(type);
  const GLchar* text = source.c_str();
  glShaderSource(shader, 1, &text, nullptr);
  glCompileShader(shader);
  GLint compiled = GL_FALSE;
  glGetShaderiv(shader, GL_COMPILE_STATUS, &compiled);
  if (compiled == GL_TRUE) return shader;
  GLchar log[512] = "";
  glGetShaderInfoLog(shader, sizeof(log), nullptr, log);
  *error = std::string("Cannot compile the video shader: ") + log;
  glDeleteShader(shader);
  return 0;
}

// A program of the two shaders, which draws its quad from attribute 0
// (`position`); 0, with `error`, when it does not compile or link.
GLuint Build(const std::string& vertex, const std::string& fragment, std::string* error) {
  const GLuint vertex_shader = Compile(GL_VERTEX_SHADER, vertex, error);
  const GLuint fragment_shader = vertex_shader ? Compile(GL_FRAGMENT_SHADER, fragment, error) : 0;
  if (!fragment_shader) {
    if (vertex_shader) glDeleteShader(vertex_shader);
    return 0;
  }
  const GLuint name = glCreateProgram();
  glAttachShader(name, vertex_shader);
  glAttachShader(name, fragment_shader);
  glBindAttribLocation(name, 0, "position");
  glLinkProgram(name);
  // Flagged for deletion, they go with the program.
  glDeleteShader(vertex_shader);
  glDeleteShader(fragment_shader);
  GLint linked = GL_FALSE;
  glGetProgramiv(name, GL_LINK_STATUS, &linked);
  if (linked == GL_TRUE) return name;
  GLchar log[512] = "";
  glGetProgramInfoLog(name, sizeof(log), nullptr, log);
  *error = std::string("Cannot link the video shader: ") + log;
  glDeleteProgram(name);
  return 0;
}

StateCaps Caps(bool modern, bool samplers, bool vertex_arrays, GLuint own_vertex_array, bool row_length) {
  StateCaps caps;
  caps.modern = modern;
  caps.samplers = samplers;
  caps.vertex_arrays = vertex_arrays;
  caps.own_vertex_array = own_vertex_array;
  caps.row_length = row_length;
  return caps;
}

// The colour conversion's uniforms: Y'CbCr samples, normalized, less
// `offset` and times `scale`, give Y' in 0-1 and Cb and Cr in -0.5-0.5;
// R'G'B' is then Y' + Cb times `cb` + Cr times `cr`.
struct Colour {
  GLfloat offset[3], scale[3], cb[3], cr[3];
};
Colour ColourOf(const PbVideoPlanes& planes) {
  double kr = planes.kr, kb = planes.kb;
  // A matrix the caps do not give is BT.709's, which every host encodes.
  if (!(kr > 0 && kb > 0 && kr + kb < 1)) {
    kr = 0.2126;
    kb = 0.0722;
  }
  const double kg = 1 - kr - kb;
  const double luma_offset = planes.full_range ? 0.0 : 16.0 / 255.0;
  const double luma_scale = planes.full_range ? 1.0 : 255.0 / 219.0;
  const double chroma_scale = planes.full_range ? 1.0 : 255.0 / 224.0;
  Colour colour{};
  colour.offset[0] = static_cast<GLfloat>(luma_offset);
  colour.offset[1] = colour.offset[2] = static_cast<GLfloat>(128.0 / 255.0);
  colour.scale[0] = static_cast<GLfloat>(luma_scale);
  colour.scale[1] = colour.scale[2] = static_cast<GLfloat>(chroma_scale);
  colour.cb[0] = 0;
  colour.cb[1] = static_cast<GLfloat>(-2 * kb * (1 - kb) / kg);
  colour.cb[2] = static_cast<GLfloat>(2 * (1 - kb));
  colour.cr[0] = static_cast<GLfloat>(2 * (1 - kr));
  colour.cr[1] = static_cast<GLfloat>(-2 * kr * (1 - kr) / kg);
  colour.cr[2] = 0;
  return colour;
}

// A layout this converts: NV12 or I420 whose planes fit the picture.
bool Valid(const PbVideoPlanes& planes) {
  const uint32_t count = planes.format == PB_VIDEO_FORMAT_NV12 ? 2 : planes.format == PB_VIDEO_FORMAT_I420 ? 3 : 0;
  if (count == 0 || planes.plane_count != count || planes.width == 0 || planes.height == 0 || planes.width > kMaxSide ||
      planes.height > kMaxSide) {
    return false;
  }
  for (uint32_t p = 0; p < count; ++p) {
    const PbVideoPlane& plane = planes.planes[p];
    const uint32_t width = p == 0 ? planes.width : (planes.width + 1) / 2;
    const uint32_t height = p == 0 ? planes.height : (planes.height + 1) / 2;
    const uint32_t bytes = planes.format == PB_VIDEO_FORMAT_NV12 && p == 1 ? 2 : 1;
    if (plane.width != width || plane.height != height || plane.stride < width * bytes) return false;
    if (planes.dmabuf ? plane.fd < 0 : plane.data == nullptr) return false;
  }
  return true;
}

std::vector<uint64_t> Modifiers(EGLDisplay display, uint32_t fourcc) {
  std::vector<uint64_t> found;
  EGLint count = 0;
  const EGLint format = static_cast<EGLint>(fourcc);
  if (!eglQueryDmaBufModifiersEXT(display, format, 0, nullptr, nullptr, &count) || count <= 0) return found;
  std::vector<EGLuint64KHR> modifiers(static_cast<size_t>(count));
  std::vector<EGLBoolean> external(static_cast<size_t>(count));
  if (!eglQueryDmaBufModifiersEXT(display, format, count, modifiers.data(), external.data(), &count)) return found;
  for (EGLint i = 0; i < count && static_cast<size_t>(i) < modifiers.size(); ++i) {
    // External-only layouts sample through samplerExternalOES, never as
    // the GL_TEXTURE_2D planes the shader reads.
    if (!external[static_cast<size_t>(i)]) found.push_back(modifiers[static_cast<size_t>(i)]);
  }
  return found;
}

// Whether the space-separated `list` names `extension`, a whole word.
bool HasExtension(const char* list, const char* extension) {
  if (!list) return false;
  const size_t length = strlen(extension);
  for (const char* at = strstr(list, extension); at; at = strstr(at + length, extension)) {
    if ((at == list || at[-1] == ' ') && (at[length] == '\0' || at[length] == ' ')) return true;
  }
  return false;
}

}  // namespace

void ReadPresenter(EGLDisplay display, PbVideoPresenter* out) {
  *out = PbVideoPresenter{};
  if (display == EGL_NO_DISPLAY || !epoxy_has_egl_extension(display, "EGL_KHR_image_base") ||
      !epoxy_has_egl_extension(display, "EGL_EXT_image_dma_buf_import") ||
      !epoxy_has_egl_extension(display, "EGL_KHR_fence_sync")) {
    return;
  }
  std::vector<uint64_t> modifiers{kModifierLinear};
  if (epoxy_has_egl_extension(display, "EGL_EXT_image_dma_buf_import_modifiers")) {
    const std::vector<uint64_t> luma = Modifiers(display, kDrmR8), chroma = Modifiers(display, kDrmGR88);
    for (const uint64_t modifier : luma) {
      if (modifier == kModifierLinear || modifiers.size() >= PB_VIDEO_MAX_MODIFIERS) continue;
      if (std::find(chroma.begin(), chroma.end(), modifier) != chroma.end()) modifiers.push_back(modifier);
    }
  }
  out->dmabuf = 1;
  for (const uint64_t modifier : modifiers) {
    if (out->modifier_count < PB_VIDEO_MAX_MODIFIERS) out->modifiers[out->modifier_count++] = modifier;
  }
}

std::string PresentingDevice(EGLDisplay display) {
  // Client extensions, which libepoxy finds with no display.
  if (display == EGL_NO_DISPLAY || (!epoxy_has_egl_extension(EGL_NO_DISPLAY, "EGL_EXT_device_query") &&
                                    !epoxy_has_egl_extension(EGL_NO_DISPLAY, "EGL_EXT_device_base"))) {
    return "";
  }
  EGLAttrib value = 0;
  if (!eglQueryDisplayAttribEXT(display, EGL_DEVICE_EXT, &value) || value == 0) return "";
  const auto device = reinterpret_cast<EGLDeviceEXT>(value);
  const char* extensions = eglQueryDeviceStringEXT(device, EGL_EXTENSIONS);
  const char* file = nullptr;
  if (HasExtension(extensions, "EGL_EXT_device_drm_render_node")) {
    file = eglQueryDeviceStringEXT(device, EGL_DRM_RENDER_NODE_FILE_EXT);
  }
  if ((!file || !*file) && HasExtension(extensions, "EGL_EXT_device_drm")) {
    file = eglQueryDeviceStringEXT(device, EGL_DRM_DEVICE_FILE_EXT);
  }
  return file ? file : "";
}

VideoGl::VideoGl(const PbVideoModule* module, bool es2_paths, Owner owner)
    : module_(module), es2_paths_(es2_paths), owner_(owner) {}

VideoGl::~VideoGl() {
  // The GPU drew the last of these at least a frame ago.
  for (const InFlight& flight : in_flight_) {
    if (flight.fence != EGL_NO_SYNC_KHR) eglDestroySyncKHR(display_, flight.fence);
    module_->release(flight.picture);
  }
  // A context of the owner's: Free deleted everything there, or the
  // context's objects stay with it.
  if (owner_ != Owner::kFlutter) return;
  // An image's texture keeps its buffer until the texture goes too.
  Names names;
  for (const Image& image : images_) {
    eglDestroyImageKHR(display_, image.image);
    Names::Add(&names.textures, image.texture);
  }
  for (const PlaneTexture& plane : planes_) Names::Add(&names.textures, plane.name);
  for (const GLuint texture : {output_, small_, black_}) Names::Add(&names.textures, texture);
  for (const GLuint framebuffer : {framebuffer_, small_framebuffer_}) Names::Add(&names.framebuffers, framebuffer);
  Names::Add(&names.buffers, quad_);
  for (const Program& program : programs_) Names::Add(&names.programs, program.name);
  if (names.empty()) return;
  if (context_ != EGL_NO_CONTEXT && eglGetCurrentContext() == context_) {
    names.Delete();
    return;
  }
  if (DeleteInShareGroup(display_, context_, names)) return;
  std::lock_guard<std::mutex> lock(g_garbage_mutex);
  g_garbage.Take(&names);
}

void VideoGl::Free(bool context_gone) {
  for (const InFlight& flight : in_flight_) {
    if (flight.fence != EGL_NO_SYNC_KHR) eglDestroySyncKHR(display_, flight.fence);
    module_->release(flight.picture);
  }
  in_flight_.clear();
  // EGLImages belong to the display, which outlives the context.
  Names names;
  for (const Image& image : images_) {
    eglDestroyImageKHR(display_, image.image);
    Names::Add(&names.textures, image.texture);
  }
  images_.clear();
  for (PlaneTexture& plane : planes_) {
    Names::Add(&names.textures, plane.name);
    plane = PlaneTexture{};
  }
  for (const GLuint texture : {output_, small_, black_}) Names::Add(&names.textures, texture);
  for (const GLuint framebuffer : {framebuffer_, small_framebuffer_}) Names::Add(&names.framebuffers, framebuffer);
  Names::Add(&names.buffers, quad_);
  Names::Add(&names.vertex_arrays, vertex_array_);
  for (Program& program : programs_) {
    Names::Add(&names.programs, program.name);
    program = Program{};
  }
  if (!context_gone) names.Delete();
  output_ = small_ = black_ = framebuffer_ = small_framebuffer_ = quad_ = vertex_array_ = 0;
  width_ = height_ = small_width_ = small_height_ = 0;
  images_width_ = images_height_ = 0;
  images_modifier_ = 0;
  ready_ = converted_ = last_dmabuf_ = last_refused_ = caps_read_ = false;
  dmabuf_conversions_ = 0;
  setup_error_.clear();
}

void VideoGl::ReadCaps() {
  if (caps_read_) return;
  caps_read_ = true;
  const int version = epoxy_gl_version();
  es3_ = !es2_paths_ && !epoxy_is_desktop_gl() && version >= 30;
  desktop_ = !es2_paths_ && epoxy_is_desktop_gl() && version >= 32;
  modern_ = es3_ || desktop_;
  samplers_ = es3_ || (desktop_ && version >= 33);
  texture_rg_ = modern_ || (!es2_paths_ && epoxy_has_gl_extension("GL_EXT_texture_rg"));
  row_length_ = modern_ || (!es2_paths_ && epoxy_has_gl_extension("GL_EXT_unpack_subimage"));
  // A desktop context binds the converter's own instead (OwnVertexArray).
  vertex_arrays_ = es3_ || (!es2_paths_ && !desktop_ && epoxy_has_gl_extension("GL_OES_vertex_array_object"));
  // A GLX context imports no dma-buf and makes no EGL fence.
  const bool egl = owner_ != Owner::kOwnGlx;
  egl_image_ = egl && epoxy_has_gl_extension("GL_OES_EGL_image");
  display_ = egl ? eglGetCurrentDisplay() : EGL_NO_DISPLAY;
  context_ = egl ? eglGetCurrentContext() : EGL_NO_CONTEXT;
  fences_ = display_ != EGL_NO_DISPLAY && epoxy_has_egl_extension(display_, "EGL_KHR_fence_sync");
  modifiers_ =
      display_ != EGL_NO_DISPLAY && epoxy_has_egl_extension(display_, "EGL_EXT_image_dma_buf_import_modifiers");
}

GLuint VideoGl::OwnVertexArray() {
  if (desktop_ && vertex_array_ == 0) glGenVertexArrays(1, &vertex_array_);
  return vertex_array_;
}

bool VideoGl::Setup(std::string* error) {
  if (ready_) return true;
  if (!setup_error_.empty()) {
    *error = setup_error_;
    return false;
  }
  glGenBuffers(1, &quad_);
  glBindBuffer(GL_ARRAY_BUFFER, quad_);
  glBufferData(GL_ARRAY_BUFFER, sizeof(kQuad), kQuad, GL_STATIC_DRAW);
  const GLubyte black[4] = {0, 0, 0, 255};
  glGenTextures(1, &black_);
  glBindTexture(GL_TEXTURE_2D, black_);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
  glTexImage2D(GL_TEXTURE_2D, 0, modern_ ? GL_RGBA8 : GL_RGBA, 1, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, black);
  if (glGetError() != GL_NO_ERROR) {
    setup_error_ = "Cannot create the video texture";
    *error = setup_error_;
    return false;
  }
  ready_ = true;
  return true;
}

const VideoGl::Program* VideoGl::UseProgram(Variant variant, std::string* error) {
  Program& program = programs_[variant];
  if (!program.failure.empty()) {
    *error = program.failure;
    return nullptr;
  }
  if (program.name == 0) {
    std::string fragment = ShaderHeader(desktop_, GL_FRAGMENT_SHADER);
    if (variant == kI420) fragment += "#define PLANAR\n";
    if (variant == kNv12LuminanceAlpha) fragment += "#define LUMINANCE_ALPHA\n";
    fragment += variant == kDownscale ? kDownscaleShader : kFragmentShader;
    const GLuint name = Build(ShaderHeader(desktop_, GL_VERTEX_SHADER) + kVertexShader, fragment, error);
    if (name == 0) {
      program.failure = *error;
      return nullptr;
    }
    program.name = name;
    program.luma = glGetUniformLocation(name, "luma");
    program.chroma = glGetUniformLocation(name, "chroma");
    program.chroma_v = glGetUniformLocation(name, "chroma_v");
    program.luma_scale = glGetUniformLocation(name, "luma_scale");
    program.chroma_scale = glGetUniformLocation(name, "chroma_scale");
    program.offset = glGetUniformLocation(name, "offset");
    program.scale = glGetUniformLocation(name, "scale");
    program.cb = glGetUniformLocation(name, "cb");
    program.cr = glGetUniformLocation(name, "cr");
    program.spacing = glGetUniformLocation(name, "spacing");
  }
  glUseProgram(program.name);
  glUniform1i(program.luma, 0);
  glUniform1i(program.chroma, 1);
  glUniform1i(program.chroma_v, 2);
  return &program;
}

bool VideoGl::Upload(const PbVideoPlanes& planes, GLuint names[3], GLfloat luma_scale[2], GLfloat chroma_scale[2],
                     Variant* variant, std::string* error) {
  const bool nv12 = planes.format == PB_VIDEO_FORMAT_NV12;
  *variant = !nv12 ? kI420 : texture_rg_ ? kNv12 : kNv12LuminanceAlpha;
  glActiveTexture(GL_TEXTURE0);
  for (uint32_t p = 0; p < planes.plane_count; ++p) {
    const PbVideoPlane& plane = planes.planes[p];
    const bool pairs = nv12 && p == 1;
    const uint32_t bytes = pairs ? 2 : 1;
    const GLenum format = pairs ? (texture_rg_ ? GL_RG : GL_LUMINANCE_ALPHA) : (texture_rg_ ? GL_RED : GL_LUMINANCE);
    const GLenum internal = !modern_ ? format : pairs ? GL_RG8 : GL_R8;
    if (plane.stride % bytes != 0) {
      *error = "Unsupported picture stride";
      return false;
    }
    const uint32_t row = plane.stride / bytes;
    // Without GL_UNPACK_ROW_LENGTH the texture takes whole rows, padding
    // included, and the shader scales its coordinates to the picture.
    const uint32_t width = row_length_ ? plane.width : row;
    PlaneTexture& texture = planes_[p];
    if (texture.name == 0) {
      glGenTextures(1, &texture.name);
      glBindTexture(GL_TEXTURE_2D, texture.name);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    } else {
      glBindTexture(GL_TEXTURE_2D, texture.name);
    }
    if (texture.format != internal || texture.width != width || texture.height != plane.height) {
      glTexImage2D(GL_TEXTURE_2D, 0, static_cast<GLint>(internal), static_cast<GLsizei>(width),
                   static_cast<GLsizei>(plane.height), 0, format, GL_UNSIGNED_BYTE, nullptr);
      texture.format = internal;
      texture.width = width;
      texture.height = plane.height;
    }
    if (row_length_) {
      glPixelStorei(GL_UNPACK_ROW_LENGTH, static_cast<GLint>(row));
      glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, static_cast<GLsizei>(plane.width), static_cast<GLsizei>(plane.height),
                      format, GL_UNSIGNED_BYTE, plane.data);
      glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
    } else {
      // Every row whole but the last, which may end at the picture.
      if (plane.height > 1) {
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, static_cast<GLsizei>(row), static_cast<GLsizei>(plane.height - 1),
                        format, GL_UNSIGNED_BYTE, plane.data);
      }
      glTexSubImage2D(GL_TEXTURE_2D, 0, 0, static_cast<GLint>(plane.height - 1), static_cast<GLsizei>(plane.width), 1,
                      format, GL_UNSIGNED_BYTE, plane.data + static_cast<size_t>(plane.stride) * (plane.height - 1));
    }
    names[p] = texture.name;
  }
  luma_scale[0] = static_cast<GLfloat>(planes.planes[0].width) / static_cast<GLfloat>(planes_[0].width);
  chroma_scale[0] = static_cast<GLfloat>(planes.planes[1].width) / static_cast<GLfloat>(planes_[1].width);
  if (!nv12 && planes_[2].width != planes_[1].width) {
    *error = "Unsupported picture layout";
    return false;
  }
  if (glGetError() != GL_NO_ERROR) {
    *error = "Cannot upload a decoded picture";
    return false;
  }
  return true;
}

GLuint VideoGl::ImportPlane(const PbVideoPlane& plane, uint32_t fourcc, uint64_t modifier) {
  // A dma-buf's inode names it for as long as it lives, and an imported
  // image keeps it alive; a descriptor number does not.
  struct stat file{};
  if (fstat(plane.fd, &file) != 0) return 0;
  const uint64_t device = static_cast<uint64_t>(file.st_dev), inode = static_cast<uint64_t>(file.st_ino);
  for (Image& image : images_) {
    if (image.device == device && image.inode == inode && image.offset == plane.offset &&
        image.stride == plane.stride && image.width == plane.width && image.height == plane.height &&
        image.fourcc == fourcc && image.modifier == modifier) {
      image.used = dmabuf_conversions_;
      return image.texture;
    }
  }
  EGLint attributes[] = {EGL_WIDTH,
                         static_cast<EGLint>(plane.width),
                         EGL_HEIGHT,
                         static_cast<EGLint>(plane.height),
                         EGL_LINUX_DRM_FOURCC_EXT,
                         static_cast<EGLint>(fourcc),
                         EGL_DMA_BUF_PLANE0_FD_EXT,
                         plane.fd,
                         EGL_DMA_BUF_PLANE0_OFFSET_EXT,
                         static_cast<EGLint>(plane.offset),
                         EGL_DMA_BUF_PLANE0_PITCH_EXT,
                         static_cast<EGLint>(plane.stride),
                         EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                         static_cast<EGLint>(static_cast<uint32_t>(modifier & 0xffffffffu)),
                         EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
                         static_cast<EGLint>(static_cast<uint32_t>(modifier >> 32)),
                         EGL_NONE};
  // A display without the modifiers extension imports linear buffers
  // only, and takes no modifier.
  if (!modifiers_) attributes[12] = EGL_NONE;
  const EGLImageKHR image = eglCreateImageKHR(display_, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, nullptr, attributes);
  if (image == EGL_NO_IMAGE_KHR) return 0;
  GLuint texture = 0;
  glGenTextures(1, &texture);
  glBindTexture(GL_TEXTURE_2D, texture);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
  glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, static_cast<GLeglImageOES>(image));
  if (glGetError() != GL_NO_ERROR) {
    glDeleteTextures(1, &texture);
    eglDestroyImageKHR(display_, image);
    return 0;
  }
  if (images_.size() >= kMaxImages) {
    // The least recently used goes; the GPU keeps what it still reads.
    const auto oldest = std::min_element(images_.begin(), images_.end(),
                                         [](const Image& a, const Image& b) { return a.used < b.used; });
    glDeleteTextures(1, &oldest->texture);
    eglDestroyImageKHR(display_, oldest->image);
    images_.erase(oldest);
  }
  Image cached;
  cached.device = device;
  cached.inode = inode;
  cached.modifier = modifier;
  cached.used = dmabuf_conversions_;
  cached.offset = plane.offset;
  cached.stride = plane.stride;
  cached.width = plane.width;
  cached.height = plane.height;
  cached.fourcc = fourcc;
  cached.image = image;
  cached.texture = texture;
  images_.push_back(cached);
  return texture;
}

void VideoGl::DropImages() {
  for (const Image& image : images_) {
    glDeleteTextures(1, &image.texture);
    eglDestroyImageKHR(display_, image.image);
  }
  images_.clear();
}

void VideoGl::DropIdleImages() {
  const uint64_t now = dmabuf_conversions_;
  const auto idle = [now](const Image& image) { return now - image.used > kImageIdle; };
  for (const Image& image : images_) {
    if (!idle(image)) continue;
    glDeleteTextures(1, &image.texture);
    eglDestroyImageKHR(display_, image.image);
  }
  images_.erase(std::remove_if(images_.begin(), images_.end(), idle), images_.end());
}

bool VideoGl::Import(const PbVideoPlanes& planes, GLuint names[2], std::string* error) {
  if (!egl_image_ || !fences_ || planes.format != PB_VIDEO_FORMAT_NV12 ||
      (planes.modifier != kModifierLinear && !modifiers_)) {
    refused_ = true;
    *error = "This GL context cannot sample the decoder's dma-buf pictures";
    return false;
  }
  names[0] = ImportPlane(planes.planes[0], kDrmR8, planes.modifier);
  names[1] = names[0] ? ImportPlane(planes.planes[1], kDrmGR88, planes.modifier) : 0;
  if (!names[0] || !names[1]) {
    refused_ = true;
    *error = "Cannot import a decoded dma-buf picture";
    return false;
  }
  return true;
}

bool VideoGl::Target(uint32_t width, uint32_t height, std::string* error) {
  if (output_ == 0) {
    glGenTextures(1, &output_);
    glBindTexture(GL_TEXTURE_2D, output_);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
  }
  if (framebuffer_ == 0) glGenFramebuffers(1, &framebuffer_);
  glBindFramebuffer(GL_FRAMEBUFFER, framebuffer_);
  if (width == width_ && height == height_) return true;
  glActiveTexture(GL_TEXTURE0);
  glBindTexture(GL_TEXTURE_2D, output_);
  glTexImage2D(GL_TEXTURE_2D, 0, modern_ ? GL_RGBA8 : GL_RGBA, static_cast<GLsizei>(width),
               static_cast<GLsizei>(height), 0, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
  glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, output_, 0);
  if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
    width_ = height_ = 0;
    converted_ = false;
    *error = "Cannot render the video texture";
    return false;
  }
  width_ = width;
  height_ = height;
  return true;
}

void VideoGl::Draw() {
  glBindBuffer(GL_ARRAY_BUFFER, quad_);
  glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, nullptr);
  glEnableVertexAttribArray(0);
  glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

void VideoGl::Reap(bool wait) {
  while (!in_flight_.empty()) {
    const InFlight& oldest = in_flight_.front();
    if (oldest.fence != EGL_NO_SYNC_KHR) {
      const EGLint status =
          eglClientWaitSyncKHR(display_, oldest.fence, EGL_SYNC_FLUSH_COMMANDS_BIT_KHR, wait ? kFenceWaitNs : 0);
      if (status == EGL_TIMEOUT_EXPIRED_KHR && !wait) return;
      eglDestroySyncKHR(display_, oldest.fence);
    }
    module_->release(oldest.picture);
    in_flight_.pop_front();
    if (wait && in_flight_.size() <= kMaxInFlight) return;
  }
}

bool VideoGl::Convert(PbVideoPicture* picture, std::string* error) {
  last_refused_ = false;
  PbVideoPlanes planes{};
  if (!picture || module_->describe(picture, &planes) != 0 || !Valid(planes)) {
    *error = "Unsupported decoded picture layout";
    return false;
  }
  if (planes.dmabuf && refused_) {
    // Until the decoder's next keyframe comes in system memory.
    last_refused_ = true;
    *error = "This GL context does not sample the decoder's dma-buf pictures";
    return false;
  }
  ReadCaps();
  ClearErrors();
  // Garbage is Flutter's context's, which a context of the owner's never
  // shares.
  if (owner_ == Owner::kFlutter) Collect();
  SavedState saved(Caps(modern_, samplers_, vertex_arrays_, OwnVertexArray(), row_length_));
  if (!Setup(error)) return false;
  // Pictures whose fence signalled since go back to the decoder.
  Reap(false);
  GLuint names[3] = {0, 0, 0};
  GLfloat luma_scale[2] = {1, 1}, chroma_scale[2] = {1, 1};
  Variant variant = kNv12;
  if (planes.dmabuf) {
    // Another size or layout is another pool: the old one's buffers go.
    if (planes.width != images_width_ || planes.height != images_height_ || planes.modifier != images_modifier_) {
      DropImages();
      images_width_ = planes.width;
      images_height_ = planes.height;
      images_modifier_ = planes.modifier;
    }
    ++dmabuf_conversions_;
    const bool imported = Import(planes, names, error);
    DropIdleImages();
    if (!imported) {
      last_refused_ = refused_;
      return false;
    }
  } else {
    // A pipeline in system memory holds none of the imported buffers.
    if (!images_.empty()) DropImages();
    if (!Upload(planes, names, luma_scale, chroma_scale, &variant, error)) return false;
  }
  const Program* program = UseProgram(variant, error);
  if (!program || !Target(planes.width, planes.height, error)) return false;
  for (int unit = 0; unit < kUnits; ++unit) {
    glActiveTexture(GL_TEXTURE0 + unit);
    glBindTexture(GL_TEXTURE_2D, names[unit]);
  }
  const Colour colour = ColourOf(planes);
  glUniform2f(program->luma_scale, luma_scale[0], luma_scale[1]);
  glUniform2f(program->chroma_scale, chroma_scale[0], chroma_scale[1]);
  glUniform3f(program->offset, colour.offset[0], colour.offset[1], colour.offset[2]);
  glUniform3f(program->scale, colour.scale[0], colour.scale[1], colour.scale[2]);
  glUniform3f(program->cb, colour.cb[0], colour.cb[1], colour.cb[2]);
  glUniform3f(program->cr, colour.cr[0], colour.cr[1], colour.cr[2]);
  glViewport(0, 0, static_cast<GLsizei>(planes.width), static_cast<GLsizei>(planes.height));
  Draw();
  if (glGetError() != GL_NO_ERROR) {
    *error = "Cannot convert a decoded picture";
    return false;
  }
  converted_ = true;
  last_dmabuf_ = planes.dmabuf != 0;
  if (planes.dmabuf) {
    // The decoder may write the buffer again once the GPU has read it.
    InFlight flight;
    flight.picture = module_->retain(picture);
    flight.fence = eglCreateSyncKHR(display_, EGL_SYNC_FENCE_KHR, nullptr);
    if (flight.fence == EGL_NO_SYNC_KHR) glFinish();
    in_flight_.push_back(flight);
    if (in_flight_.size() > kMaxInFlight) Reap(true);
  }
  return true;
}

bool VideoGl::Texture(uint32_t* name, uint32_t* width, uint32_t* height, std::string* error) {
  if (!ready_) {
    ReadCaps();
    ClearErrors();
    SavedState saved(Caps(modern_, samplers_, vertex_arrays_, OwnVertexArray(), row_length_));
    if (!Setup(error)) return false;
  }
  if (converted_) {
    *name = output_;
    *width = width_;
    *height = height_;
  } else {
    *name = black_;
    *width = 1;
    *height = 1;
  }
  return true;
}

bool VideoGl::ReadBack(uint32_t max_width, std::vector<uint8_t>* rgba, uint32_t* width, uint32_t* height) {
  if (!converted_ || width_ == 0 || height_ == 0 || max_width == 0) return false;
  ClearErrors();
  SavedState saved(Caps(modern_, samplers_, vertex_arrays_, OwnVertexArray(), row_length_));
  const uint32_t scaled_width = std::min(max_width, width_);
  const uint32_t scaled_height = std::max<uint32_t>(
      1, static_cast<uint32_t>((static_cast<uint64_t>(height_) * scaled_width + width_ / 2) / width_));
  if (small_ == 0) {
    glGenTextures(1, &small_);
    glBindTexture(GL_TEXTURE_2D, small_);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
  }
  if (small_framebuffer_ == 0) glGenFramebuffers(1, &small_framebuffer_);
  glBindFramebuffer(GL_FRAMEBUFFER, small_framebuffer_);
  if (scaled_width != small_width_ || scaled_height != small_height_) {
    glBindTexture(GL_TEXTURE_2D, small_);
    glTexImage2D(GL_TEXTURE_2D, 0, modern_ ? GL_RGBA8 : GL_RGBA, static_cast<GLsizei>(scaled_width),
                 static_cast<GLsizei>(scaled_height), 0, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, small_, 0);
    small_width_ = small_height_ = 0;
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return false;
    small_width_ = scaled_width;
    small_height_ = scaled_height;
  }
  std::string error;
  const Program* program = UseProgram(kDownscale, &error);
  if (!program) return false;
  glActiveTexture(GL_TEXTURE0);
  glBindTexture(GL_TEXTURE_2D, output_);
  // Four taps a side across the source pixels one target pixel covers.
  glUniform2f(program->spacing, 1.0f / (4.0f * static_cast<GLfloat>(scaled_width)),
              1.0f / (4.0f * static_cast<GLfloat>(scaled_height)));
  glViewport(0, 0, static_cast<GLsizei>(scaled_width), static_cast<GLsizei>(scaled_height));
  Draw();
  rgba->resize(static_cast<size_t>(scaled_width) * scaled_height * 4);
  glReadPixels(0, 0, static_cast<GLsizei>(scaled_width), static_cast<GLsizei>(scaled_height), GL_RGBA, GL_UNSIGNED_BYTE,
               rgba->data());
  if (glGetError() != GL_NO_ERROR) return false;
  *width = scaled_width;
  *height = scaled_height;
  return true;
}

bool VideoGl::TakeDmabufRefusal() {
  if (!refused_ || refusal_taken_) return false;
  refusal_taken_ = true;
  return true;
}

bool PlaneGl::Setup(std::string* error) {
  if (program_ != 0) return true;
  if (tried_) {
    *error = failure_;
    return false;
  }
  tried_ = true;
  const bool desktop = epoxy_is_desktop_gl();
  const int version = epoxy_gl_version();
  if (desktop && version < 32) {
    failure_ = "The video plane needs desktop GL 3.2 or later, or GL ES";
    *error = failure_;
    return false;
  }
  ClearErrors();
  const GLuint program = Build(ShaderHeader(desktop, GL_VERTEX_SHADER) + kPlaneVertexShader,
                               ShaderHeader(desktop, GL_FRAGMENT_SHADER) + kPlaneFragmentShader, &failure_);
  if (program == 0) {
    *error = failure_;
    return false;
  }
  program_ = program;
  picture_ = glGetUniformLocation(program_, "picture");
  bounds_ = glGetUniformLocation(program_, "bounds");
  tone_ = glGetUniformLocation(program_, "tone");
  samplers_ = desktop ? version >= 33 : version >= 30;
  // Desktop GL has no default vertex array object; GL ES 3 takes one too,
  // which then holds the quad's attribute for good.
  if (desktop || version >= 30) {
    glGenVertexArrays(1, &vertex_array_);
    glBindVertexArray(vertex_array_);
  }
  glGenBuffers(1, &quad_);
  glBindBuffer(GL_ARRAY_BUFFER, quad_);
  glBufferData(GL_ARRAY_BUFFER, sizeof(kQuad), kQuad, GL_STATIC_DRAW);
  if (vertex_array_ != 0) {
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, nullptr);
    glEnableVertexAttribArray(0);
  }
  if (glGetError() != GL_NO_ERROR) {
    Free();
    tried_ = true;
    failure_ = "Cannot set up the video plane";
    *error = failure_;
    return false;
  }
  return true;
}

void PlaneGl::Draw(GLuint texture, const PlaneRect& picture, const PlaneRect& clip, bool grey, int32_t target_width,
                   int32_t target_height) {
  if (target_width <= 0 || target_height <= 0) return;
  for (const GLenum cap : {GL_SCISSOR_TEST, GL_BLEND, GL_DEPTH_TEST, GL_STENCIL_TEST, GL_CULL_FACE}) glDisable(cap);
  glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
  glViewport(0, 0, target_width, target_height);
  glClearColor(0, 0, 0, 1);
  glClear(GL_COLOR_BUFFER_BIT);
  // What of the picture shows: inside the clip and the framebuffer.
  const int64_t left = std::max<int64_t>({clip.x, picture.x, 0});
  const int64_t top = std::max<int64_t>({clip.y, picture.y, 0});
  const int64_t right = std::min<int64_t>(
      {static_cast<int64_t>(clip.x) + clip.width, static_cast<int64_t>(picture.x) + picture.width, target_width});
  const int64_t bottom = std::min<int64_t>(
      {static_cast<int64_t>(clip.y) + clip.height, static_cast<int64_t>(picture.y) + picture.height, target_height});
  if (program_ == 0 || texture == 0 || picture.width <= 0 || picture.height <= 0 || right <= left || bottom <= top) {
    return;
  }
  // GL counts rows from the bottom.
  glEnable(GL_SCISSOR_TEST);
  glScissor(static_cast<GLint>(left), static_cast<GLint>(target_height - bottom), static_cast<GLsizei>(right - left),
            static_cast<GLsizei>(bottom - top));
  const double width = target_width, height = target_height;
  const double x0 = picture.x, y0 = picture.y;
  const double x1 = x0 + picture.width, y1 = y0 + picture.height;
  glUseProgram(program_);
  glUniform4f(bounds_, static_cast<GLfloat>(2 * x0 / width - 1), static_cast<GLfloat>(1 - 2 * y0 / height),
              static_cast<GLfloat>(2 * x1 / width - 1), static_cast<GLfloat>(1 - 2 * y1 / height));
  glUniformMatrix3fv(tone_, 1, GL_FALSE, grey ? kGreyTone : kIdentityTone);
  glUniform1i(picture_, 0);
  glActiveTexture(GL_TEXTURE0);
  glBindTexture(GL_TEXTURE_2D, texture);
  if (samplers_) glBindSampler(0, 0);
  if (vertex_array_ != 0) {
    glBindVertexArray(vertex_array_);
  } else {
    glBindBuffer(GL_ARRAY_BUFFER, quad_);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, nullptr);
    glEnableVertexAttribArray(0);
  }
  glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
  glDisable(GL_SCISSOR_TEST);
}

void PlaneGl::Free(bool context_gone) {
  if (!context_gone) {
    if (program_ != 0) glDeleteProgram(program_);
    if (quad_ != 0) glDeleteBuffers(1, &quad_);
    if (vertex_array_ != 0) glDeleteVertexArrays(1, &vertex_array_);
  }
  program_ = quad_ = vertex_array_ = 0;
  picture_ = bounds_ = tone_ = -1;
  samplers_ = tried_ = false;
  failure_.clear();
}

}  // namespace pb_video
