// libpb_mapper_desktop_video.so: native H.264 and H.265 decoding for the
// Linux viewer, through GStreamer. The runner loads it with dlopen
// (desktop_video.cc), so the app starts where GStreamer is missing.
//
// One decoder thread per attached source pulls access units straight from
// Rust into pooled GStreamer buffers and feeds an explicit pipeline,
//   appsrc -> h264parse | h265parse -> decoder -> appsink,
// whose streaming thread publishes the newest picture as the decoder made
// it: NV12 or I420 in system memory, or NV12 in dma-bufs where the
// presenter samples them and GStreamer (1.24 on) says their layout. A
// decoder that gives neither gets videoconvert to NV12. The compositor's
// raster thread acquires the picture, which is the presentation receipt,
// and converts it to RGBA on the GPU (desktop_video_gl.cc). The decoder
// state rules are the
// Windows runner's (ui/windows/runner/desktop_video.cpp) and the Android
// feeder's (VideoFeeder.kt): a generation change or a discontinuity starts a
// revision that waits for a keyframe, a codec or size change rebuilds the
// pipeline at a keyframe, and pictures of an older revision earn no receipt.
#define G_LOG_DOMAIN "pb-mapper-video"

#include "desktop_video_module.h"

#include <gst/allocators/allocators.h>
#include <gst/app/gstappsink.h>
#include <gst/app/gstappsrc.h>
#include <gst/gst.h>
#include <gst/video/video.h>
#include <pthread.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cinttypes>
#include <condition_variable>
#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <set>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

static_assert(sizeof(PbDesktopVideoMetaV1) == 56, "PbDesktopVideoMetaV1 is 56 bytes");
static_assert(offsetof(PbDesktopVideoMetaV1, generation) == 0 && offsetof(PbDesktopVideoMetaV1, frame_id) == 8 &&
                  offsetof(PbDesktopVideoMetaV1, seq) == 16 && offsetof(PbDesktopVideoMetaV1, pts_us) == 24 &&
                  offsetof(PbDesktopVideoMetaV1, age_us) == 32 && offsetof(PbDesktopVideoMetaV1, width) == 40 &&
                  offsetof(PbDesktopVideoMetaV1, height) == 44 && offsetof(PbDesktopVideoMetaV1, len) == 48 &&
                  offsetof(PbDesktopVideoMetaV1, flags) == 52,
              "PbDesktopVideoMetaV1 matches desktop_video.rs");

