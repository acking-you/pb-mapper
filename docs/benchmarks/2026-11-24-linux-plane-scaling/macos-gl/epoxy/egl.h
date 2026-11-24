// Stands in for libepoxy's <epoxy/egl.h> on macOS, which has no EGL, so
// that ui/linux/runner/desktop_video_gl.cc compiles for planecheck.cc. A
// converter made with Owner::kOwnGlx, as planecheck.cc makes it, calls none
// of these but eglGetCurrent*, whose answer it ignores; every function here
// answers that EGL has nothing.
#ifndef PLANECHECK_EPOXY_EGL_H_
#define PLANECHECK_EPOXY_EGL_H_

#include <cstdint>

typedef void* EGLDisplay;
typedef void* EGLContext;
typedef void* EGLConfig;
typedef void* EGLSurface;
typedef void* EGLImageKHR;
typedef void* EGLSyncKHR;
typedef void* EGLClientBuffer;
typedef void* EGLDeviceEXT;
typedef int32_t EGLint;
typedef unsigned int EGLBoolean;
typedef unsigned int EGLenum;
typedef intptr_t EGLAttrib;
typedef uint64_t EGLTimeKHR;
typedef uint64_t EGLuint64KHR;

#define EGL_NO_DISPLAY ((EGLDisplay)0)
#define EGL_NO_CONTEXT ((EGLContext)0)
#define EGL_NO_SURFACE ((EGLSurface)0)
#define EGL_NO_IMAGE_KHR ((EGLImageKHR)0)
#define EGL_NO_SYNC_KHR ((EGLSyncKHR)0)
#define EGL_TRUE 1
#define EGL_NONE 0x3038
#define EGL_CONFIG_ID 0x3028
#define EGL_EXTENSIONS 0x3055
#define EGL_HEIGHT 0x3056
#define EGL_WIDTH 0x3057
#define EGL_CONTEXT_CLIENT_TYPE 0x3097
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_SYNC_FENCE_KHR 0x30F9
#define EGL_SYNC_FLUSH_COMMANDS_BIT_KHR 0x0001
#define EGL_TIMEOUT_EXPIRED_KHR 0x30F5
#define EGL_LINUX_DMA_BUF_EXT 0x3270
#define EGL_LINUX_DRM_FOURCC_EXT 0x3271
#define EGL_DMA_BUF_PLANE0_FD_EXT 0x3272
#define EGL_DMA_BUF_PLANE0_OFFSET_EXT 0x3273
#define EGL_DMA_BUF_PLANE0_PITCH_EXT 0x3274
#define EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT 0x3443
#define EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT 0x3444
#define EGL_DEVICE_EXT 0x322C
#define EGL_DRM_DEVICE_FILE_EXT 0x3233

inline bool epoxy_has_egl_extension(EGLDisplay, const char*) { return false; }
inline EGLDisplay eglGetCurrentDisplay() { return EGL_NO_DISPLAY; }
inline EGLContext eglGetCurrentContext() { return EGL_NO_CONTEXT; }
inline EGLBoolean eglQueryContext(EGLDisplay, EGLContext, EGLint, EGLint*) { return 0; }
inline EGLBoolean eglBindAPI(EGLenum) { return 0; }
inline EGLBoolean eglChooseConfig(EGLDisplay, const EGLint*, EGLConfig*, EGLint, EGLint*) { return 0; }
inline EGLContext eglCreateContext(EGLDisplay, EGLConfig, EGLContext, const EGLint*) { return EGL_NO_CONTEXT; }
inline EGLBoolean eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext) { return 0; }
inline EGLBoolean eglDestroyContext(EGLDisplay, EGLContext) { return 0; }
inline EGLBoolean eglReleaseThread() { return 0; }
inline EGLImageKHR eglCreateImageKHR(EGLDisplay, EGLContext, EGLenum, EGLClientBuffer, const EGLint*) {
  return EGL_NO_IMAGE_KHR;
}
inline EGLBoolean eglDestroyImageKHR(EGLDisplay, EGLImageKHR) { return 0; }
inline EGLSyncKHR eglCreateSyncKHR(EGLDisplay, EGLenum, const EGLint*) { return EGL_NO_SYNC_KHR; }
inline EGLBoolean eglDestroySyncKHR(EGLDisplay, EGLSyncKHR) { return 0; }
inline EGLint eglClientWaitSyncKHR(EGLDisplay, EGLSyncKHR, EGLint, EGLTimeKHR) { return 0; }
inline EGLBoolean eglQueryDmaBufModifiersEXT(EGLDisplay, EGLint, EGLint, EGLuint64KHR*, EGLBoolean*, EGLint*) {
  return 0;
}
inline EGLBoolean eglQueryDisplayAttribEXT(EGLDisplay, EGLint, EGLAttrib*) { return 0; }
inline const char* eglQueryDeviceStringEXT(EGLDeviceEXT, EGLint) { return nullptr; }

#endif  // PLANECHECK_EPOXY_EGL_H_
