// libpb_mapper_desktop_video.so: native H.264 and H.265 decoding for the
// Linux viewer, through GStreamer. The runner loads it with dlopen
// (desktop_video.cc), so the app starts where GStreamer is missing.
//
// One decoder thread per attached source pulls access units straight from
// Rust into pooled GStreamer buffers and feeds an explicit pipeline,
//   appsrc -> h264parse | h265parse -> decoder -> videoconvert -> appsink,
// whose streaming thread decodes, converts to RGBA and publishes the newest
// picture. The compositor's raster thread acquires it (FlPixelBufferTexture
// copies it into its texture: the one CPU copy of this path) and that
// acquisition is the presentation receipt. The decoder state rules are the
// Windows runner's (ui/windows/runner/desktop_video.cpp) and the Android
// feeder's (VideoFeeder.kt): a generation change or a discontinuity starts a
// revision that waits for a keyframe, a codec or size change rebuilds the
// pipeline at a keyframe, and pictures of an older revision earn no receipt.
#define G_LOG_DOMAIN "pb-mapper-video"

#include "desktop_video_module.h"

#include <gst/app/gstappsink.h>
#include <gst/app/gstappsrc.h>
#include <gst/gst.h>
#include <gst/video/video.h>
#include <pthread.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
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
constexpr uint32_t kMaxConvertThreads = 4;

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

bool OffersSystemMemory(GstElementFactory* factory) {
  for (const GList* item = gst_element_factory_get_static_pad_templates(factory); item; item = item->next) {
    auto* templ = static_cast<GstStaticPadTemplate*>(item->data);
    if (templ->direction != GST_PAD_SRC) continue;
    GstCaps* caps = gst_static_pad_template_get_caps(templ);
    bool found = false;
    for (guint i = 0; !found && i < gst_caps_get_size(caps); ++i) {
      GstCapsFeatures* features = gst_caps_get_features(caps, i);
      found = !features || gst_caps_features_is_any(features) ||
              gst_caps_features_contains(features, GST_CAPS_FEATURE_MEMORY_SYSTEM_MEMORY);
    }
    gst_caps_unref(caps);
    if (found) return true;
  }
  return false;
}