namespace {
using Clock = std::chrono::steady_clock;

constexpr int32_t kVideoUnit = 1;
constexpr int32_t kVideoTimeout = 0;
constexpr int32_t kVideoNeedCapacity = -2;
constexpr int32_t kVideoPaused = -3;
constexpr int32_t kVideoInvalid = -5;
constexpr uint32_t kFlagKeyframe = 1;
constexpr uint32_t kFlagDiscontinuity = 2;
// A unit with neither this nor the JPEG bit (Dart decodes JPEG) is H.264.
constexpr uint32_t kFlagH265 = 1u << 4;
constexpr uint32_t kReasonGap = 1;
constexpr uint32_t kReasonStall = 3;
constexpr uint32_t kPullTimeoutMs = 50;
// The wire's largest payload, and the first buffer size.
constexpr uint32_t kMaxBytes = 4 * 1024 * 1024;
constexpr uint32_t kFirstCapacity = 1024 * 1024;
// The largest picture a session allows (VideoLimits): nothing larger
// reaches a decoder.
constexpr uint32_t kMaxWidth = 3840, kMaxHeight = 2160;
// The bounds a viewer may ask for (Resolution::bounds in lib.rs), largest
// first: the probe reports the largest one a decoder may take.
constexpr uint32_t kBounds[][2] = {{3840, 2160}, {2560, 1440}, {1920, 1080}, {1280, 720}};
// The largest picture a software decoder may take: decoding and converting
// on the CPU, 1080p is the size every session asked for before decoders
// were probed.
constexpr uint32_t kSoftwareWidth = 1920, kSoftwareHeight = 1080;
// Units inside the pipeline and not yet decoded. The decoder thread pulls
// the next one only below this, so a backlog builds in Rust's sink, which
// drops it to the next keyframe, and never here.
constexpr size_t kMaxInFlight = 4;
// A pipeline holding units this long without a picture is rebuilt.
constexpr auto kStall = std::chrono::seconds(2);
// Unique, increasing timestamps that identify each unit's picture; the
// network's are never replayed.
constexpr GstClockTime kFrameDuration = GST_SECOND / 60;
// videoconvert's threads, for a decoder that gives no NV12 or I420.
constexpr uint32_t kMaxConvertThreads = 4;
constexpr char kDmabufFeature[] = "memory:DMABuf";
constexpr uint64_t kModifierLinear = 0;

enum class Codec { kH264, kH265 };
const char* CodecName(Codec codec) { return codec == Codec::kH265 ? "H.265" : "H.264"; }
const char* MediaType(Codec codec) { return codec == Codec::kH265 ? "video/x-h265" : "video/x-h264"; }
const char* Parser(Codec codec) { return codec == Codec::kH265 ? "h265parse" : "h264parse"; }

void CopyText(char* dst, size_t capacity, const std::string& text) {
  if (!dst || capacity == 0) return;
  size_t len = std::min(text.size(), capacity - 1);
  // Never split a UTF-8 sequence.
  while (len > 0 && len < text.size() && (static_cast<unsigned char>(text[len]) & 0xC0) == 0x80) --len;
  memcpy(dst, text.data(), len);
  dst[len] = '\0';
}

// GStreamer starts once per process and stays: pictures and its threads
// may outlive any viewer, so the module is never unloaded either.
std::once_flag g_init_once;
std::atomic<bool> g_ready{false};
std::string g_init_error;

bool InitGStreamer(std::string* error) {
  std::call_once(g_init_once, [] {
    GError* failure = nullptr;
    if (gst_init_check(nullptr, nullptr, &failure)) {
      g_ready = true;
    } else {
      g_init_error = failure ? failure->message : "GStreamer did not initialize";
      g_clear_error(&failure);
    }
  });
  if (!g_ready && error) *error = g_init_error;
  return g_ready;
}

// DMA_DRM caps, which give a dma-buf's format and DRM modifier, are
// GStreamer 1.24's. Before them a decoder's dma-buf says nothing of its
// tiling, so none is asked for.
bool NegotiatesDmabuf() {
  static const bool negotiates = [] {
    guint major = 0, minor = 0, micro = 0, nano = 0;
    gst_version(&major, &minor, &micro, &nano);
    return major > 1 || (major == 1 && minor >= 24);
  }();
  return negotiates;
}

bool HasElement(const char* name) {
  GstElementFactory* factory = gst_element_factory_find(name);
  if (!factory) return false;
  gst_object_unref(factory);
  return true;
}

// The largest value a caps field allows; UINT32_MAX when it sets no bound.
uint32_t Largest(const GValue* value) {
  if (!value) return UINT32_MAX;
  if (G_VALUE_HOLDS_INT(value)) return static_cast<uint32_t>(std::max(0, g_value_get_int(value)));
  if (GST_VALUE_HOLDS_INT_RANGE(value)) return static_cast<uint32_t>(std::max(0, gst_value_get_int_range_max(value)));
  if (GST_VALUE_HOLDS_LIST(value)) {
    uint32_t largest = 0;
    for (guint i = 0; i < gst_value_list_get_size(value); ++i) {
      largest = std::max(largest, Largest(gst_value_list_get_value(value, i)));
    }
    return largest;
  }
  return UINT32_MAX;
}

// "vah264dec (VA-API H.264 Decoder in Intel(R) Gen Graphics)", which names
// the device, or the element alone when that is longer than the 63 bytes
// report_v1 takes.
std::string DecoderName(GstElementFactory* factory) {
  const std::string element = GST_OBJECT_NAME(factory);
  const char* longname = gst_element_factory_get_metadata(factory, GST_ELEMENT_METADATA_LONGNAME);
  const std::string name = longname && *longname ? element + " (" + longname + ")" : element;
  return name.size() <= 63 ? name : element.substr(0, 63);
}

// A decoder the registry has for a codec.
struct Candidate {
  std::string factory;
  std::string name;
  bool hardware = false;
  guint rank = 0;
  // The largest picture its sink template takes.
  uint32_t max_width = UINT32_MAX, max_height = UINT32_MAX;
};

// Whether the decoder gives pictures in system memory, or in dma-bufs when
// `dmabuf` (the presenter samples them).
bool OffersMemory(GstElementFactory* factory, bool dmabuf) {
  for (const GList* item = gst_element_factory_get_static_pad_templates(factory); item; item = item->next) {
    auto* templ = static_cast<GstStaticPadTemplate*>(item->data);
    if (templ->direction != GST_PAD_SRC) continue;
    GstCaps* caps = gst_static_pad_template_get_caps(templ);
    bool found = false;
    for (guint i = 0; !found && i < gst_caps_get_size(caps); ++i) {
      GstCapsFeatures* features = gst_caps_get_features(caps, i);
      found = !features || gst_caps_features_is_any(features) ||
              gst_caps_features_contains(features, GST_CAPS_FEATURE_MEMORY_SYSTEM_MEMORY) ||
              (dmabuf && gst_caps_features_contains(features, kDmabufFeature));
    }
    gst_caps_unref(caps);
    if (found) return true;
  }
  return false;
}

// Every decoder the registry has for `codec` that this pipeline can use:
// it takes the codec, gives pictures in system memory or, when `dmabuf`,
// in dma-bufs, and has a rank (GST_PLUGIN_FEATURE_RANK=name:NONE takes one
// out). Hardware first, then by rank, as GStreamer would pick them.
std::vector<Candidate> Candidates(Codec codec, bool dmabuf) {
  std::vector<Candidate> found;
  GList* factories = gst_element_factory_list_get_elements(
      GST_ELEMENT_FACTORY_TYPE_DECODER | GST_ELEMENT_FACTORY_TYPE_MEDIA_VIDEO, GST_RANK_MARGINAL);
  for (GList* item = factories; item; item = item->next) {
    auto* factory = static_cast<GstElementFactory*>(item->data);
    Candidate candidate;
    bool takes = false;
    for (const GList* pad = gst_element_factory_get_static_pad_templates(factory); pad; pad = pad->next) {
      auto* templ = static_cast<GstStaticPadTemplate*>(pad->data);
      if (templ->direction != GST_PAD_SINK) continue;
      GstCaps* caps = gst_static_pad_template_get_caps(templ);
      for (guint i = 0; i < gst_caps_get_size(caps); ++i) {
        const GstStructure* s = gst_caps_get_structure(caps, i);
        if (!gst_structure_has_name(s, MediaType(codec))) continue;
        const uint32_t width = Largest(gst_structure_get_value(s, "width"));
        const uint32_t height = Largest(gst_structure_get_value(s, "height"));
        candidate.max_width = takes ? std::max(candidate.max_width, width) : width;
        candidate.max_height = takes ? std::max(candidate.max_height, height) : height;
        takes = true;
      }
      gst_caps_unref(caps);
    }
    if (!takes || !OffersMemory(factory, dmabuf)) continue;
    const char* klass = gst_element_factory_get_metadata(factory, GST_ELEMENT_METADATA_KLASS);
    candidate.factory = GST_OBJECT_NAME(factory);
    candidate.name = DecoderName(factory);
    candidate.hardware = klass && strstr(klass, "Hardware");
    candidate.rank = gst_plugin_feature_get_rank(GST_PLUGIN_FEATURE(factory));
    found.push_back(std::move(candidate));
  }
  gst_plugin_feature_list_free(factories);
  std::stable_sort(found.begin(), found.end(), [](const Candidate& a, const Candidate& b) {
    return a.hardware != b.hardware ? a.hardware : a.rank > b.rank;
  });
  return found;
}

// Decoders that failed before their first picture: the next configuration
// takes the next candidate, and a broken hardware decoder costs one
// failure per run, not one per keyframe.
std::mutex g_failed_mutex;
std::set<std::string> g_failed;

bool Failed(const std::string& factory) {
  std::lock_guard<std::mutex> lock(g_failed_mutex);
  return g_failed.count(factory) != 0;
}
void MarkFailed(const std::string& factory) {
  std::lock_guard<std::mutex> lock(g_failed_mutex);
  if (g_failed.insert(factory).second) g_warning("%s failed before its first picture; it is not used again", factory.c_str());
}

// Whether `candidate` may decode `codec` at `width` x `height`: the one rule
// the probe advertises and every pipeline keeps, so a session never runs on
// a decoder the probe would not have offered for it. A hardware decoder
// takes what its sink template takes. A software one takes H.264 only, up
// to 1080p in either orientation: H.265 in software only costs more than
// H.264, as on Windows and Android, which offer it on hardware only.
bool Allows(const Candidate& candidate, Codec codec, uint32_t width, uint32_t height) {
  if (width > candidate.max_width || height > candidate.max_height) return false;
  if (candidate.hardware) return true;
  return codec == Codec::kH264 && std::max(width, height) <= kSoftwareWidth &&
         std::min(width, height) <= kSoftwareHeight;
}

// The probe's answer for one codec: the first candidate that has not failed
// and is allowed a viewer bound, with the largest bound it is allowed.
void ProbeCodec(Codec codec, PbVideoCodecProbe* out) {
  if (!HasElement(Parser(codec))) return;
  for (const Candidate& candidate : Candidates(codec, NegotiatesDmabuf())) {
    if (Failed(candidate.factory)) continue;
    for (const auto& bounds : kBounds) {
      if (!Allows(candidate, codec, bounds[0], bounds[1])) continue;
      out->max_width = bounds[0];
      out->max_height = bounds[1];
      out->hardware = candidate.hardware ? 1 : 0;
      CopyText(out->decoder, sizeof(out->decoder), candidate.name);
      return;
    }
  }
}

int32_t Probe(PbVideoProbe* out, char* error, size_t error_len) {
  if (!out) return -1;
  *out = PbVideoProbe{};
  std::string why;
  if (!InitGStreamer(&why)) {
    CopyText(error, error_len, why);
    return -1;
  }
  if (!HasElement("appsrc") || !HasElement("appsink") || !HasElement("videoconvert")) {
    CopyText(error, error_len, "GStreamer's base plugins (appsrc, appsink, videoconvert) are missing");
    return 0;
  }
  ProbeCodec(Codec::kH264, &out->h264);
  ProbeCodec(Codec::kH265, &out->h265);
  return 0;
}

void SetIfPresent(GstElement* element, const char* property, const char* value) {
  if (g_object_class_find_property(G_OBJECT_GET_CLASS(element), property)) {
    gst_util_set_object_arg(G_OBJECT(element), property, value);
  }
}

// A stream that does not say its colorimetry is the desktop encoders'
// BT.709 limited range (the Windows runner converts with it too), not the
// default GStreamer guesses from the size (BT.2020 at 2160 rows).
GstPadProbeReturn DefaultColorimetry(GstPad*, GstPadProbeInfo* info, gpointer) {
  GstEvent* event = GST_PAD_PROBE_INFO_EVENT(info);
  if (GST_EVENT_TYPE(event) != GST_EVENT_CAPS) return GST_PAD_PROBE_OK;
  GstCaps* caps = nullptr;
  gst_event_parse_caps(event, &caps);
  if (!caps || gst_caps_get_size(caps) == 0 ||
      gst_structure_has_field(gst_caps_get_structure(caps, 0), "colorimetry")) {
    return GST_PAD_PROBE_OK;
  }
  GstCaps* stated = gst_caps_copy(caps);
  gst_caps_set_simple(stated, "colorimetry", G_TYPE_STRING, "bt709", nullptr);
  GST_PAD_PROBE_INFO_DATA(info) = gst_event_new_caps(stated);
  gst_caps_unref(stated);
  gst_event_unref(event);
  return GST_PAD_PROBE_OK;
}

// The decoders lay pictures out as they keep them (padded rows, a 1088-row
// surface) and say so with GstVideoMeta, which appsink alone does not ask
// for: without it a decoder copies each picture into a plain layout, and a
// dma-buf picture is refused. Pictures are the decoder's own buffers, and
// the decoder is told how many stay out of its pool: the newest, the one
// on screen and one the GPU may still be reading.
GstPadProbeReturn AnswerAllocation(GstPad*, GstPadProbeInfo* info, gpointer) {
  constexpr guint kHeldPictures = 3;
  GstQuery* query = GST_PAD_PROBE_INFO_QUERY(info);
  if (GST_QUERY_TYPE(query) != GST_QUERY_ALLOCATION) return GST_PAD_PROBE_OK;
  if (!gst_query_find_allocation_meta(query, GST_VIDEO_META_API_TYPE, nullptr)) {
    gst_query_add_allocation_meta(query, GST_VIDEO_META_API_TYPE, nullptr);
  }
  if (gst_query_get_n_allocation_pools(query) == 0) {
    GstCaps* caps = nullptr;
    gboolean need_pool = FALSE;
    gst_query_parse_allocation(query, &caps, &need_pool);
    GstVideoInfo picture;
    gst_video_info_init(&picture);
    // A dma-buf's size is its decoder's to know.
    const guint size = caps && gst_video_info_from_caps(&picture, caps) ? static_cast<guint>(picture.size) : 0;
    gst_query_add_allocation_pool(query, nullptr, size, kHeldPictures, 0);
  }
  return GST_PAD_PROBE_OK;
}

// What the presenter takes as it is: NV12 dma-bufs of the layouts it
// imports (`modifiers`, none when it imports none), then NV12 or I420 in
// system memory. Each layout is written as GStreamer writes it, so the
// strings match: the fourcc alone when linear.
GstCaps* Wanted(const std::vector<uint64_t>& modifiers) {
  GstCaps* caps = gst_caps_new_empty();
  if (!modifiers.empty()) {
    std::string text = "video/x-raw(memory:DMABuf),format=(string)DMA_DRM,drm-format=(string){";
    for (size_t i = 0; i < modifiers.size(); ++i) {
      char format[40];
      if (modifiers[i] == kModifierLinear) {
        snprintf(format, sizeof(format), "\"NV12\"");
      } else {
        snprintf(format, sizeof(format), "\"NV12:0x%016" PRIx64 "\"", modifiers[i]);
      }
      text += i == 0 ? "" : ",";
      text += format;
    }
    text += "}";
    if (GstCaps* dmabuf = gst_caps_from_string(text.c_str())) gst_caps_append(caps, dmabuf);
  }
  if (GstCaps* system = gst_caps_from_string("video/x-raw,format=(string){NV12,I420}")) gst_caps_append(caps, system);
  return caps;
}

// Whether `decoder` can give what `wanted` takes, with nothing in between.
bool Gives(GstElement* decoder, const GstCaps* wanted) {
  GstElementFactory* factory = gst_element_get_factory(decoder);
  if (!factory) return false;
  for (const GList* item = gst_element_factory_get_static_pad_templates(factory); item; item = item->next) {
    auto* templ = static_cast<GstStaticPadTemplate*>(item->data);
    if (templ->direction != GST_PAD_SRC) continue;
    GstCaps* caps = gst_static_pad_template_get_caps(templ);
    const bool gives = gst_caps_can_intersect(caps, wanted);
    gst_caps_unref(caps);
    if (gives) return true;
  }
  return false;
}

// What a caps change says of the pictures that follow.
struct Layout {
  bool dmabuf = false;
  // System memory's: what maps the pictures.
  GstVideoInfo info{};
  uint32_t width = 0, height = 0, format = 0;
  uint64_t modifier = kModifierLinear;
  // A stream that names no matrix is BT.709 limited (DefaultColorimetry).
  float kr = 0.2126f, kb = 0.0722f;
  bool full_range = false;
};

void ReadColour(const GstVideoColorimetry& colorimetry, Layout* layout) {
  gdouble kr = 0, kb = 0;
  if (gst_video_color_matrix_get_Kr_Kb(colorimetry.matrix, &kr, &kb)) {
    layout->kr = static_cast<float>(kr);
    layout->kb = static_cast<float>(kb);
  }
  layout->full_range = colorimetry.range == GST_VIDEO_COLOR_RANGE_0_255;
}

// GStreamer 1.24's drm-format for NV12: "NV12" when linear, or
// "NV12:0x<modifier>".
bool ReadDrmFormat(const char* text, uint64_t* modifier) {
  if (!text || strncmp(text, "NV12", 4) != 0) return false;
  if (text[4] == '\0') {
    *modifier = kModifierLinear;
    return true;
  }
  if (text[4] != ':' || text[5] != '0' || (text[6] != 'x' && text[6] != 'X')) return false;
  char* end = nullptr;
  const guint64 value = g_ascii_strtoull(text + 7, &end, 16);
  if (!end || end == text + 7 || *end != '\0') return false;
  *modifier = value;
  return true;
}

bool ReadLayout(GstCaps* caps, Layout* out) {
  *out = Layout{};
  if (gst_caps_get_size(caps) == 0) return false;
  GstCapsFeatures* features = gst_caps_get_features(caps, 0);
  const GstStructure* structure = gst_caps_get_structure(caps, 0);
  if (features && gst_caps_features_contains(features, kDmabufFeature)) {
    // Read by hand: the GStreamer this was built against may predate
    // DMA_DRM.
    gint width = 0, height = 0;
    if (g_strcmp0(gst_structure_get_string(structure, "format"), "DMA_DRM") != 0 ||
        !gst_structure_get_int(structure, "width", &width) || !gst_structure_get_int(structure, "height", &height) ||
        width <= 0 || height <= 0 || !ReadDrmFormat(gst_structure_get_string(structure, "drm-format"), &out->modifier)) {
      return false;
    }
    GstVideoColorimetry colorimetry{};
    const char* named = gst_structure_get_string(structure, "colorimetry");
    if (named && gst_video_colorimetry_from_string(&colorimetry, named)) ReadColour(colorimetry, out);
    out->dmabuf = true;
    out->width = static_cast<uint32_t>(width);
    out->height = static_cast<uint32_t>(height);
    out->format = PB_VIDEO_FORMAT_NV12;
    return true;
  }
  if (!gst_video_info_from_caps(&out->info, caps)) return false;
  switch (GST_VIDEO_INFO_FORMAT(&out->info)) {
    case GST_VIDEO_FORMAT_NV12:
      out->format = PB_VIDEO_FORMAT_NV12;
      break;
    case GST_VIDEO_FORMAT_I420:
      out->format = PB_VIDEO_FORMAT_I420;
      break;
    default:
      return false;
  }
  out->width = static_cast<uint32_t>(GST_VIDEO_INFO_WIDTH(&out->info));
  out->height = static_cast<uint32_t>(GST_VIDEO_INFO_HEIGHT(&out->info));
  ReadColour(out->info.colorimetry, out);
  return true;
}

// Pipelines stopping on threads of their own (Pipeline::Retire).
std::mutex g_retire_mutex;
std::condition_variable g_retired;
int g_retiring = 0;
void WaitRetired() {
  std::unique_lock<std::mutex> lock(g_retire_mutex);
  g_retired.wait_for(lock, std::chrono::seconds(2), [] { return g_retiring == 0; });
}

struct Frame {
  int64_t generation = 0, frame = 0;
  uint32_t width = 0, height = 0;
  uint64_t revision = 0;
  bool key = false;
  Codec codec = Codec::kH264;
  // When it entered the pipeline.
  Clock::time_point fed{};
};

// One of the stats' timings: count, total and longest, in constant space,
// as the other runners keep theirs.
struct Timing {
  uint64_t samples = 0;
  Clock::duration total{}, longest{};
  void Add(Clock::duration elapsed) {
    ++samples;
    total += elapsed;
    longest = std::max(longest, elapsed);
  }
  PbVideoTiming Snapshot() const {
    using std::chrono::duration_cast;
    using std::chrono::microseconds;
    PbVideoTiming out{};
    out.samples = samples;
    out.total_us = static_cast<uint64_t>(duration_cast<microseconds>(total).count());
    out.max_us = static_cast<uint64_t>(duration_cast<microseconds>(longest).count());
    return out;
  }
};

// Compressed units are pulled straight into these buffers; a buffer the
// pipeline releases comes back for the next unit.
class InputPool {
 public:
  InputPool() = default;
  InputPool(const InputPool&) = delete;
  InputPool& operator=(const InputPool&) = delete;
  ~InputPool() { Reset(); }
  // A buffer of `capacity()` bytes, or null.
  GstBuffer* Take() {
    if (!pool_) {
      pool_ = gst_buffer_pool_new();
      GstStructure* config = gst_buffer_pool_get_config(pool_);
      gst_buffer_pool_config_set_params(config, nullptr, capacity_, 2, 0);
      if (!gst_buffer_pool_set_config(pool_, config) || !gst_buffer_pool_set_active(pool_, TRUE)) {
        Reset();
        return nullptr;
      }
    }
    GstBuffer* buffer = nullptr;
    return gst_buffer_pool_acquire_buffer(pool_, &buffer, nullptr) == GST_FLOW_OK ? buffer : nullptr;
  }
  // Buffers out of the old pool are freed as the pipeline releases them.
  void Grow(uint32_t capacity) {
    capacity_ = capacity;
    Reset();
  }
  uint32_t capacity() const { return capacity_; }

