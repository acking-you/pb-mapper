// The interface between the runner and libpb_mapper_desktop_video.so, the
// GStreamer decoder module the runner loads with dlopen (desktop_video.cc).
// The module is optional: built only where the GStreamer development files
// are installed, and loadable only where GStreamer is. Without it the
// runner's `pb_mapper/desktop_video` channel answers "no decoders" and every
// session uses JPEG.
//
// The module hands over each decoder's own picture, NV12 or I420 in system
// memory or NV12 in dma-bufs, and the runner converts it to RGBA in the GL
// context that presents it (desktop_video_gl.cc). Both sides are built
// together by ui/linux/CMakeLists.txt; the ABI number still guards a stale
// module left in a bundle.
#ifndef RUNNER_DESKTOP_VIDEO_MODULE_H_
#define RUNNER_DESKTOP_VIDEO_MODULE_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The video C ABI of libpb_mapper_ffi.so (desktop_video.rs).
typedef struct PbDesktopVideoMetaV1 {
  uint64_t generation;
  uint64_t frame_id;
  uint64_t seq;
  uint64_t pts_us;
  uint64_t age_us;
  uint32_t width;
  uint32_t height;
  uint32_t len;
  uint32_t flags;
} PbDesktopVideoMetaV1;

// The consumer exports the module pulls and receipts with, resolved by the
// runner from the copy of libpb_mapper_ffi.so that Dart loaded.
typedef struct PbVideoExports {
  int64_t (*attach)(uint64_t source);
  int32_t (*next)(uint64_t consumer, uint32_t timeout_ms, uint8_t* dst,
                  uint32_t capacity, PbDesktopVideoMetaV1* meta);
  int32_t (*presented)(uint64_t consumer, uint64_t generation,
                       uint64_t frame_id);
  int32_t (*keyframe)(uint64_t consumer, uint32_t reason);
  int32_t (*failed)(uint64_t consumer, const char* message_utf8);
  int32_t (*report)(uint64_t consumer, const char* decoder_utf8,
                    uint32_t flags);
  void (*detach)(uint64_t consumer);
} PbVideoExports;

// What a codec decodes here: the largest picture a session should ask for
// (0 x 0: none), whether its decoder runs on hardware, and its element with
// the GPU it decodes on where that is known ("vah264dec on renderD128"), as
// the session reports it.
typedef struct PbVideoCodecProbe {
  uint32_t max_width;
  uint32_t max_height;
  uint32_t hardware;
  char decoder[64];
} PbVideoCodecProbe;

typedef struct PbVideoProbe {
  PbVideoCodecProbe h264;
  PbVideoCodecProbe h265;
} PbVideoProbe;

#define PB_VIDEO_MAX_MODIFIERS 32

// What the presenter samples in place, read by the runner from its EGL
// display before the decoder starts: whether it imports dma-bufs, and the
// DRM format modifiers an NV12 picture may have for it to import each plane
// as an R8 and a GR88 texture. A presenter that imports none gets every
// picture in system memory.
typedef struct PbVideoPresenter {
  uint32_t dmabuf;
  uint32_t modifier_count;
  uint64_t modifiers[PB_VIDEO_MAX_MODIFIERS];
} PbVideoPresenter;

// One of the stats' timings since the read that started it.
typedef struct PbVideoTiming {
  uint64_t samples;
  uint64_t total_us;
  uint64_t max_us;
} PbVideoTiming;

// PbVideoStats.flags: what the current pipeline does.
#define PB_VIDEO_STATS_CONVERTS 1u  // videoconvert turns its pictures into NV12
#define PB_VIDEO_STATS_DMABUF 2u    // its pictures come as dma-bufs

// The counters the channel's `stats` reports.
typedef struct PbVideoStats {
  uint64_t submitted;
  uint64_t decoded;
  uint64_t presented;
  uint64_t dropped;
  uint64_t configurations;
  uint64_t recoveries;
  char decoder[64];
  char last_recovery[256];
  uint32_t flags;
  // The last picture's PbVideoPlanes.format, 0 before the first.
  uint32_t format;
  // While timings are on: from a unit entering the pipeline until its
  // picture is published.
  PbVideoTiming decode;
} PbVideoStats;

// PbVideoPlanes.format.
#define PB_VIDEO_FORMAT_NV12 1u  // Y, then interleaved U and V
#define PB_VIDEO_FORMAT_I420 2u  // Y, U, V

// One plane of a picture, from its first visible row: `data` in system
// memory, or a dma-buf (`fd` >= 0, owned by the picture) whose plane starts
// `offset` bytes in. `width` and `height` count its samples (a U and V pair
// is one sample of an NV12 chroma plane).
typedef struct PbVideoPlane {
  const uint8_t* data;
  int32_t fd;
  uint32_t offset;
  uint32_t stride;
  uint32_t width;
  uint32_t height;
} PbVideoPlane;

