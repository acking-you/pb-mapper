// Decodes recorded clips through libpb_mapper_desktop_video.so outside
// Flutter: libpb_mapper_ffi.so's test source (feature desktop-test-source)
// plays them into a video source, the module pulls, decodes and publishes
// them, and a thread standing in for Flutter's raster thread acquires each
// picture, which is its presentation receipt, and converts it to RGBA with
// the runner's GL code (desktop_video_gl.cc) in a GL ES context with no
// surface (EGL). It checks the decoder state rules the runner shares with
// Windows and Android and the conversion against GStreamer's own, and
// measures how many pictures a second each size decodes and presents and
// what each thread's CPU costs per picture. Run by
// scripts/test/linux-desktop-video.sh, which builds the library and the
// clips.
//
// desktop_video_test <libpb_mapper_ffi.so> <clip folder>
#include <dirent.h>
#include <dlfcn.h>
#include <epoxy/egl.h>
#include <epoxy/gl.h>
#include <pthread.h>
#include <sys/resource.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <functional>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "desktop_video_gl.h"
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

#ifndef EGL_PLATFORM_SURFACELESS_MESA
#define EGL_PLATFORM_SURFACELESS_MESA 0x31DD
#endif

// A GL ES 2 context with no surface, as Flutter's raster thread has one
// (fl_opengl_manager.cc asks for client version 2): llvmpipe where there
// is no GPU.
class GlContext {
 public:
  GlContext() = default;
  GlContext(const GlContext&) = delete;
  GlContext& operator=(const GlContext&) = delete;
  ~GlContext() {
    if (context_ != EGL_NO_CONTEXT) eglDestroyContext(display_, context_);
    if (display_ != EGL_NO_DISPLAY) eglTerminate(display_);
  }
  bool Create(std::string* why) {
    // Mesa's surfaceless platform, which falls back to llvmpipe without a
    // GPU; else whatever display EGL picks.
    EGLint major = 0, minor = 0;
    if (epoxy_has_egl_extension(EGL_NO_DISPLAY, "EGL_MESA_platform_surfaceless")) {
      display_ = eglGetPlatformDisplayEXT(EGL_PLATFORM_SURFACELESS_MESA, nullptr, nullptr);
      if (display_ != EGL_NO_DISPLAY && !eglInitialize(display_, &major, &minor)) display_ = EGL_NO_DISPLAY;
    }
    if (display_ == EGL_NO_DISPLAY) {
      display_ = eglGetDisplay(EGL_DEFAULT_DISPLAY);
      if (display_ != EGL_NO_DISPLAY && !eglInitialize(display_, &major, &minor)) display_ = EGL_NO_DISPLAY;
    }
    if (display_ == EGL_NO_DISPLAY) {
      *why = "no EGL display initializes";
      return false;
    }
    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
      *why = "no GL ES in EGL";
      return false;
    }
    const EGLint config_attributes[] = {EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE};
    EGLConfig config = nullptr;
    EGLint configs = 0;
    if (!eglChooseConfig(display_, config_attributes, &config, 1, &configs) || configs < 1) config = nullptr;
    const EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    context_ = eglCreateContext(display_, config, EGL_NO_CONTEXT, context_attributes);
    if (context_ == EGL_NO_CONTEXT) {
      *why = "cannot create a GL ES 2 context";
      return false;
    }
    return true;
  }
  // On the thread that renders, as Flutter makes its context current there.
  bool MakeCurrent() { return eglMakeCurrent(display_, EGL_NO_SURFACE, EGL_NO_SURFACE, context_) == EGL_TRUE; }
  void Release() { eglMakeCurrent(display_, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT); }
  EGLDisplay display() const { return display_; }

 private:
  EGLDisplay display_ = EGL_NO_DISPLAY;
  EGLContext context_ = EGL_NO_CONTEXT;
};