 private:
  void Reset() {
    if (!pool_) return;
    gst_buffer_pool_set_active(pool_, FALSE);
    gst_object_unref(pool_);
    pool_ = nullptr;
  }
  GstBufferPool* pool_ = nullptr;
  uint32_t capacity_ = kFirstCapacity;
};

}  // namespace

// A decoded picture: the decoder's own buffer, out of its pool until the
// last reference goes. In system memory it stays mapped meanwhile; a
// dma-buf one is never mapped.
struct PbVideoPicture {
  std::atomic<int> refs{1};
  GstVideoFrame frame{};
  bool mapped = false;
  // A dma-buf picture's.
  GstBuffer* buffer = nullptr;
  PbVideoPlanes planes{};
  int64_t generation = 0, frame_id = 0;
  uint64_t revision = 0;
  ~PbVideoPicture() {
    if (mapped) gst_video_frame_unmap(&frame);
    if (buffer) gst_buffer_unref(buffer);
  }
};

namespace {
PbVideoPicture* Retain(PbVideoPicture* picture) {
  if (picture) picture->refs.fetch_add(1, std::memory_order_relaxed);
  return picture;
}
void Release(PbVideoPicture* picture) {
  if (picture && picture->refs.fetch_sub(1, std::memory_order_acq_rel) == 1) delete picture;
}
}  // namespace

