// Decodes recorded clips through libpb_mapper_desktop_video.so outside
// Flutter: libpb_mapper_ffi.so's test source (feature desktop-test-source)
// plays them into a video source, the module pulls, decodes and publishes
// them, and a thread standing in for Flutter's raster thread acquires each
// picture, which is its presentation receipt. It checks the decoder state
// rules the runner shares with Windows and Android. Run by
// scripts/test/linux-desktop-video.sh, which builds the library and the
// clips.
//
// desktop_video_test <libpb_mapper_ffi.so> <clip folder>
#include <dlfcn.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <functional>
#include <iterator>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "desktop_video_module.h"

namespace {
using Clock = std::chrono::steady_clock;
using std::chrono::milliseconds;

constexpr uint32_t kFlagKeyframe = 1;
constexpr uint32_t kFlagDiscontinuity = 2;

struct Ffi {
  PbVideoExports exports{};
  uint64_t (*source)(const char* path, uint32_t fps) = nullptr;
  int32_t (*pause)(uint64_t source, uint32_t paused) = nullptr;
  void (*stop)(uint64_t source) = nullptr;
};
Ffi g_ffi;

// What the module told Rust, counted by shims around the real exports.
std::atomic<int> g_gaps{0}, g_stalls{0}, g_failures{0}, g_reports{0}, g_accepted{0}, g_receipts{0},
    g_need_capacity{0};
std::mutex g_text_mutex;
std::string g_decoder, g_failure;
bool g_hardware = false;
// Set by a check: the next pulled delta unit is marked a discontinuity,
// as the sink marks the first unit after a drop.
std::atomic<bool> g_break_next_delta{false};
std::atomic<uint64_t> g_broken_frame{0};

int32_t Next(uint64_t consumer, uint32_t timeout_ms, uint8_t* dst, uint32_t capacity, PbDesktopVideoMetaV1* meta) {
  const int32_t r = g_ffi.exports.next(consumer, timeout_ms, dst, capacity, meta);
  if (r == -2) ++g_need_capacity;
  if (r == 1 && !(meta->flags & kFlagKeyframe) && g_break_next_delta.exchange(false)) {
    meta->flags |= kFlagDiscontinuity;
    g_broken_frame = meta->frame_id;
  }
  return r;
}
int32_t Presented(uint64_t consumer, uint64_t generation, uint64_t frame_id) {
  ++g_receipts;
  const int32_t r = g_ffi.exports.presented(consumer, generation, frame_id);
  if (r == 1) ++g_accepted;
  return r;
}
int32_t Keyframe(uint64_t consumer, uint32_t reason) {
  if (reason == 1) ++g_gaps;
  if (reason == 3) ++g_stalls;
  return g_ffi.exports.keyframe(consumer, reason);
}
int32_t Failed(uint64_t consumer, const char* message) {
  ++g_failures;
  {
    std::lock_guard<std::mutex> lock(g_text_mutex);
    g_failure = message ? message : "";
  }
  fprintf(stderr, "  failed_v1: %s\n", message ? message : "");
  return g_ffi.exports.failed(consumer, message);
}
int32_t Report(uint64_t consumer, const char* decoder, uint32_t flags) {
  ++g_reports;
  {
    std::lock_guard<std::mutex> lock(g_text_mutex);
    g_decoder = decoder;
    g_hardware = flags & 1;
  }
  return g_ffi.exports.report(consumer, decoder, flags);
}

template <typename Fn>
void Resolve(void* handle, Fn& fn, const char* name) {
  fn = reinterpret_cast<Fn>(dlsym(handle, name));
  if (!fn) {
    fprintf(stderr, "missing %s\n", name);
    exit(2);
  }
}

// Flutter's raster thread: acquires the newest picture whenever the module
// says there is one, and keeps it until the next.
class Raster {
 public:
  explicit Raster(const PbVideoModule* module) : module_(module) {}
  ~Raster() { Stop(); }
  static void Ready(void* context) {
    auto* self = static_cast<Raster*>(context);
    {
      std::lock_guard<std::mutex> lock(self->mutex_);
      self->ready_ = true;
      self->ready_at_.push_back(Clock::now());
    }
    self->wake_.notify_one();
  }
  // How regularly pictures came out since the last call: the median, 95th
  // percentile and largest interval, in milliseconds.
  std::string Pacing() {
    std::vector<double> intervals;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      for (size_t i = 1; i < ready_at_.size(); ++i) {
        intervals.push_back(std::chrono::duration<double, std::milli>(ready_at_[i] - ready_at_[i - 1]).count());
      }
      ready_at_.clear();
    }
    if (intervals.empty()) return "no pictures";
    std::sort(intervals.begin(), intervals.end());
    char text[96];
    snprintf(text, sizeof(text), "picture intervals p50 %.1f ms, p95 %.1f ms, max %.1f ms",
             intervals[intervals.size() / 2], intervals[intervals.size() * 95 / 100], intervals.back());
    return text;
  }
  void Start(PbVideoDecoder* decoder) {
    decoder_ = decoder;
    thread_ = std::thread([this] { Run(); });
  }
  void Stop() {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      stopping_ = true;
    }
    wake_.notify_one();
    if (thread_.joinable()) thread_.join();
    if (shown_) module_->release(shown_);
    shown_ = nullptr;
  }
  int pictures() const { return pictures_; }
  // The size of the picture shown last, and whether it has content.
  void Last(uint32_t* width, uint32_t* height, bool* content) {
    std::lock_guard<std::mutex> lock(mutex_);
    *width = width_;
    *height = height_;
    *content = content_;
  }

 private:
  void Run() {
    while (true) {
      {
        std::unique_lock<std::mutex> lock(mutex_);
        wake_.wait(lock, [this] { return ready_ || stopping_; });
        if (stopping_) return;
        ready_ = false;
      }
      PbVideoPicture* picture = module_->acquire(decoder_);
      if (!picture) continue;
      uint32_t width = 0, height = 0;
      const uint8_t* rgba = module_->pixels(picture, &width, &height);
      // The test pattern is never one colour.
      bool content = false;
      for (size_t i = 4; rgba && !content && i < static_cast<size_t>(width) * height * 4; i += 4099 * 4) {
        content = memcmp(rgba, rgba + i, 3) != 0;
      }
      if (shown_) module_->release(shown_);
      shown_ = picture;
      ++pictures_;
      std::lock_guard<std::mutex> lock(mutex_);
      width_ = width;
      height_ = height;
      content_ = content;
    }
  }
  const PbVideoModule* module_;
  PbVideoDecoder* decoder_ = nullptr;
  std::thread thread_;
  std::mutex mutex_;
  std::condition_variable wake_;
  bool ready_ = false, stopping_ = false, content_ = false;
  std::vector<Clock::time_point> ready_at_;
  uint32_t width_ = 0, height_ = 0;
  std::atomic<int> pictures_{0};
  PbVideoPicture* shown_ = nullptr;
};

