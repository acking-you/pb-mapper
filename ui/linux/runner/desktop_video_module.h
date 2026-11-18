// The interface between the runner and libpb_mapper_desktop_video.so, the
// GStreamer decoder module the runner loads with dlopen (desktop_video.cc).
// The module is optional: built only where the GStreamer development files
// are installed, and loadable only where GStreamer is. Without it the
// runner's `pb_mapper/desktop_video` channel answers "no decoders" and every
// session uses JPEG.
//
// Both sides are built together by ui/linux/CMakeLists.txt; the ABI number
// still guards a stale module left in a bundle.
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
// (0 x 0: none), whether its decoder runs on hardware, and its element.
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
} PbVideoStats;

// A decoded picture: tightly packed RGBA, width x 4 bytes a row. Counted;
// it outlives the decoder that made it.
typedef struct PbVideoPicture PbVideoPicture;
typedef struct PbVideoDecoder PbVideoDecoder;

typedef struct PbVideoModule {
  uint32_t abi;
  // Reads the GStreamer registry and builds nothing. Blocks while GStreamer
  // initializes (a first run scans its plugins), so never on the GTK
  // thread. 0, or -1 with `error` set when GStreamer cannot start.
  int32_t (*probe)(PbVideoProbe* out, char* error, size_t error_len);
  // `frame_ready` runs on a GStreamer thread whenever a newer picture can be
  // acquired. Nothing here touches GStreamer yet.
  PbVideoDecoder* (*create)(const PbVideoExports* exports,
                            void (*frame_ready)(void* context),
                            void* context);
  // Binds a source, replacing the previous one, and starts the decoder
  // thread. The consumer id (> 0), or the code `attach_v1` returned.
  int64_t (*attach)(PbVideoDecoder* decoder, uint64_t source);
  // Stops the decoder thread; detaching wakes a blocked pull, so it is
  // prompt. The last picture stays.
  void (*detach)(PbVideoDecoder* decoder);
  // The compositor's call: the newest picture, retained for the caller, or
  // null before the first. The first acquisition of a current picture is
  // its presentation receipt to Rust. Non-blocking.
  PbVideoPicture* (*acquire)(PbVideoDecoder* decoder);
  void (*stats)(PbVideoDecoder* decoder, PbVideoStats* out);
  // Detaches and frees the decoder; pictures still retained stay valid.
  void (*destroy)(PbVideoDecoder* decoder);
  const uint8_t* (*pixels)(const PbVideoPicture* picture, uint32_t* width,
                           uint32_t* height);
  PbVideoPicture* (*retain)(PbVideoPicture* picture);
  void (*release)(PbVideoPicture* picture);
} PbVideoModule;

#define PB_VIDEO_MODULE_ABI 1u
#define PB_VIDEO_MODULE_FILE "libpb_mapper_desktop_video.so"
#define PB_VIDEO_MODULE_SYMBOL "pb_mapper_desktop_video_module_v1"
typedef const PbVideoModule* (*PbVideoModuleEntry)(void);

#ifdef __cplusplus
}
#endif

#endif  // RUNNER_DESKTOP_VIDEO_MODULE_H_