struct PbVideoDecoder;

namespace {
// One configuration: a codec at a size, on one decoder element.
class Pipeline {
 public:
  // The first allowed candidate that starts, or throws: the caller reports
  // the failure, and repeated failures end the session as decoder_failed,
  // after which Dart connects again without H.265.
  // `modifiers`: the dma-buf layouts the presenter samples, none for
  // system memory only.
  static std::unique_ptr<Pipeline> Start(const Frame& frame, PbVideoDecoder* owner,
                                         const std::vector<uint64_t>& modifiers) {
    std::string last;
    bool any = false;
    if (!HasElement(Parser(frame.codec))) throw std::runtime_error(std::string("GStreamer has no ") + Parser(frame.codec));
    for (Candidate& candidate : Candidates(frame.codec, !modifiers.empty())) {
      if (Failed(candidate.factory) || !Allows(candidate, frame.codec, frame.width, frame.height)) continue;
      any = true;
      std::unique_ptr<Pipeline> pipeline(new Pipeline(frame, std::move(candidate), owner, modifiers));
      if (pipeline->Build(&last)) return pipeline;
      MarkFailed(pipeline->decoder().factory);
    }
    if (!any) throw std::runtime_error(std::string("No working GStreamer ") + CodecName(frame.codec) + " decoder for " +
                                       std::to_string(frame.width) + "x" + std::to_string(frame.height));
    throw std::runtime_error(std::string("No GStreamer ") + CodecName(frame.codec) + " decoder starts: " + last);
  }
  Pipeline(const Pipeline&) = delete;
  Pipeline& operator=(const Pipeline&) = delete;
  // Stopping joins the streaming thread: no callback runs afterwards.
  ~Pipeline() {
    if (pipeline_) {
      gst_element_set_state(pipeline_, GST_STATE_NULL);
      gst_object_unref(pipeline_);
    }
    if (bus_) gst_object_unref(bus_);
    if (decoded_) gst_object_unref(decoded_);
    if (caps_) gst_caps_unref(caps_);
  }

  const Candidate& decoder() const { return decoder_; }
  Codec codec() const { return codec_; }
  // Whether videoconvert turns the decoder's pictures into NV12.
  bool converts() const { return converts_; }
  // Whether it asked for dma-bufs.
  bool dmabuf() const { return dmabuf_; }
  uint32_t width() const { return width_; }
  uint32_t height() const { return height_; }

  // Decoder thread. Waits up to `timeout_ms` for fewer than kMaxInFlight
  // units in the pipeline; false at once when the pipeline failed.
  bool WaitForRoom(uint32_t timeout_ms) {
    std::unique_lock<std::mutex> lock(mutex_);
    room_.wait_for(lock, std::chrono::milliseconds(timeout_ms),
                   [this] { return pending_.size() < kMaxInFlight || !error_.empty(); });
    return pending_.size() < kMaxInFlight && error_.empty();
  }

  // Decoder thread. Takes `buffer`.
  void Feed(const Frame& frame, GstBuffer* buffer) {
    next_pts_ += kFrameDuration;
    GST_BUFFER_PTS(buffer) = next_pts_;
    GST_BUFFER_DTS(buffer) = next_pts_;
    GST_BUFFER_DURATION(buffer) = kFrameDuration;
    if (frame.key) {
      GST_BUFFER_FLAG_UNSET(buffer, GST_BUFFER_FLAG_DELTA_UNIT);
    } else {
      GST_BUFFER_FLAG_SET(buffer, GST_BUFFER_FLAG_DELTA_UNIT);
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      const Clock::time_point now = Clock::now();
      if (pending_.empty()) progress_ = now;
      Frame fed = frame;
      fed.fed = now;
      pending_.emplace(next_pts_, fed);
    }
    const GstFlowReturn flow = gst_app_src_push_buffer(src_, buffer);
    if (flow != GST_FLOW_OK) throw std::runtime_error(std::string("Decode video input (") + gst_flow_get_name(flow) + ")");
  }

  // Decoder thread. The first error the bus or the streaming thread saw,
  // or empty.
  std::string TakeError() {
    GstMessage* message = gst_bus_pop_filtered(bus_, GST_MESSAGE_ERROR);
    if (message) {
      GError* error = nullptr;
      gchar* debug = nullptr;
      gst_message_parse_error(message, &error, &debug);
      std::string text = std::string(GST_OBJECT_NAME(GST_MESSAGE_SRC(message))) + ": " +
                         (error ? error->message : "error");
      g_warning("GStreamer %s (%s)", text.c_str(), debug ? debug : "");
      g_clear_error(&error);
      g_free(debug);
      gst_message_unref(message);
      std::lock_guard<std::mutex> lock(mutex_);
      if (error_.empty()) error_ = text;
    }
    std::lock_guard<std::mutex> lock(mutex_);
    return error_;
  }