// An Annex B stream with access unit delimiters, as `u32 BE len | unit`.
std::string WriteClip(const std::vector<std::string>& streams, bool hevc, const std::string& out) {
  std::ofstream clip(out, std::ios::binary);
  size_t units = 0;
  for (const std::string& path : streams) {
    std::ifstream in(path, std::ios::binary);
    const std::vector<uint8_t> data((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    std::vector<size_t> starts;
    for (size_t i = 0; i + 4 < data.size(); ++i) {
      if (data[i] != 0 || data[i + 1] != 0 || data[i + 2] != 1) continue;
      const uint8_t header = data[i + 3];
      const bool aud = hevc ? ((header >> 1) & 0x3f) == 35 : (header & 0x1f) == 9;
      if (aud) starts.push_back(i > 0 && data[i - 1] == 0 ? i - 1 : i);
    }
    if (starts.empty()) return "";
    starts.push_back(data.size());
    for (size_t i = 0; i + 1 < starts.size(); ++i) {
      const uint32_t len = static_cast<uint32_t>(starts[i + 1] - starts[i]);
      const uint8_t be[4] = {uint8_t(len >> 24), uint8_t(len >> 16), uint8_t(len >> 8), uint8_t(len)};
      clip.write(reinterpret_cast<const char*>(be), 4);
      clip.write(reinterpret_cast<const char*>(&data[starts[i]]), len);
      ++units;
    }
  }
  return units ? out : "";
}

bool Exists(const std::string& path) { return std::ifstream(path).good(); }

bool WaitFor(const std::function<bool()>& done, milliseconds limit) {
  const auto deadline = Clock::now() + limit;
  while (!done()) {
    if (Clock::now() > deadline) return false;
    std::this_thread::sleep_for(milliseconds(10));
  }
  return true;
}

int g_checks_failed = 0;
void Check(bool ok, const char* what) {
  printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
  if (!ok) ++g_checks_failed;
}

PbVideoStats Stats(const PbVideoModule* module, PbVideoDecoder* decoder) {
  PbVideoStats stats{};
  module->stats(decoder, &stats);
  return stats;
}
}  // namespace

int main(int argc, char** argv) {
  // --broken-hardware: the registry lists hardware decoders whose device
  // cannot open (the script hides it), as on a machine whose GPU or driver
  // is gone; every check then expects what the rule leaves.
  const bool broken = argc == 4 && strcmp(argv[3], "--broken-hardware") == 0;
  if (argc != 3 && !broken) {
    fprintf(stderr, "usage: %s <libpb_mapper_ffi.so> <clip folder> [--broken-hardware]\n", argv[0]);
    return 2;
  }
  void* ffi = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!ffi) {
    fprintf(stderr, "%s\n", dlerror());
    return 2;
  }
  PbVideoExports& real = g_ffi.exports;
  Resolve(ffi, real.attach, "pb_mapper_desktop_video_attach_v1");
  Resolve(ffi, real.next, "pb_mapper_desktop_video_next_v1");
  Resolve(ffi, real.presented, "pb_mapper_desktop_video_presented_v1");
  Resolve(ffi, real.keyframe, "pb_mapper_desktop_video_keyframe_v1");
  Resolve(ffi, real.failed, "pb_mapper_desktop_video_failed_v1");
  Resolve(ffi, real.report, "pb_mapper_desktop_video_report_v1");
  Resolve(ffi, real.detach, "pb_mapper_desktop_video_detach_v1");
  Resolve(ffi, g_ffi.source, "pb_mapper_desktop_video_test_source_v1");
  Resolve(ffi, g_ffi.pause, "pb_mapper_desktop_video_test_source_pause_v1");
  Resolve(ffi, g_ffi.stop, "pb_mapper_desktop_video_test_source_stop_v1");
  const PbVideoExports shims = {real.attach, Next, Presented, Keyframe, Failed, Report, real.detach};

  void* handle = dlopen(PB_VIDEO_MODULE_PATH, RTLD_NOW | RTLD_LOCAL);
  auto entry = handle ? reinterpret_cast<PbVideoModuleEntry>(dlsym(handle, PB_VIDEO_MODULE_SYMBOL)) : nullptr;
  const PbVideoModule* module = entry ? entry() : nullptr;
  if (!module || module->abi != PB_VIDEO_MODULE_ABI) {
    fprintf(stderr, "cannot load %s: %s\n", PB_VIDEO_MODULE_PATH, handle ? "wrong ABI" : dlerror());
    return 2;
  }

  const std::string folder = argv[2];
  const std::string h264 = WriteClip({folder + "/h264-720.h264"}, false, folder + "/h264-720.clip");
  const std::string mixed =
      WriteClip({folder + "/h264-720.h264", folder + "/h264-1080.h264"}, false, folder + "/h264-mixed.clip");
  const std::string h265 =
      Exists(folder + "/h265-720.h265") ? WriteClip({folder + "/h265-720.h265"}, true, folder + "/h265-720.clip") : "";
  const std::string big =
      Exists(folder + "/h264-large.h264") ? WriteClip({folder + "/h264-large.h264"}, false, folder + "/h264-large.clip") : "";
  const std::string uhd =
      Exists(folder + "/h264-2160.h264") ? WriteClip({folder + "/h264-2160.h264"}, false, folder + "/h264-2160.clip") : "";
  if (h264.empty() || mixed.empty()) {
    fprintf(stderr, "no H.264 clips in %s\n", folder.c_str());
    return 2;
  }

  PbVideoProbe probe{};
  char error[256] = "";
  const auto probe_started = Clock::now();
  const int32_t probed = module->probe(&probe, error, sizeof(error));
  printf("probe (%lld ms): H.264 %s %ux%u%s; H.265 %s %ux%u%s %s\n",
         static_cast<long long>(std::chrono::duration_cast<milliseconds>(Clock::now() - probe_started).count()),
         probe.h264.decoder, probe.h264.max_width, probe.h264.max_height, probe.h264.hardware ? " hardware" : "",
         probe.h265.decoder, probe.h265.max_width, probe.h265.max_height, probe.h265.hardware ? " hardware" : "",
         error);
  Check(probed == 0 && probe.h264.max_width > 0, "the probe finds an H.264 decoder");
  if (broken) Check(probe.h264.hardware != 0, "the registry offers a hardware H.264 decoder to break");

  Raster raster(module);
  PbVideoDecoder* decoder = module->create(&shims, Raster::Ready, &raster);
  Check(decoder != nullptr, "a decoder is created");
  if (!decoder) return 1;
  raster.Start(decoder);
  auto stats = [&] { return Stats(module, decoder); };
  // Attaching stops the previous source's decoder thread on the caller's
  // (the GTK) thread: how long that holds the caller.
  milliseconds longest_attach{0};
  auto attach = [&](uint64_t source) {
    const auto started = Clock::now();
    const int64_t consumer = source ? module->attach(decoder, source) : 0;
    longest_attach = std::max(longest_attach, std::chrono::duration_cast<milliseconds>(Clock::now() - started));
    return consumer > 0;
  };

  printf("H.264 1280x720 at 60 fps\n");
  uint64_t source = g_ffi.source(h264.c_str(), 60);
  Check(attach(source), "the decoder attaches to the test source");
  Check(WaitFor([] { return g_accepted >= 90; }, milliseconds(6000)), "90 receipts are accepted by Rust");
  printf("  %s\n", raster.Pacing().c_str());
  {
    uint32_t width = 0, height = 0;
    bool content = false;
    raster.Last(&width, &height, &content);
    Check(width == 1280 && height == 720 && content, "pictures are 1280x720 RGBA with content");
    std::lock_guard<std::mutex> lock(g_text_mutex);
    printf("  decoder: %s (%s)\n", g_decoder.c_str(), g_hardware ? "hardware" : "software");
    if (broken) {
      Check(!g_hardware, "a hardware decoder that does not start gives way to a software one the rule allows");
    } else {
      Check(g_decoder == probe.h264.decoder, "the probed decoder is the one configured");
    }
  }
  Check(stats().configurations == 1 && g_reports == 1 && g_failures == 0, "one configuration, reported, no failure");

  printf("a discontinuity on a delta unit\n");
  const uint64_t configured = stats().configurations;
  const int gaps = g_gaps;
  const uint64_t dropped = stats().dropped;
  g_break_next_delta = true;
  Check(WaitFor([] { return g_broken_frame != 0; }, milliseconds(1000)), "a delta unit is marked");
  const int accepted = g_accepted;
  // The clip has a keyframe every 60 units: decoding resumes within a second.
  Check(WaitFor([&] { return g_accepted >= accepted + 30; }, milliseconds(3000)), "decoding resumes at the next keyframe");
  Check(g_gaps > gaps && stats().dropped > dropped, "the units before it are dropped and a keyframe is asked for (gap)");
  Check(stats().configurations == configured && g_failures == 0, "the pipeline is kept: no rebuild, no failure");

  printf("pause and resume\n");
  g_ffi.pause(source, 1);
  std::this_thread::sleep_for(milliseconds(300));
  const uint64_t paused_decoded = stats().decoded;
  std::this_thread::sleep_for(milliseconds(300));
  Check(stats().decoded <= paused_decoded + 1, "nothing decodes while paused");
  const int resumed = g_accepted;
  g_ffi.pause(source, 0);
  Check(WaitFor([&] { return g_accepted >= resumed + 60; }, milliseconds(3000)), "decoding resumes after the pause");
  Check(stats().configurations == configured && g_failures == 0 && g_stalls == 0,
        "the resume's discontinuity keeps the pipeline");
  g_ffi.stop(source);

  printf("one source changing 1280x720 -> 1920x1080 at keyframes\n");
  source = g_ffi.source(mixed.c_str(), 60);
  const uint64_t before = stats().configurations;
  Check(attach(source), "the decoder attaches to the next source");
  bool large = false, small = false;
  WaitFor([&] {
    uint32_t width = 0, height = 0;
    bool content = false;
    raster.Last(&width, &height, &content);
    large |= width == 1920 && height == 1080 && content;
    small |= width == 1280 && height == 720 && content;
    return large && small && stats().configurations >= before + 3;
  }, milliseconds(8000));
  Check(large && small, "pictures of both sizes are shown");
  Check(stats().configurations >= before + 3 && g_failures == 0, "each size change rebuilds the pipeline, no failure");
  g_ffi.stop(source);

  if (!big.empty()) {
    printf("a keyframe larger than the first buffer\n");
    source = g_ffi.source(big.c_str(), 30);
    const int start = g_accepted;
    Check(attach(source), "the decoder attaches to the next source");
    Check(WaitFor([&] { return g_accepted >= start + 30; }, milliseconds(5000)), "30 receipts are accepted");
    Check(g_need_capacity > 0 && g_failures == 0, "the unit waited in Rust for a larger buffer, then decoded");
    g_ffi.stop(source);
  }

  // A session the rule leaves no decoder for: every keyframe is a failure
  // Rust counts toward ending the session (then Dart drops H.265), and no
  // decoder outside the rule is configured.
  auto refused = [&](uint64_t source, const char* what) {
    const int failures = g_failures, reports = g_reports, accepted = g_accepted;
    Check(attach(source), "the decoder attaches");
    Check(WaitFor([&] { return g_failures > failures; }, milliseconds(3000)), what);
    std::this_thread::sleep_for(milliseconds(500));
    Check(g_reports == reports && g_accepted == accepted, "no decoder is configured for it, nothing is shown");
    g_ffi.stop(source);
  };

  if (!h265.empty() && (broken || probe.h265.max_width == 0)) {
    printf("H.265 with no hardware decoder that works\n");
    refused(g_ffi.source(h265.c_str(), 60), "it fails: no software H.265, reported with failed_v1");
  } else if (!h265.empty()) {
    printf("H.265 1280x720 at 60 fps\n");
    source = g_ffi.source(h265.c_str(), 60);
    raster.Pacing();
    const int start = g_accepted;
    Check(attach(source), "the decoder attaches to the H.265 source");
    Check(WaitFor([&] { return g_accepted >= start + 90; }, milliseconds(6000)), "90 H.265 receipts are accepted");
    printf("  %s\n", raster.Pacing().c_str());
    {
      std::lock_guard<std::mutex> lock(g_text_mutex);
      printf("  decoder: %s (%s)\n", g_decoder.c_str(), g_hardware ? "hardware" : "software");
    }
    Check(g_failures == 0, "no failure");
    g_ffi.stop(source);
  }

  if (!uhd.empty() && (broken || probe.h264.max_width < 3840)) {
    printf("3840x2160 H.264 with no hardware decoder that works\n");
    refused(g_ffi.source(uhd.c_str(), 60), "it fails: software H.264 stops at 1080p, reported with failed_v1");
  } else if (!uhd.empty()) {
    // A measurement, not a check: how many 3840x2160 pictures a second the
    // decoder and the conversion to RGBA keep up with, offered 120.
    source = g_ffi.source(uhd.c_str(), 120);
    attach(source);
    WaitFor([&] { return stats().decoded > 0 && raster.pictures() > 0; }, milliseconds(5000));
    const PbVideoStats first = stats();
    const auto started = Clock::now();
    std::this_thread::sleep_for(milliseconds(3000));
    const PbVideoStats last = stats();
    const double seconds = std::chrono::duration<double>(Clock::now() - started).count();
    printf("3840x2160 offered at 120 fps: %.1f decoded/s, %.1f presented/s, %.1f dropped/s (%s)\n",
           (last.decoded - first.decoded) / seconds, (last.presented - first.presented) / seconds,
           (last.dropped - first.dropped) / seconds, last.decoder);
    g_ffi.stop(source);
  }

  if (broken) {
    PbVideoProbe after{};
    module->probe(&after, error, sizeof(error));
    Check(after.h264.hardware == 0 && after.h264.max_width == 1920 && after.h265.max_width == 0,
          "the probe stops offering what failed: software H.264 at 1080p, no H.265");
  }

  printf("the longest attach, stopping the previous decoder thread: %lld ms\n",
         static_cast<long long>(longest_attach.count()));
  module->detach(decoder);
  raster.Stop();
  const PbVideoStats end = stats();
  printf("totals: submitted %llu, decoded %llu, presented %llu, dropped %llu, configurations %llu, recoveries %llu\n",
         static_cast<unsigned long long>(end.submitted), static_cast<unsigned long long>(end.decoded),
         static_cast<unsigned long long>(end.presented), static_cast<unsigned long long>(end.dropped),
         static_cast<unsigned long long>(end.configurations), static_cast<unsigned long long>(end.recoveries));
  module->destroy(decoder);
  printf("%s: %d check(s) failed\n", g_checks_failed ? "FAILED" : "PASSED", g_checks_failed);
  return g_checks_failed ? 1 : 0;
}
