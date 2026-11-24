// Stands in for libepoxy's <epoxy/gl.h> on macOS, so that the Linux
// runner's ui/linux/runner/desktop_video_gl.cc compiles unchanged against
// Apple's OpenGL 4.1 core profile for planecheck.cc. Only the desktop GL
// paths run: the GL ES names below are never called there.
#ifndef PLANECHECK_EPOXY_GL_H_
#define PLANECHECK_EPOXY_GL_H_

#define GL_SILENCE_DEPRECATION
#include <OpenGL/gl3.h>
#include <OpenGL/gl3ext.h>

#include <cstdlib>
#include <cstring>

#ifndef GL_LUMINANCE
#define GL_LUMINANCE 0x1909
#endif
#ifndef GL_LUMINANCE_ALPHA
#define GL_LUMINANCE_ALPHA 0x190A
#endif

typedef void* GLeglImageOES;
inline void glBindVertexArrayOES(GLuint) { abort(); }
inline void glEGLImageTargetTexture2DOES(GLenum, GLeglImageOES) { abort(); }

inline bool epoxy_is_desktop_gl() { return true; }

inline int epoxy_gl_version() {
  GLint major = 0, minor = 0;
  glGetIntegerv(GL_MAJOR_VERSION, &major);
  glGetIntegerv(GL_MINOR_VERSION, &minor);
  return major * 10 + minor;
}

inline bool epoxy_has_gl_extension(const char* name) {
  GLint count = 0;
  glGetIntegerv(GL_NUM_EXTENSIONS, &count);
  for (GLint i = 0; i < count; ++i) {
    const char* extension = reinterpret_cast<const char*>(glGetStringi(GL_EXTENSIONS, static_cast<GLuint>(i)));
    if (extension && strcmp(extension, name) == 0) return true;
  }
  return false;
}

#endif  // PLANECHECK_EPOXY_GL_H_