  // Decoder thread: units wait and nothing came out since `since`.
  bool Stalled(Clock::time_point since) {
    std::lock_guard<std::mutex> lock(mutex_);
    return !pending_.empty() && Clock::now() - std::max(progress_, since) > kStall;
  }

  bool decoded_any() {
    std::lock_guard<std::mutex> lock(mutex_);
    return decoded_any_;
  }

  // Decoder thread, at its exit: the pipeline stops on a thread of its own,
  // since closing a device can take a while and the GTK thread waits for
  // the decoder thread. A picture decoded meanwhile is dropped, never
  // handed to a decoder that may be gone.
  static void Retire(std::unique_ptr<Pipeline> pipeline) {
    if (!pipeline) return;
    {
      std::lock_guard<std::mutex> lock(pipeline->owner_mutex_);
      pipeline->owner_ = nullptr;
    }
    {
      std::lock_guard<std::mutex> lock(g_retire_mutex);
      // The process waits for them at exit, before the drivers' own
      // exit handlers (registered earlier, so run later) unload anything.
      static const bool waits = std::atexit(WaitRetired) == 0;
      (void)waits;
      ++g_retiring;
    }
    try {
      std::thread(
          [](Pipeline* doomed) {
            delete doomed;
            {
              std::lock_guard<std::mutex> lock(g_retire_mutex);
              --g_retiring;
            }
            g_retired.notify_all();
          },
          pipeline.get())
          .detach();
      pipeline.release();
    } catch (const std::exception&) {
      // No thread: it stops here instead.
      pipeline.reset();
      std::lock_guard<std::mutex> lock(g_retire_mutex);
      --g_retiring;
    }
  }

 private:
  Pipeline(const Frame& frame, Candidate decoder, PbVideoDecoder* owner, const std::vector<uint64_t>& modifiers)
      : decoder_(std::move(decoder)),
        codec_(frame.codec),
        width_(frame.width),
        height_(frame.height),
        modifiers_(modifiers),
        owner_(owner) {}

  bool Build(std::string* why) {
    pipeline_ = gst_pipeline_new("pb-desktop-video");
    GstElement* src = gst_element_factory_make("appsrc", nullptr);
    GstElement* parse = gst_element_factory_make(Parser(codec_), nullptr);
    GstElement* decode = gst_element_factory_make(decoder_.factory.c_str(), nullptr);
    GstElement* sink = gst_element_factory_make("appsink", nullptr);
    GstCaps* wanted = Wanted(modifiers_);
    // A decoder that cannot give what the presenter takes gets
    // videoconvert to NV12, in system memory.
    converts_ = decode && !Gives(decode, wanted);
    GstElement* convert = converts_ ? gst_element_factory_make("videoconvert", nullptr) : nullptr;
    if (!pipeline_ || !src || !parse || !decode || !sink || (converts_ && !convert)) {
      for (GstElement* element : {src, parse, decode, convert, sink}) {
        if (element) gst_object_unref(gst_object_ref_sink(element));
      }
      gst_caps_unref(wanted);
      *why = "cannot create " + decoder_.factory;
      return false;
    }
    dmabuf_ = !converts_ && !modifiers_.empty();
    gst_bin_add_many(GST_BIN(pipeline_), src, parse, decode, sink, nullptr);
    if (convert) gst_bin_add(GST_BIN(pipeline_), convert);
    src_ = GST_APP_SRC(src);

    GstCaps* input = gst_caps_new_simple(MediaType(codec_), "stream-format", G_TYPE_STRING, "byte-stream",
                                         "alignment", G_TYPE_STRING, "au", nullptr);
    // Live: decoders hold nothing back for playback smoothness. The queue
    // is bounded by kMaxInFlight, not by appsrc.
    g_object_set(src, "caps", input, "format", GST_FORMAT_TIME, "is-live", TRUE, "do-timestamp", FALSE, "block", FALSE,
                 "max-bytes", static_cast<guint64>(0), "emit-signals", FALSE, nullptr);
    gst_caps_unref(input);
    // Low latency where the element has a say: slice threads only in
    // libav (frame threads delay each picture by a frame per thread), no
    // display delay in NVDEC, immediate output in the legacy VA-API plugin.
    SetIfPresent(decode, "thread-type", "slice");
    SetIfPresent(decode, "max-display-delay", "0");
    SetIfPresent(decode, "low-latency", "true");
    if (convert) {
      SetIfPresent(convert, "n-threads", std::to_string(std::min(kMaxConvertThreads, g_get_num_processors())).c_str());
    }
    GstCaps* output = converts_ ? gst_caps_from_string("video/x-raw,format=(string)NV12") : gst_caps_ref(wanted);
    gst_caps_unref(wanted);
    g_object_set(sink, "caps", output, "sync", FALSE, "max-buffers", 1u, "drop", FALSE, "enable-last-sample", FALSE,
                 "qos", FALSE, "emit-signals", FALSE, nullptr);
    if (output) gst_caps_unref(output);
    GstAppSinkCallbacks callbacks{};
    callbacks.new_sample = &Pipeline::OnSample;
    gst_app_sink_set_callbacks(GST_APP_SINK(sink), &callbacks, this, nullptr);

    const bool linked = convert ? gst_element_link_many(src, parse, decode, convert, sink, nullptr)
                                : gst_element_link_many(src, parse, decode, sink, nullptr);
    if (!linked) {
      *why = "cannot link " + decoder_.factory;
      return false;
    }
    GstPad* sunk = gst_element_get_static_pad(sink, "sink");
    gst_pad_add_probe(sunk, GST_PAD_PROBE_TYPE_QUERY_DOWNSTREAM, AnswerAllocation, nullptr, nullptr);
    gst_object_unref(sunk);
    GstPad* parsed = gst_element_get_static_pad(parse, "src");
    gst_pad_add_probe(parsed, GST_PAD_PROBE_TYPE_EVENT_DOWNSTREAM, DefaultColorimetry, nullptr, nullptr);
    gst_object_unref(parsed);
    decoded_ = gst_element_get_static_pad(decode, "src");
    bus_ = gst_element_get_bus(pipeline_);
    // Opening a device (VA display, CUDA context) happens here.
    if (gst_element_set_state(pipeline_, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE) {
      *why = decoder_.factory + ": " + TakeError();
      return false;
    }
    g_debug("Decoding %s %ux%u with %s%s", CodecName(codec_), width_, height_, decoder_.name.c_str(),
            converts_ ? " and videoconvert" : "");
    return true;
  }

  static GstFlowReturn OnSample(GstAppSink* sink, gpointer data) {
    GstSample* sample = gst_app_sink_pull_sample(sink);
    if (sample) {
      static_cast<Pipeline*>(data)->Decoded(sample);
      gst_sample_unref(sample);
    }
    return GST_FLOW_OK;
  }

  // Streaming thread.
  void Decoded(GstSample* sample);
  // Streaming thread: the picture's planes, or why there are none.
  std::string MapSystem(GstBuffer* buffer, PbVideoPicture* picture);
  std::string WrapDmabuf(GstBuffer* buffer, PbVideoPicture* picture);
  void Error(const std::string& text) {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (error_.empty()) error_ = text;
    }
    room_.notify_all();
  }

  const Candidate decoder_;
  const Codec codec_;
  const uint32_t width_, height_;
  const std::vector<uint64_t> modifiers_;
  bool converts_ = false, dmabuf_ = false;
  // Who takes the pictures; null once retired.
  std::mutex owner_mutex_;
  PbVideoDecoder* owner_;
  GstElement* pipeline_ = nullptr;
  GstAppSrc* src_ = nullptr;
  GstBus* bus_ = nullptr;
  // The decoder's source pad, for the log of what it gives.
  GstPad* decoded_ = nullptr;
  GstClockTime next_pts_ = 0;
  std::mutex mutex_;
  std::condition_variable room_;
  // Guarded by mutex_: the units fed and not yet decoded, by timestamp.
  std::map<GstClockTime, Frame> pending_;
  Clock::time_point progress_ = Clock::now();
  std::string error_;
  bool decoded_any_ = false;
  // Streaming thread only: the caps of the last picture and their layout.
  GstCaps* caps_ = nullptr;
  Layout layout_;
};
}  // namespace