// The renderer and version behind a context (llvmpipe on CI), read before
// the raster thread makes it current.
std::string Renderer(GlContext* gl) {
  if (!gl->MakeCurrent()) return "a context that cannot be made current";
  const auto* renderer = reinterpret_cast<const char*>(glGetString(GL_RENDERER));
  const auto* version = reinterpret_cast<const char*>(glGetString(GL_VERSION));
  const std::string text = std::string(renderer ? renderer : "an unnamed renderer") + ", " + (version ? version : "");
  gl->Release();
  return text;
}

// The calling thread's CPU time so far, in microseconds.
uint64_t ThreadCpuUs() {
  struct rusage usage {};
  if (getrusage(RUSAGE_THREAD, &usage) != 0) return 0;
  return static_cast<uint64_t>(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1000000u +
         static_cast<uint64_t>(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec);
}

// Every thread of this process: its name and CPU time so far, in
// nanoseconds (schedstat), or from its tick counts (stat) where the kernel
// keeps no schedstat. GStreamer's and the decoders' threads are not this
// harness's to ask with getrusage(RUSAGE_THREAD).
struct ThreadTime {
  std::string name;
  uint64_t ns = 0;
};
std::string ReadLine(const std::string& path) {
  std::ifstream in(path);
  std::string line;
  std::getline(in, line);
  return line;
}
std::map<long, ThreadTime> ReadThreads() {
  std::map<long, ThreadTime> threads;
  DIR* dir = opendir("/proc/self/task");
  if (!dir) return threads;
  const long ticks = sysconf(_SC_CLK_TCK);
  while (const struct dirent* entry = readdir(dir)) {
    const long tid = strtol(entry->d_name, nullptr, 10);
    if (tid <= 0) continue;
    const std::string task = std::string("/proc/self/task/") + entry->d_name;
    ThreadTime time;
    time.name = ReadLine(task + "/comm");
    const std::string schedstat = ReadLine(task + "/schedstat");
    if (!schedstat.empty()) {
      time.ns = strtoull(schedstat.c_str(), nullptr, 10);
    } else if (ticks > 0) {
      // utime and stime are the 12th and 13th fields after the name, which
      // is in parentheses and may hold spaces.
      const std::string stat = ReadLine(task + "/stat");
      const size_t close = stat.rfind(')');
      if (close == std::string::npos) continue;
      std::vector<std::string> fields;
      size_t at = close + 2;
      while (at < stat.size() && fields.size() < 13) {
        const size_t end = stat.find(' ', at);
        fields.push_back(stat.substr(at, end == std::string::npos ? std::string::npos : end - at));
        if (end == std::string::npos) break;
        at = end + 1;
      }
      if (fields.size() < 13) continue;
      const uint64_t used = strtoull(fields[11].c_str(), nullptr, 10) + strtoull(fields[12].c_str(), nullptr, 10);
      time.ns = used * 1000000000u / static_cast<uint64_t>(ticks);
    }
    threads[tid] = time;
  }
  closedir(dir);
  return threads;
}

// What each thread name spent between two reads, per decoded picture, in
// milliseconds, largest first; a thread that started meanwhile counts from
// zero, one that ended is missed.
std::string CpuPerPicture(const std::map<long, ThreadTime>& before, const std::map<long, ThreadTime>& after,
                          uint64_t pictures) {
  if (pictures == 0) return "no pictures";
  std::map<std::string, uint64_t> by_name;
  for (const auto& entry : after) {
    const auto found = before.find(entry.first);
    const uint64_t start = found == before.end() ? 0 : found->second.ns;
    if (entry.second.ns > start) by_name[entry.second.name] += entry.second.ns - start;
  }
  std::vector<std::pair<uint64_t, std::string>> spent;
  uint64_t total = 0;
  for (const auto& entry : by_name) {
    spent.emplace_back(entry.second, entry.first);
    total += entry.second;
  }
  std::sort(spent.rbegin(), spent.rend());
  char line[160];
  snprintf(line, sizeof(line), "%.2f ms in all", total / 1e6 / static_cast<double>(pictures));
  std::string text = line;
  for (const auto& entry : spent) {
    const double ms = entry.first / 1e6 / static_cast<double>(pictures);
    if (ms < 0.01) continue;
    snprintf(line, sizeof(line), "; %s %.2f", entry.second.c_str(), ms);
    text += line;
  }
  return text;
}

// Whether an RGBA picture is more than one colour, as the test pattern is.
bool Varied(const std::vector<uint8_t>& rgba) {
  for (size_t i = 4; i + 3 < rgba.size(); i += 4) {
    if (std::abs(rgba[i] - rgba[0]) > 8 || std::abs(rgba[i + 1] - rgba[1]) > 8 || std::abs(rgba[i + 2] - rgba[2]) > 8) {
      return true;
    }
  }
  return false;
}

// The peak signal-to-noise ratio of two RGBA pictures' colour, in dB.
double Psnr(const std::vector<uint8_t>& a, const std::vector<uint8_t>& b) {
  if (a.size() != b.size() || a.empty()) return 0;
  double squares = 0;
  size_t samples = 0;
  for (size_t i = 0; i + 3 < a.size(); i += 4) {
    for (size_t c = 0; c < 3; ++c) {
      const double difference = static_cast<double>(a[i + c]) - static_cast<double>(b[i + c]);
      squares += difference * difference;
      ++samples;
    }
  }
  if (squares == 0) return 99;
  return 10 * std::log10(255.0 * 255.0 * static_cast<double>(samples) / squares);
}

// What the conversion of one picture gave, checked on the raster thread.
struct Verification {
  bool done = false;
  PbVideoPlanes planes{};
  // GL_TEXTURE_2D, RGBA8 (where GL ES 3.1 can say), the picture's size.
  bool texture = false;
  std::string texture_note;
  // Against GStreamer's converter, for a picture in system memory.
  bool compared = false;
  double psnr = 0;
  bool snapshot_varied = false, still_varied = false;
};

// Flutter's raster thread: acquires the newest picture whenever the module
// says there is one, keeps it until the next, and converts it into the RGBA
// texture with the runner's GL code, as the texture's populate does.
class Raster {
 public:
  Raster(const PbVideoModule* module, GlContext* gl) : module_(module), gl_(gl) {}
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
  // While held, the compositor takes nothing, so a published picture waits
  // untaken; released, it takes the newest at once.
  void Hold(bool held) {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      held_ = held;
      if (!held) ready_ = true;
    }
    wake_.notify_one();
  }
  int pictures() const { return pictures_; }
  // This thread's CPU time so far (getrusage), in microseconds.
  uint64_t cpu_us() const { return cpu_us_; }
  // Whether its GL context was current and every picture converted.
  bool converted() const { return gl_ready_ && convert_failures_ == 0; }
  std::string failure() {
    std::lock_guard<std::mutex> lock(mutex_);
    return failure_;
  }
  // Checks the conversion of the next picture.
  Verification Verify(milliseconds limit) {
    std::unique_lock<std::mutex> lock(mutex_);
    verification_ = Verification{};
    verify_ = true;
    verified_.wait_for(lock, limit, [this] { return verification_.done; });
    verify_ = false;
    return verification_;
  }
  // The size of the picture shown last, and whether it has content.
  void Last(uint32_t* width, uint32_t* height, bool* content) {
    std::lock_guard<std::mutex> lock(mutex_);
    *width = width_;
    *height = height_;
    *content = content_;
  }

 private:
  void Run() {
    pthread_setname_np(pthread_self(), "raster");
    gl_ready_ = gl_->MakeCurrent();
    std::unique_ptr<pb_video::VideoGl> converter(gl_ready_ ? new pb_video::VideoGl(module_) : nullptr);
    while (true) {
      {
        std::unique_lock<std::mutex> lock(mutex_);
        wake_.wait(lock, [this] { return (ready_ && !held_) || stopping_; });
        if (stopping_) break;
        ready_ = false;
      }
      PbVideoPicture* picture = module_->acquire(decoder_);
      if (!picture) continue;
      if (picture == shown_) {
        module_->release(picture);
        continue;
      }
      PbVideoPlanes planes{};
      module_->describe(picture, &planes);
      std::string failure;
      const bool converted = converter && converter->Convert(picture, &failure);
      if (converter && converter->TakeDmabufRefusal()) module_->refuse_dmabuf(decoder_);
      uint32_t name = 0, width = 0, height = 0;
      if (converted && !converter->Texture(&name, &width, &height, &failure)) name = 0;
      if (!converted || name == 0) ++convert_failures_;
      // The test pattern is never one colour: its luma varies, read where
      // it is in memory (a dma-buf's is checked by reading the texture).
      bool content = converted && planes.dmabuf;
      const PbVideoPlane& luma = planes.planes[0];
      for (size_t row = 1; !planes.dmabuf && luma.data && luma.width && !content && row < luma.height; row += 97) {
        content = luma.data[row * luma.stride + (row * 7) % luma.width] != luma.data[0];
      }
      bool verify = false;
      {
        std::lock_guard<std::mutex> lock(mutex_);
        verify = verify_ && !verification_.done;
        if (!converted && failure_.empty()) failure_ = failure;
      }
      if (verify && converted) {
        Verification checked = Inspect(converter.get(), picture, planes, name, width, height);
        std::lock_guard<std::mutex> lock(mutex_);
        verification_ = checked;
        verified_.notify_all();
      }
      if (shown_) module_->release(shown_);
      shown_ = picture;
      ++pictures_;
      cpu_us_ = ThreadCpuUs();
      std::lock_guard<std::mutex> lock(mutex_);
      width_ = width;
      height_ = height;
      content_ = content;
    }
    converter.reset();
    if (gl_ready_) gl_->Release();
  }

  // Reads the converted texture back and compares it with GStreamer's
  // converter on the same picture; checks both snapshots.
  Verification Inspect(pb_video::VideoGl* converter, PbVideoPicture* picture, const PbVideoPlanes& planes, GLuint name,
                     uint32_t width, uint32_t height) {
    Verification result;
    result.done = true;
    result.planes = planes;
    result.texture = width == planes.width && height == planes.height && name != 0 && glIsTexture(name);
    if (!epoxy_is_desktop_gl() && epoxy_gl_version() >= 31) {
      GLint internal = 0, levelled_width = 0, levelled_height = 0;
      glBindTexture(GL_TEXTURE_2D, name);
      glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_INTERNAL_FORMAT, &internal);
      glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &levelled_width);
      glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &levelled_height);
      glBindTexture(GL_TEXTURE_2D, 0);
      result.texture = result.texture && internal == GL_RGBA8 && levelled_width == static_cast<GLint>(width) &&
                       levelled_height == static_cast<GLint>(height) && glGetError() == GL_NO_ERROR;
      result.texture_note = "internal format checked";
    } else {
      result.texture_note = "GL ES before 3.1 cannot say its internal format";
    }
    // The whole texture, rows top first as the picture's.
    std::vector<uint8_t> converted(static_cast<size_t>(width) * height * 4);
    GLuint framebuffer = 0;
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, name, 0);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE) {
      glReadPixels(0, 0, static_cast<GLsizei>(width), static_cast<GLsizei>(height), GL_RGBA, GL_UNSIGNED_BYTE,
                   converted.data());
    }
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDeleteFramebuffers(1, &framebuffer);
    if (!planes.dmabuf) {
      std::vector<uint8_t> reference(converted.size());
      uint32_t reference_width = 0, reference_height = 0;
      result.compared = module_->snapshot(picture, planes.width, reference.data(), reference.size(), &reference_width,
                                          &reference_height) == 0 &&
                        reference_width == width && reference_height == height;
      if (result.compared) result.psnr = Psnr(converted, reference);
      std::vector<uint8_t> snapshot(320 * 320 * 4);
      uint32_t snapshot_width = 0, snapshot_height = 0;
      result.snapshot_varied = module_->snapshot(picture, 320, snapshot.data(), snapshot.size(), &snapshot_width,
                                                 &snapshot_height) == 0 &&
                               snapshot_width == 320 && Varied(snapshot);
    }
    std::vector<uint8_t> still;
    uint32_t still_width = 0, still_height = 0;
    result.still_varied =
        converter->ReadBack(320, &still, &still_width, &still_height) && still_width == 320 && Varied(still);
    return result;
  }

  const PbVideoModule* module_;
  GlContext* const gl_;
  bool gl_ready_ = false;
  std::atomic<int> convert_failures_{0};
  std::atomic<uint64_t> cpu_us_{0};
  bool verify_ = false;
  Verification verification_;
  std::condition_variable verified_;
  std::string failure_;
  PbVideoDecoder* decoder_ = nullptr;
  std::thread thread_;
  std::mutex mutex_;
  std::condition_variable wake_;
  bool ready_ = false, stopping_ = false, content_ = false, held_ = false;
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
  module->stats(decoder, 1, &stats);
  return stats;
}