// Every decoder the registry has for `codec` that this pipeline can use:
// it takes the codec, gives pictures in system memory (videoconvert reads
// nothing else) and has a rank (GST_PLUGIN_FEATURE_RANK=name:NONE takes one
// out). Hardware first, then by rank, as GStreamer would pick them.
std::vector<Candidate> Candidates(Codec codec) {
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
    if (!takes || !OffersSystemMemory(factory)) continue;
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
  for (const Candidate& candidate : Candidates(codec)) {
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

// A decoded picture. The video frame stays mapped, and its buffer out of
// videoconvert's pool, until the last reference goes.
struct PbVideoPicture {
  std::atomic<int> refs{1};
  GstVideoFrame frame{};
  bool mapped = false;
  // Only for a decoder whose rows are padded: Flutter reads packed rows.
  std::vector<uint8_t> packed;
  const uint8_t* data = nullptr;
  uint32_t width = 0, height = 0;
  int64_t generation = 0, frame_id = 0;
  uint64_t revision = 0;
  ~PbVideoPicture() {
    if (mapped) gst_video_frame_unmap(&frame);
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
  static std::unique_ptr<Pipeline> Start(const Frame& frame, PbVideoDecoder* owner) {
    std::string last;
    bool any = false;
    if (!HasElement(Parser(frame.codec))) throw std::runtime_error(std::string("GStreamer has no ") + Parser(frame.codec));
    for (Candidate& candidate : Candidates(frame.codec)) {
      if (Failed(candidate.factory) || !Allows(candidate, frame.codec, frame.width, frame.height)) continue;
      any = true;
      std::unique_ptr<Pipeline> pipeline(new Pipeline(frame, std::move(candidate), owner));
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
      if (pending_.empty()) progress_ = Clock::now();
      pending_.emplace(next_pts_, frame);
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
  Pipeline(const Frame& frame, Candidate decoder, PbVideoDecoder* owner)
      : decoder_(std::move(decoder)), codec_(frame.codec), width_(frame.width), height_(frame.height), owner_(owner) {}

  bool Build(std::string* why) {
    pipeline_ = gst_pipeline_new("pb-desktop-video");
    GstElement* src = gst_element_factory_make("appsrc", nullptr);
    GstElement* parse = gst_element_factory_make(Parser(codec_), nullptr);
    GstElement* decode = gst_element_factory_make(decoder_.factory.c_str(), nullptr);
    GstElement* convert = gst_element_factory_make("videoconvert", nullptr);
    GstElement* sink = gst_element_factory_make("appsink", nullptr);
    if (!pipeline_ || !src || !parse || !decode || !convert || !sink) {
      for (GstElement* element : {src, parse, decode, convert, sink}) {
        if (element) gst_object_unref(gst_object_ref_sink(element));
      }
      *why = "cannot create " + decoder_.factory;
      return false;
    }
    gst_bin_add_many(GST_BIN(pipeline_), src, parse, decode, convert, sink, nullptr);
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
    SetIfPresent(convert, "n-threads", std::to_string(std::min(kMaxConvertThreads, g_get_num_processors())).c_str());
    GstCaps* output = gst_caps_new_simple("video/x-raw", "format", G_TYPE_STRING, "RGBA", nullptr);
    g_object_set(sink, "caps", output, "sync", FALSE, "max-buffers", 1u, "drop", FALSE, "enable-last-sample", FALSE,
                 "qos", FALSE, "emit-signals", FALSE, nullptr);
    gst_caps_unref(output);
    GstAppSinkCallbacks callbacks{};
    callbacks.new_sample = &Pipeline::OnSample;
    gst_app_sink_set_callbacks(GST_APP_SINK(sink), &callbacks, this, nullptr);

    if (!gst_element_link_many(src, parse, decode, convert, sink, nullptr)) {
      *why = "cannot link " + decoder_.factory;
      return false;
    }
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
    g_debug("Decoding %s %ux%u with %s", CodecName(codec_), width_, height_, decoder_.name.c_str());
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
  GstVideoInfo info_{};
};
}  // namespace

// One texture's decoder. While a source is attached, its thread pulls the
// access units straight from Rust; nothing of the video crosses Dart.
struct PbVideoDecoder {
  PbVideoDecoder(const PbVideoExports& api, void (*frame_ready)(void*), void* context)
      : api_(api), frame_ready_(frame_ready), context_(context) {}
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

  void Stats(PbVideoStats* out) {
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
  }

  // Streaming thread: the newest picture, unless a newer revision began.
  void Publish(PbVideoPicture* picture) {
    ++decoded_;
    PbVideoPicture* replaced = picture;
    bool shown = false;
    {
      std::lock_guard<std::mutex> lock(mutex_);
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

 private:
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
        pipeline = Pipeline::Start(frame, this);
        ++configurations_;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          decoder_name_ = pipeline->decoder().name;
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
    if (!gst_video_info_from_caps(&info_, caps)) return Error("Unreadable decoded format");
    gst_caps_replace(&caps_, caps);
    // What the decoder gives, colorimetry included, once per format.
    if (GstCaps* decoded = gst_pad_get_current_caps(decoded_)) {
      gchar* text = gst_caps_to_string(decoded);
      g_debug("%s gives %s", decoder_.factory.c_str(), text);
      g_free(text);
      gst_caps_unref(decoded);
    }
  }
  // The visible picture must be the authorized frame, never a padded or
  // cropped one.
  if (static_cast<uint32_t>(GST_VIDEO_INFO_WIDTH(&info_)) != frame.width ||
      static_cast<uint32_t>(GST_VIDEO_INFO_HEIGHT(&info_)) != frame.height ||
      GST_VIDEO_INFO_FORMAT(&info_) != GST_VIDEO_FORMAT_RGBA) {
    return Error("Decoded picture does not match the authorized frame");
  }
  auto* picture = new PbVideoPicture();
  picture->width = frame.width;
  picture->height = frame.height;
  picture->generation = frame.generation;
  picture->frame_id = frame.frame;
  picture->revision = frame.revision;
  if (!gst_video_frame_map(&picture->frame, &info_, buffer, GST_MAP_READ)) {
    Release(picture);
    return Error("Cannot read a decoded picture");
  }
  picture->mapped = true;
  const auto* pixels = static_cast<const uint8_t*>(GST_VIDEO_FRAME_PLANE_DATA(&picture->frame, 0));
  const size_t stride = static_cast<size_t>(GST_VIDEO_FRAME_PLANE_STRIDE(&picture->frame, 0));
  const size_t row = static_cast<size_t>(frame.width) * 4;
  if (stride == row) {
    picture->data = pixels;
  } else {
    picture->packed.resize(row * frame.height);
    for (uint32_t y = 0; y < frame.height; ++y) memcpy(&picture->packed[y * row], pixels + y * stride, row);
    gst_video_frame_unmap(&picture->frame);
    picture->mapped = false;
    picture->data = picture->packed.data();
  }
  std::lock_guard<std::mutex> lock(owner_mutex_);
  if (owner_) {
    owner_->Publish(picture);
  } else {
    Release(picture);
  }
}

PbVideoDecoder* Create(const PbVideoExports* exports, void (*frame_ready)(void*), void* context) {
  if (!exports || !frame_ready || !exports->attach || !exports->next || !exports->presented || !exports->keyframe ||
      !exports->failed || !exports->report || !exports->detach) {
    return nullptr;
  }
  try {
    return new PbVideoDecoder(*exports, frame_ready, context);
  } catch (const std::exception&) {
    return nullptr;
  }
}
int64_t Attach(PbVideoDecoder* decoder, uint64_t source) { return decoder ? decoder->Attach(source) : kVideoInvalid; }
void Detach(PbVideoDecoder* decoder) {
  if (decoder) decoder->Detach();
}
PbVideoPicture* Acquire(PbVideoDecoder* decoder) { return decoder ? decoder->Acquire() : nullptr; }
void Stats(PbVideoDecoder* decoder, PbVideoStats* out) {
  if (decoder && out) decoder->Stats(out);
}
void Destroy(PbVideoDecoder* decoder) { delete decoder; }
const uint8_t* Pixels(const PbVideoPicture* picture, uint32_t* width, uint32_t* height) {
  if (!picture) return nullptr;
  if (width) *width = picture->width;
  if (height) *height = picture->height;
  return picture->data;
}
PbVideoPicture* RetainPicture(PbVideoPicture* picture) { return Retain(picture); }
void ReleasePicture(PbVideoPicture* picture) { Release(picture); }
}  // namespace

extern "C" __attribute__((visibility("default"))) const PbVideoModule* pb_mapper_desktop_video_module_v1(void) {
  static const PbVideoModule module = {PB_VIDEO_MODULE_ABI, Probe, Create, Attach, Detach, Acquire,
                                       Stats, Destroy, Pixels, RetainPicture, ReleasePicture};
  return &module;
}