// One texture's decoder. While a source is attached, its thread pulls the
// access units straight from Rust; nothing of the video crosses Dart.
struct PbVideoDecoder {
  PbVideoDecoder(const PbVideoExports& api, const PbVideoPresenter& presenter, void (*frame_ready)(void*),
                 void* context)
      : api_(api), presenter_(presenter), frame_ready_(frame_ready), context_(context) {}
  PbVideoDecoder(const PbVideoDecoder&) = delete;
  PbVideoDecoder& operator=(const PbVideoDecoder&) = delete;
  ~PbVideoDecoder() {
    Detach();
    Release(latest_);
  }

  // The caller's thread (the GTK one). The last picture stays through a
  // reconnect; the new session's first IDR replaces it.
  int64_t Attach(uint64_t source) {
    Detach();
    // Probing initialized GStreamer before any decoder was offered; this
    // thread never waits for it.
    if (!g_ready) return kVideoInvalid;
    generation_ = -1;
    last_ = 0;
    awaiting_key_ = true;
    paused_ = false;
    const int64_t consumer = api_.attach(source);
    if (consumer <= 0) return consumer;
    consumer_ = static_cast<uint64_t>(consumer);
    try {
      worker_ = std::thread([this, consumer] { Run(static_cast<uint64_t>(consumer)); });
    } catch (const std::exception&) {
      if (const uint64_t attached = consumer_.exchange(0)) api_.detach(attached);
      return kVideoInvalid;
    }
    return consumer;
  }

  // Detaching wakes a blocked pull, so the join is prompt. Then, with no
  // decoder thread left to publish, a new revision: a picture of this
  // source that the compositor has not taken yet stays on screen until the
  // next source's first one, but earns no receipt from any consumer
  // (Android's FeedQueue.reset). Attach detaches first, so it starts one
  // too.
  void Detach() {
    stopping_ = true;
    if (const uint64_t consumer = consumer_.exchange(0)) api_.detach(consumer);
    if (worker_.joinable()) worker_.join();
    stopping_ = false;
    std::lock_guard<std::mutex> lock(mutex_);
    ++revision_;
  }