double MeanMs(const PbVideoTiming& first, const PbVideoTiming& last) {
  if (last.samples <= first.samples) return 0;
  return (last.total_us - first.total_us) / 1000.0 / static_cast<double>(last.samples - first.samples);
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
  const std::string fhd = WriteClip({folder + "/h264-1080.h264"}, false, folder + "/h264-1080.clip");
  if (h264.empty() || mixed.empty() || fhd.empty()) {
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

  GlContext gl;
  std::string why;
  const bool has_gl = gl.Create(&why);
  Check(has_gl, has_gl ? "a GL ES context with no surface stands in for Flutter's" : why.c_str());
  if (!has_gl) return 1;

  // What this display samples in place, as the runner reads Flutter's.
  PbVideoPresenter presenter{};
  pb_video::ReadPresenter(gl.display(), &presenter);
  printf("the GL context: %s; dma-buf import %s (%u layouts)\n", Renderer(&gl).c_str(),
         presenter.dmabuf ? "yes" : "no", presenter.modifier_count);

  Raster raster(module, &gl);
  PbVideoDecoder* decoder = module->create(&shims, &presenter, Raster::Ready, &raster);
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

  // A measurement, not a check: each size offered at 60 and at 120 fps
  // (Flutter's Linux embedder draws at most 60), as decoded and presented
  // pictures a second, the mean decode time and every thread's CPU per
  // decoded picture. On llvmpipe the compositor's GL work runs on the CPU
  // too, so its share here is no figure for a GPU.
  auto measure = [&](const std::string& clip, const char* size, uint32_t fps) {
    const uint64_t played = g_ffi.source(clip.c_str(), fps);
    if (!attach(played)) {
      printf("%s offered at %u fps: the decoder did not attach\n", size, fps);
      g_ffi.stop(played);
      return;
    }
    const int taken = raster.pictures();
    WaitFor([&] { return raster.pictures() > taken + 10; }, milliseconds(5000));
    const PbVideoStats first = stats();
    const auto threads = ReadThreads();
    const uint64_t raster_cpu = raster.cpu_us();
    const auto started = Clock::now();
    std::this_thread::sleep_for(milliseconds(3000));
    const PbVideoStats last = stats();
    const auto threads_after = ReadThreads();
    const uint64_t raster_after = raster.cpu_us();
    const double seconds = std::chrono::duration<double>(Clock::now() - started).count();
    const uint64_t decoded = last.decoded - first.decoded, presented = last.presented - first.presented;
    printf("%s offered at %u fps: %.1f decoded/s, %.1f presented/s, %.1f dropped/s, decode %.2f ms (%s)\n", size, fps,
           decoded / seconds, presented / seconds, (last.dropped - first.dropped) / seconds,
           MeanMs(first.decode, last.decode), last.decoder);
    printf("  CPU per decoded picture: %s\n", CpuPerPicture(threads, threads_after, decoded).c_str());
    printf("  raster thread (getrusage) per presented picture: %.2f ms\n",
           presented ? (raster_after - raster_cpu) / 1000.0 / static_cast<double>(presented) : 0.0);
    g_ffi.stop(played);
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
    Check(width == 1280 && height == 720 && content, "pictures are converted to 1280x720 RGBA, with content");
    Check(raster.converted(), raster.converted() ? "every picture is converted on the GPU" : raster.failure().c_str());
    std::lock_guard<std::mutex> lock(g_text_mutex);
    printf("  decoder: %s (%s)\n", g_decoder.c_str(), g_hardware ? "hardware" : "software");
    if (broken) {
      Check(!g_hardware, "a hardware decoder that does not start gives way to a software one the rule allows");
    } else {
      Check(g_decoder == probe.h264.decoder, "the probed decoder is the one configured");
    }
  }
  Check(stats().configurations == 1 && g_reports == 1 && g_failures == 0, "one configuration, reported, no failure");

  printf("the GPU conversion\n");
  {
    const Verification checked = raster.Verify(milliseconds(3000));
    const PbVideoStats now = stats();
    const PbVideoPlanes& planes = checked.planes;
    printf("  pictures: %s %s, Kr %.4f Kb %.4f, %s range; %s; %s\n",
           planes.format == PB_VIDEO_FORMAT_NV12   ? "NV12"
           : planes.format == PB_VIDEO_FORMAT_I420 ? "I420"
                                                   : "unknown",
           planes.dmabuf ? "in dma-bufs" : "in system memory", planes.kr, planes.kb,
           planes.full_range ? "full" : "limited",
           (now.flags & PB_VIDEO_STATS_CONVERTS) ? "through videoconvert" : "as the decoder gives them",
           checked.texture_note.c_str());
    Check(checked.done, "a picture is converted and read back");
    Check(checked.texture, "the texture is GL_TEXTURE_2D, RGBA8, 1280x720");
    if (!planes.dmabuf) {
      printf("  PSNR against GStreamer's converter: %.1f dB\n", checked.psnr);
      Check(checked.compared && checked.psnr >= 40, "it matches GStreamer's converter at 40 dB or more");
      Check(checked.snapshot_varied, "the snapshot, scaled to 320 wide, has content");
    }
    Check(checked.still_varied, "the GPU's still, scaled to 320 wide, has content");
    if (!g_hardware) {
      Check(planes.format == PB_VIDEO_FORMAT_I420 && !planes.dmabuf && !(now.flags & PB_VIDEO_STATS_CONVERTS),
            "the software decoder's I420 reaches the GPU with no videoconvert");
    }
  }

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

  printf("a picture still untaken when the next source attaches\n");
  raster.Hold(true);
  const uint64_t held = stats().decoded;
  Check(WaitFor([&] { return stats().decoded > held; }, milliseconds(1000)), "a picture waits untaken");
  // The next source sends nothing yet, so its decoder thread starts no
  // revision of its own before the compositor takes the waiting picture.
  const uint64_t next = g_ffi.source(h264.c_str(), 60);
  g_ffi.pause(next, 1);
  const int receipts = g_receipts;
  const uint64_t presented = stats().presented;
  const int taken = raster.pictures();
  Check(attach(next), "the decoder attaches to the next source");
  g_ffi.stop(source);
  source = next;
  raster.Hold(false);
  Check(WaitFor([&] { return raster.pictures() > taken; }, milliseconds(1000)), "the compositor takes it");
  Check(g_receipts == receipts && stats().presented == presented,
        "the previous source's picture earns no receipt from the next one");
  const int next_start = g_accepted;
  g_ffi.pause(source, 0);
  Check(WaitFor([&] { return g_accepted >= next_start + 30; }, milliseconds(3000)), "the next source's pictures do");
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
    // How many 3840x2160 pictures a second the decoder and the compositor's
    // stand-in keep up with.
    measure(uhd, "3840x2160", 60);
    measure(uhd, "3840x2160", 120);
  }

  if (!broken) {
    for (const uint32_t fps : {60u, 120u}) {
      measure(h264, "1280x720", fps);
      measure(fhd, "1920x1080", fps);
    }
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