// A picture's layout and colour, valid while the picture is retained.
typedef struct PbVideoPlanes {
  // The visible picture: the authorized frame's size.
  uint32_t width;
  uint32_t height;
  uint32_t format;
  uint32_t plane_count;
  // The colour matrix (Kr and Kb of R'G'B' to Y'CbCr) and whether the
  // samples use the full range rather than 16-235.
  float kr;
  float kb;
  uint32_t full_range;
  // 1 when every plane is a dma-buf laid out by `modifier` (a DRM format
  // modifier; 0 is linear).
  uint32_t dmabuf;
  uint64_t modifier;
  PbVideoPlane planes[3];
} PbVideoPlanes;

// A decoded picture, the decoder's own buffer. Counted; it outlives the
// decoder that made it.
typedef struct PbVideoPicture PbVideoPicture;
typedef struct PbVideoDecoder PbVideoDecoder;

typedef struct PbVideoModule {
  uint32_t abi;
  // Reads the GStreamer registry and sysfs and builds nothing: it loads no
  // plugin and opens no device itself. Blocks while GStreamer initializes
  // (a first run scans its plugins), so never on the GTK thread. 0, or -1
  // with `error` set when GStreamer cannot start. `device`, as in `create`,
  // puts the decoders on the presenting GPU first.
  int32_t (*probe)(const char* device, PbVideoProbe* out, char* error,
                   size_t error_len);
  // `presenter` says what the pictures may be (copied). `device` is the DRM
  // device file of the GPU that presents them, as EGL names it (a render
  // node, else a primary node), or null or empty when that is unknown: a
  // hardware decoder on that GPU is tried before one whose GPU is unknown,
  // and that before one on another GPU. `frame_ready` runs on a GStreamer
  // thread whenever a newer picture can be acquired. Nothing here touches
  // GStreamer yet.
  PbVideoDecoder* (*create)(const PbVideoExports* exports,
                            const PbVideoPresenter* presenter,
                            const char* device,
                            void (*frame_ready)(void* context),
                            void* context);
  // Binds a source, replacing the previous one, and starts the decoder
  // thread. The consumer id (> 0), or the code `attach_v1` returned.
  int64_t (*attach)(PbVideoDecoder* decoder, uint64_t source);
  // Stops the decoder thread; detaching wakes a blocked pull, so it is
  // prompt. The last picture stays.
  void (*detach)(PbVideoDecoder* decoder);
  // The compositor's call: the newest picture, retained for the caller, or
  // null before the first. Taking it is no receipt: `present` is. The
  // compositor's thread, non-blocking.
  PbVideoPicture* (*acquire)(PbVideoDecoder* decoder);
  // The compositor shows `picture`, acquired from this decoder: called only
  // once it is converted and on screen. The first presentation of a current
  // picture is its receipt to Rust, which authorizes input; 1 when it sent
  // one. The compositor's thread, non-blocking.
  int32_t (*present)(PbVideoDecoder* decoder, const PbVideoPicture* picture);
  // The compositor could not show a picture it acquired, for a reason other
  // than a dma-buf it refused (`refuse_dmabuf`): at most once a second it
  // reaches Rust as a decoder error, which asks for a keyframe and, five in
  // 30 s with nothing presented, ends the session as decoder_failed. The
  // compositor's thread, non-blocking.
  void (*present_failed)(PbVideoDecoder* decoder, const char* reason_utf8);
  // `timings` non-zero starts the timing accumulators, or keeps them; zero
  // drops them until a read that asks again.
  void (*stats)(PbVideoDecoder* decoder, uint32_t timings, PbVideoStats* out);
  // Detaches and frees the decoder; pictures still retained stay valid.
  void (*destroy)(PbVideoDecoder* decoder);
  // The picture's layout: 0, or -1 when there is none to give.
  int32_t (*describe)(const PbVideoPicture* picture, PbVideoPlanes* out);
  // A system-memory picture as packed RGBA, width x 4 bytes a row, scaled
  // to at most `max_width` wide with its aspect kept, written to `rgba`.
  // 0 with the size set; -2 when `capacity` is short, with the size it
  // needs set; -1 when it cannot (a dma-buf picture, or no converter).
  int32_t (*snapshot)(const PbVideoPicture* picture, uint32_t max_width,
                      uint8_t* rgba, size_t capacity, uint32_t* width,
                      uint32_t* height);
  // The presenter could not sample a dma-buf picture: from the next
  // keyframe, which the decoder asks for, its pictures are in system
  // memory. Any thread.
  void (*refuse_dmabuf)(PbVideoDecoder* decoder);
  PbVideoPicture* (*retain)(PbVideoPicture* picture);
  void (*release)(PbVideoPicture* picture);
} PbVideoModule;

#define PB_VIDEO_MODULE_ABI 3u
#define PB_VIDEO_MODULE_FILE "libpb_mapper_desktop_video.so"
#define PB_VIDEO_MODULE_SYMBOL "pb_mapper_desktop_video_module_v1"
typedef const PbVideoModule* (*PbVideoModuleEntry)(void);

#ifdef __cplusplus
}
#endif

#endif  // RUNNER_DESKTOP_VIDEO_MODULE_H_