  // Raster thread. A picture of an older revision stays on screen until a
  // newer one replaces it, but only a current one earns a receipt.
  PbVideoPicture* Acquire() {
    PbVideoPicture* picture = nullptr;
    bool acknowledge = false;
    // Read with the revision, under the lock that Detach starts a new one
    // under: a picture is only ever receipted to the source it came from.
    uint64_t consumer = 0;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (!latest_) return nullptr;
      picture = Retain(latest_);
      latest_acquired_ = true;
      if (picture->revision == revision_.load() &&
          (consumed_revision_ != picture->revision || consumed_frame_ != picture->frame_id)) {
        consumed_revision_ = picture->revision;
        consumed_frame_ = picture->frame_id;
        acknowledge = true;
        consumer = consumer_.load();
      }
    }
    if (acknowledge) {
      ++presented_;
      // Taken by the compositor: the receipt Rust validates and forwards
      // for input authorisation. Non-blocking.
      if (consumer) {
        api_.presented(consumer, static_cast<uint64_t>(picture->generation), static_cast<uint64_t>(picture->frame_id));
      }
    }
    return picture;
  }

  // `timings` starts the decode timing, or keeps it; false drops it until
  // the next read that asks.
  void Stats(bool timings, PbVideoStats* out) {
    *out = PbVideoStats{};
    out->submitted = submitted_;
    out->decoded = decoded_;
    out->presented = presented_;
    out->dropped = dropped_;
    out->configurations = configurations_;
    out->recoveries = recoveries_;
    std::lock_guard<std::mutex> lock(mutex_);
    CopyText(out->decoder, sizeof(out->decoder), decoder_name_);
    CopyText(out->last_recovery, sizeof(out->last_recovery), last_recovery_);
    out->flags = (converts_ ? PB_VIDEO_STATS_CONVERTS : 0) | (dmabuf_ ? PB_VIDEO_STATS_DMABUF : 0);
    out->format = format_;
    if (timings != timing_) decode_ = Timing{};
    timing_ = timings;
    if (timings) out->decode = decode_.Snapshot();
  }

  // Streaming thread: the newest picture, unless a newer revision began.
  // `decode` is how long its unit took from the pipeline's input.
  void Publish(PbVideoPicture* picture, Clock::duration decode) {
    ++decoded_;
    PbVideoPicture* replaced = picture;
    bool shown = false;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (timing_) decode_.Add(decode);
      format_ = picture->planes.format;
      dmabuf_ = picture->planes.dmabuf != 0;
      if (picture->revision == revision_.load()) {
        replaced = latest_;
        if (replaced && !latest_acquired_) ++dropped_;
        latest_ = picture;
        latest_acquired_ = false;
        shown = true;
      } else {
        ++dropped_;
      }
    }
    Release(replaced);
    if (shown) frame_ready_(context_);
  }

  // Any thread: the presenter cannot sample this decoder's dma-bufs.
  void RefuseDmabuf() {
    if (!dmabuf_refused_.exchange(true)) g_message("The presenter cannot sample dma-bufs: decoding to system memory");
  }

 private:
  // The dma-buf layouts the next pipeline may ask for: the presenter's,
  // while it samples them and GStreamer says their layout.
  std::vector<uint64_t> Modifiers() const {
    std::vector<uint64_t> modifiers;
    if (!presenter_.dmabuf || dmabuf_refused_ || !NegotiatesDmabuf()) return modifiers;
    const uint32_t count = std::min<uint32_t>(presenter_.modifier_count, PB_VIDEO_MAX_MODIFIERS);
    modifiers.assign(presenter_.modifiers, presenter_.modifiers + count);
    return modifiers;
  }

  // Decoder thread. The next keyframe builds a pipeline in a new revision.
  void Recover(const std::string& reason) {
    ++revision_;
    awaiting_key_ = true;
    ++recoveries_;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      last_recovery_ = reason;
    }
    g_warning("Native video decoding failed: %s", reason.c_str());
  }
  // Rust logs the reason, counts it toward giving up and rate-limits the
  // request. A paused source asks for nothing: its resume starts with an
  // IDR.
  void Fail(uint64_t consumer, const std::string& reason) {
    Recover(reason);
    if (!paused_) api_.failed(consumer, reason.c_str());
  }

  // Decoder thread: what Submit decided on the platform thread on Windows.
  // A new generation or a discontinuity starts a revision; older
  // generations, duplicates and anything before the next keyframe are
  // dropped.
  void Accept(uint64_t consumer, std::unique_ptr<Pipeline>& pipeline, GstBuffer* buffer,
              const PbDesktopVideoMetaV1& meta) {
    std::unique_ptr<GstBuffer, void (*)(GstBuffer*)> unit(buffer, [](GstBuffer* b) { gst_buffer_unref(b); });
    Frame frame;
    frame.generation = static_cast<int64_t>(meta.generation);
    frame.frame = static_cast<int64_t>(meta.frame_id);
    frame.width = meta.width;
    frame.height = meta.height;
    frame.key = (meta.flags & kFlagKeyframe) != 0;
    frame.codec = (meta.flags & kFlagH265) != 0 ? Codec::kH265 : Codec::kH264;
    if (frame.generation < generation_) {
      ++dropped_;
      return;
    }
    if (frame.generation != generation_ || (meta.flags & kFlagDiscontinuity) != 0) {
      ++revision_;
      generation_ = frame.generation;
      last_ = 0;
      awaiting_key_ = true;
    }
    if (frame.frame <= last_) {
      ++dropped_;
      return;
    }
    last_ = frame.frame;
    if (awaiting_key_ && !frame.key) {
      ++dropped_;
      api_.keyframe(consumer, kReasonGap);
      return;
    }
    awaiting_key_ = false;
    frame.revision = revision_;
    ++submitted_;
    try {
      // Only a size or codec change, or a failure (which released it),
      // rebuilds the pipeline. A discontinuity keeps it: the IDR that
      // follows resets the decoder's references, and what it still holds
      // of the old revision is decoded and not shown.
      if (!pipeline || pipeline->width() != frame.width || pipeline->height() != frame.height ||
          pipeline->codec() != frame.codec) {
        if (!frame.key) throw std::runtime_error("A fresh key frame is required");
        if (frame.width == 0 || frame.height == 0 || frame.width > kMaxWidth || frame.height > kMaxHeight) {
          throw std::runtime_error("Video size outside the session limits");
        }
        pipeline.reset();
        pipeline = Pipeline::Start(frame, this, Modifiers());
        ++configurations_;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          decoder_name_ = pipeline->decoder().name;
          converts_ = pipeline->converts();
        }
        api_.report(consumer, pipeline->decoder().name.c_str(), pipeline->decoder().hardware ? 1 : 0);
      }
      pipeline->Feed(frame, unit.release());
    } catch (const std::exception& e) {
      pipeline.reset();
      Fail(consumer, e.what());
    }
  }

  // Decoder thread, after every pull: a pipeline error, or a stall, ends
  // the pipeline.
  void Service(uint64_t consumer, std::unique_ptr<Pipeline>& pipeline) {
    if (!pipeline) return;
    const std::string error = pipeline->TakeError();
    const bool stalled = error.empty() && !paused_ && pipeline->Stalled(resumed_at_);
    if (error.empty() && !stalled) return;
    if (!pipeline->decoded_any()) MarkFailed(pipeline->decoder().factory);
    pipeline.reset();
    if (!error.empty()) {
      Fail(consumer, error);
      return;
    }
    Recover("Native video decoder stalled");
    api_.keyframe(consumer, kReasonStall);
  }

  // Decoder thread, one per attached source. `next` is the only blocking
  // call, and it runs only while the pipeline has room.
  void Run(uint64_t consumer) {
    pthread_setname_np(pthread_self(), "pb-desktop-video");
    std::unique_ptr<Pipeline> pipeline;
    InputPool input;
    PbDesktopVideoMetaV1 meta{};
    resumed_at_ = Clock::now();
    while (!stopping_) {
      if (pipeline && pipeline->dmabuf() && dmabuf_refused_) {
        // Its pictures cannot be shown: the next keyframe builds a
        // pipeline in system memory.
        pipeline.reset();
        awaiting_key_ = true;
        api_.keyframe(consumer, kReasonGap);
      }
      if (pipeline && !pipeline->WaitForRoom(kPullTimeoutMs)) {
        Service(consumer, pipeline);
        continue;
      }
      GstBuffer* buffer = input.Take();
      if (!buffer) {
        Fail(consumer, "Cannot allocate a video buffer");
        break;
      }
      GstMapInfo map;
      if (!gst_buffer_map(buffer, &map, GST_MAP_WRITE)) {
        gst_buffer_unref(buffer);
        Fail(consumer, "Cannot map a video buffer");
        break;
      }
      const int32_t r = api_.next(consumer, kPullTimeoutMs, map.data, static_cast<uint32_t>(map.size), &meta);
      gst_buffer_unmap(buffer, &map);
      if (r == kVideoUnit) {
        if (paused_) resumed_at_ = Clock::now();
        paused_ = false;
        gst_buffer_set_size(buffer, meta.len);
        Accept(consumer, pipeline, buffer, meta);
        buffer = nullptr;
      } else if (r == kVideoNeedCapacity) {
        // The unit stays queued in Rust and arrives on the next call.
        if (meta.len > kMaxBytes) {
          gst_buffer_unref(buffer);
          break;
        }
        input.Grow(meta.len);
      } else if (r == kVideoPaused) {
        // A hidden texture is not consumed and a paused source sends
        // nothing: the stall clock restarts when the video does.
        paused_ = true;
      } else if (r != kVideoTimeout) {
        gst_buffer_unref(buffer);
        break;  // Closed, detached or invalid: this consumer is done.
      }
      if (buffer) gst_buffer_unref(buffer);
      Service(consumer, pipeline);
    }
    api_.detach(consumer);
    Pipeline::Retire(std::move(pipeline));
  }

  const PbVideoExports api_;
  const PbVideoPresenter presenter_;
  std::atomic<bool> dmabuf_refused_{false};
  void (*const frame_ready_)(void*);
  void* const context_;
  std::atomic<uint64_t> consumer_{0};
  std::atomic<bool> stopping_{false};
  std::atomic<uint64_t> revision_{1};
  std::atomic<uint64_t> submitted_{0}, decoded_{0}, presented_{0}, dropped_{0}, configurations_{0}, recoveries_{0};
  std::mutex mutex_;
  // Guarded by mutex_.
  PbVideoPicture* latest_ = nullptr;
  bool latest_acquired_ = false;
  int64_t consumed_frame_ = 0;
  uint64_t consumed_revision_ = 0;
  std::string decoder_name_;
  std::string last_recovery_;
  // What the current pipeline does, and the last picture's format.
  bool converts_ = false, dmabuf_ = false;
  uint32_t format_ = 0;
  // While a stats read asks for timings.
  bool timing_ = false;
  Timing decode_;
  // The decoder thread's own; reset by Attach while no thread runs.
  int64_t generation_ = -1, last_ = 0;
  bool awaiting_key_ = true;
  std::atomic<bool> paused_{false};
  Clock::time_point resumed_at_ = Clock::now();
  std::thread worker_;
};

namespace {
void Pipeline::Decoded(GstSample* sample) {
  GstBuffer* buffer = gst_sample_get_buffer(sample);
  GstCaps* caps = gst_sample_get_caps(sample);
  if (!buffer || !caps) return;
  Frame frame;
  bool known = false;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = pending_.find(GST_BUFFER_PTS(buffer));
    if (found != pending_.end()) {
      frame = found->second;
      // A decoder that dropped a unit never returns it: what came before
      // this picture is done.
      pending_.erase(pending_.begin(), std::next(found));
      progress_ = Clock::now();
      decoded_any_ = true;
      known = true;
    }
  }
  room_.notify_all();
  if (!known) return Error("Unknown decoded picture");
  if (caps != caps_) {
    if (!ReadLayout(caps, &layout_)) return Error("Unreadable decoded format");
    gst_caps_replace(&caps_, caps);
    // What the decoder gives, its memory and colorimetry included, once
    // per format: G_MESSAGES_DEBUG=pb-mapper-video shows whether dma-bufs
    // were negotiated.
    if (GstCaps* decoded = gst_pad_get_current_caps(decoded_)) {
      gchar* text = gst_caps_to_string(decoded);
      g_debug("%s gives %s", decoder_.factory.c_str(), text);
      g_free(text);
      gst_caps_unref(decoded);
    }
  }
  // The visible picture must be the authorized frame, never a padded or
  // cropped one.
  if (layout_.width != frame.width || layout_.height != frame.height) {
    return Error("Decoded picture does not match the authorized frame");
  }
  auto* picture = new PbVideoPicture();
  picture->generation = frame.generation;
  picture->frame_id = frame.frame;
  picture->revision = frame.revision;
  PbVideoPlanes& planes = picture->planes;
  planes.width = frame.width;
  planes.height = frame.height;
  planes.format = layout_.format;
  planes.kr = layout_.kr;
  planes.kb = layout_.kb;
  planes.full_range = layout_.full_range ? 1 : 0;
  planes.dmabuf = layout_.dmabuf ? 1 : 0;
  planes.modifier = layout_.modifier;
  for (PbVideoPlane& plane : planes.planes) plane.fd = -1;
  const std::string failure = layout_.dmabuf ? WrapDmabuf(buffer, picture) : MapSystem(buffer, picture);
  if (!failure.empty()) {
    Release(picture);
    return Error(failure);
  }
  std::lock_guard<std::mutex> lock(owner_mutex_);
  if (owner_) {
    owner_->Publish(picture, Clock::now() - frame.fed);
  } else {
    Release(picture);
  }
}

// Mapped for the picture's life, through the decoder's GstVideoMeta when it
// gave one: each plane from its first visible row.
std::string Pipeline::MapSystem(GstBuffer* buffer, PbVideoPicture* picture) {
  if (!gst_video_frame_map(&picture->frame, &layout_.info, buffer, GST_MAP_READ)) return "Cannot read a decoded picture";
  picture->mapped = true;
  PbVideoPlanes& planes = picture->planes;
  planes.plane_count = planes.format == PB_VIDEO_FORMAT_NV12 ? 2 : 3;
  if (GST_VIDEO_FRAME_N_PLANES(&picture->frame) != planes.plane_count) return "Unexpected decoded planes";
  for (uint32_t p = 0; p < planes.plane_count; ++p) {
    const gint stride = GST_VIDEO_FRAME_PLANE_STRIDE(&picture->frame, p);
    if (stride <= 0) return "Unexpected decoded plane stride";
    PbVideoPlane& plane = planes.planes[p];
    plane.data = static_cast<const uint8_t*>(GST_VIDEO_FRAME_PLANE_DATA(&picture->frame, p));
    plane.stride = static_cast<uint32_t>(stride);
    plane.width = p == 0 ? planes.width : (planes.width + 1) / 2;
    plane.height = p == 0 ? planes.height : (planes.height + 1) / 2;
  }
  return "";
}

// Never mapped: each plane's descriptor and offset, from the GstVideoMeta a
// DMA_DRM picture always has (1088-row surfaces included).
std::string Pipeline::WrapDmabuf(GstBuffer* buffer, PbVideoPicture* picture) {
  const GstVideoMeta* meta = gst_buffer_get_video_meta(buffer);
  if (!meta || meta->n_planes != 2) return "A dma-buf picture without its NV12 layout";
  PbVideoPlanes& planes = picture->planes;
  planes.plane_count = 2;
  for (uint32_t p = 0; p < 2; ++p) {
    guint index = 0, length = 0;
    gsize skip = 0;
    if (!gst_buffer_find_memory(buffer, meta->offset[p], 1, &index, &length, &skip)) return "A dma-buf plane is missing";
    GstMemory* memory = gst_buffer_peek_memory(buffer, index);
    if (!memory || !gst_is_dmabuf_memory(memory)) return "A dma-buf picture in other memory";
    const gint fd = gst_dmabuf_memory_get_fd(memory);
    const gsize offset = memory->offset + skip;
    if (fd < 0 || meta->stride[p] <= 0 || offset > UINT32_MAX) return "An unreadable dma-buf plane";
    PbVideoPlane& plane = planes.planes[p];
    plane.fd = fd;
    plane.offset = static_cast<uint32_t>(offset);
    plane.stride = static_cast<uint32_t>(meta->stride[p]);
    plane.width = p == 0 ? planes.width : (planes.width + 1) / 2;
    plane.height = p == 0 ? planes.height : (planes.height + 1) / 2;
  }
  picture->buffer = gst_buffer_ref(buffer);
  return "";
}

PbVideoDecoder* Create(const PbVideoExports* exports, const PbVideoPresenter* presenter, void (*frame_ready)(void*),
                       void* context) {
  if (!exports || !frame_ready || !exports->attach || !exports->next || !exports->presented || !exports->keyframe ||
      !exports->failed || !exports->report || !exports->detach) {
    return nullptr;
  }
  // No presenter: system memory only.
  const PbVideoPresenter none{};
  try {
    return new PbVideoDecoder(*exports, presenter ? *presenter : none, frame_ready, context);
  } catch (const std::exception&) {
    return nullptr;
  }
}
int64_t Attach(PbVideoDecoder* decoder, uint64_t source) { return decoder ? decoder->Attach(source) : kVideoInvalid; }
void Detach(PbVideoDecoder* decoder) {
  if (decoder) decoder->Detach();
}
PbVideoPicture* Acquire(PbVideoDecoder* decoder) { return decoder ? decoder->Acquire() : nullptr; }
void Stats(PbVideoDecoder* decoder, uint32_t timings, PbVideoStats* out) {
  if (decoder && out) decoder->Stats(timings != 0, out);
}
void Destroy(PbVideoDecoder* decoder) { delete decoder; }
int32_t Describe(const PbVideoPicture* picture, PbVideoPlanes* out) {
  if (!picture || !out) return -1;
  *out = picture->planes;
  return 0;
}
// GStreamer's own converter, as videoconvert converts: no dithering and no
// chroma resampling (each chroma sample covers its two by two pixels), as
// the GPU conversion samples it, scaled in the same pass.
int32_t Snapshot(const PbVideoPicture* picture, uint32_t max_width, uint8_t* rgba, size_t capacity, uint32_t* width,
                 uint32_t* height) {
  if (!picture || !picture->mapped || max_width == 0 || !width || !height) return -1;
  const uint32_t source_width = picture->planes.width, source_height = picture->planes.height;
  if (source_width == 0 || source_height == 0) return -1;
  const uint32_t scaled_width = std::min(max_width, source_width);
  const uint32_t scaled_height = std::max<uint32_t>(
      1, static_cast<uint32_t>((static_cast<uint64_t>(source_height) * scaled_width + source_width / 2) / source_width));
  *width = scaled_width;
  *height = scaled_height;
  const size_t size = static_cast<size_t>(scaled_width) * scaled_height * 4;
  if (!rgba || capacity < size) return -2;
  GstVideoInfo out;
  gst_video_info_init(&out);
  if (!gst_video_info_set_format(&out, GST_VIDEO_FORMAT_RGBA, scaled_width, scaled_height) ||
      out.stride[0] != static_cast<gint>(scaled_width * 4)) {
    return -1;
  }
  GstBuffer* wrapped =
      gst_buffer_new_wrapped_full(static_cast<GstMemoryFlags>(0), rgba, size, 0, size, nullptr, nullptr);
  GstVideoFrame frame{};
  if (!wrapped || !gst_video_frame_map(&frame, &out, wrapped, GST_MAP_WRITE)) {
    if (wrapped) gst_buffer_unref(wrapped);
    return -1;
  }
  GstStructure* config = gst_structure_new(
      "pb-snapshot", GST_VIDEO_CONVERTER_OPT_DITHER_METHOD, GST_TYPE_VIDEO_DITHER_METHOD, GST_VIDEO_DITHER_NONE,
      GST_VIDEO_CONVERTER_OPT_CHROMA_MODE, GST_TYPE_VIDEO_CHROMA_MODE, GST_VIDEO_CHROMA_MODE_NONE, nullptr);
  GstVideoConverter* converter = gst_video_converter_new(&picture->frame.info, &out, config);
  if (converter) {
    gst_video_converter_frame(converter, &picture->frame, &frame);
    gst_video_converter_free(converter);
  }
  gst_video_frame_unmap(&frame);
  gst_buffer_unref(wrapped);
  return converter ? 0 : -1;
}
void RefuseDmabuf(PbVideoDecoder* decoder) {
  if (decoder) decoder->RefuseDmabuf();
}
PbVideoPicture* RetainPicture(PbVideoPicture* picture) { return Retain(picture); }
void ReleasePicture(PbVideoPicture* picture) { Release(picture); }
}  // namespace

extern "C" __attribute__((visibility("default"))) const PbVideoModule* pb_mapper_desktop_video_module_v1(void) {
  static const PbVideoModule module = {PB_VIDEO_MODULE_ABI, Probe,    Create,   Attach,       Detach,
                                       Acquire,            Stats,    Destroy,  Describe,     Snapshot,
                                       RefuseDmabuf,       RetainPicture, ReleasePicture};
  return &module;
}
