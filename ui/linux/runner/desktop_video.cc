// The `pb_mapper/desktop_video` channel of the Linux runner: native H.264
// and H.265 into Flutter textures, as the Windows runner
// (ui/windows/runner/desktop_video.cpp) and the Android one (DesktopVideo.kt)
// do it. Decoding is GStreamer's, in libpb_mapper_desktop_video.so
// (desktop_video_gst.cc), which this runner loads with dlopen at the first
// probe: without it, or without GStreamer, `decoders` answers none and
// every session is JPEG, which Dart decodes. The app never needs GStreamer
// to start. Each texture is an FlTextureGL whose populate, on Flutter's
// raster thread, converts the decoder's picture to RGBA on the GPU
// (desktop_video_gl.cc). `refresh_hz` answers the refresh of the display
// the view's window is on, and `display_changed` tells Dart when that may
// have changed, as on Windows: Flutter's embedder follows neither.
// A viewer that asks for the plane (`presenter`
// `plane`) has its first texture's pictures shown by the video plane under
// the Flutter view instead (desktop_video_plane.cc), which converts and
// draws them on the GTK thread whenever GTK draws the window (at the
// display's refresh where the compositor paces it); Flutter then never
// populates that texture.
#define G_LOG_DOMAIN "pb-mapper-video"

#include "desktop_video.h"

#include <dlfcn.h>
#include <fcntl.h>
#include <link.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "desktop_video_gl.h"
#include "desktop_video_module.h"
#include "desktop_video_plane.h"

// Last: Xlib's macros (None, Status) would reach everything after.
#ifdef GDK_WINDOWING_WAYLAND
#include <gdk/gdkwayland.h>
#endif
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

namespace {
using Clock = std::chrono::steady_clock;

constexpr char kChannel[] = "pb_mapper/desktop_video";
// The view's data that holds its DesktopVideo.
constexpr char kViewData[] = "pb-mapper-desktop-video";
constexpr char kFfiFile[] = "libpb_mapper_ffi.so";
constexpr int64_t kSnapshotWidth = 320;
constexpr char kSnapshotQuality[] = "80";
constexpr auto kSnapshotDeadline = std::chrono::milliseconds(300);
constexpr size_t kMaxTextures = 2;
// Flutter's raster thread looks a texture up and then populates it without
// holding a reference; a populate already running when the texture is
// unregistered still reads it. The last reference goes this much later.
constexpr guint kTextureReleaseMs = 1000;
// How often a dma-buf picture, which only the GPU reads, is read back for
// the snapshot; the last one before the picture stops changing is read
// back this long after the one before it.
constexpr auto kStillInterval = std::chrono::seconds(1);
// How long a snapshot waits for the raster thread to read back the dma-buf
// picture on screen when its still is older: well inside the snapshot's
// deadline, which the JPEG still has to meet.
constexpr gint64 kStillWaitUs = 100 * G_TIME_SPAN_MILLISECOND;
// No placement Dart sends reaches further, in physical pixels.
constexpr int64_t kMaxPlaneCoordinate = int64_t{1} << 24;

// The video C ABI of libpb_mapper_ffi.so. The runner does not link it: it
// resolves the exports from the copy Dart already loaded, which stays
// loaded for the life of the process.
struct VideoLibrary {
  PbVideoExports exports{};
  uint64_t (*instance_token)() = nullptr;
};

int FindFfi(struct dl_phdr_info* info, size_t, void* data) {
  const char* name = info->dlpi_name;
  if (!name || !*name) return 0;
  const char* base = strrchr(name, '/');
  if (strcmp(base ? base + 1 : name, kFfiFile) != 0) return 0;
  static_cast<std::string*>(data)->assign(name);
  return 1;
}

template <typename Fn>
bool Resolve(void* handle, Fn& fn, const char* name) {
  fn = reinterpret_cast<Fn>(dlsym(handle, name));
  return fn != nullptr;
}

// Finds the library Dart opened, by the name the dynamic linker recorded,
// and pins it. RTLD_NOLOAD never loads a second copy, which a plain dlopen
// could: Rust state would then split in two.
VideoLibrary LoadVideoLibrary() {
  std::string path;
  dl_iterate_phdr(FindFfi, &path);
  void* handle = path.empty() ? nullptr : dlopen(path.c_str(), RTLD_NOW | RTLD_NOLOAD);
  if (!handle) throw std::runtime_error("libpb_mapper_ffi.so is not loaded");
  VideoLibrary library;
  PbVideoExports& e = library.exports;
  if (!Resolve(handle, e.attach, "pb_mapper_desktop_video_attach_v1") ||
      !Resolve(handle, e.next, "pb_mapper_desktop_video_next_v1") ||
      !Resolve(handle, e.presented, "pb_mapper_desktop_video_presented_v1") ||
      !Resolve(handle, e.keyframe, "pb_mapper_desktop_video_keyframe_v1") ||
      !Resolve(handle, e.failed, "pb_mapper_desktop_video_failed_v1") ||
      !Resolve(handle, e.report, "pb_mapper_desktop_video_report_v1") ||
      !Resolve(handle, e.detach, "pb_mapper_desktop_video_detach_v1") ||
      !Resolve(handle, library.instance_token, "pb_mapper_desktop_instance_token_v1")) {
    throw std::runtime_error("libpb_mapper_ffi.so does not match this app");
  }
  return library;
}

// The decoder module, loaded once and never unloaded: its pictures and
// GStreamer's threads may outlive any texture. Null when it is not in the
// bundle, GStreamer is not installed, or it belongs to another build.
const PbVideoModule* LoadModule() {
  static std::once_flag once;
  static const PbVideoModule* module = nullptr;
  std::call_once(once, [] {
    // Found next to libpb_mapper_ffi.so, through this executable's
    // RUNPATH ($ORIGIN/lib).
    void* handle = dlopen(PB_VIDEO_MODULE_FILE, RTLD_NOW | RTLD_LOCAL);
    if (!handle) {
      g_message("Native desktop video is unavailable: %s", dlerror());
      return;
    }
    auto entry = reinterpret_cast<PbVideoModuleEntry>(dlsym(handle, PB_VIDEO_MODULE_SYMBOL));
    const PbVideoModule* found = entry ? entry() : nullptr;
    if (!found || found->abi != PB_VIDEO_MODULE_ABI) {
      g_message("Native desktop video is unavailable: %s does not match this app", PB_VIDEO_MODULE_FILE);
      dlclose(handle);
      return;
    }
    module = found;
  });
  return module;
}

// Flutter's software renderer draws no external texture.
bool SoftwareRenderer() { return g_strcmp0(g_getenv("FLUTTER_LINUX_RENDERER"), "software") == 0; }

// Flutter's EGL display, initialized, or EGL_NO_DISPLAY. GTK thread only.
// Flutter renders through EGL on the display GDK opened
// (fl_opengl_manager.cc), whose handle this asks for again; it is
// initialized already, initializing it again changes nothing, and nothing
// here terminates it.
EGLDisplay FlutterDisplay() {
  EGLDisplay display = EGL_NO_DISPLAY;
  GdkDisplay* gdk = gdk_display_get_default();
  if (gdk && epoxy_has_egl_extension(EGL_NO_DISPLAY, "EGL_EXT_platform_base")) {
#ifdef GDK_WINDOWING_WAYLAND
    if (GDK_IS_WAYLAND_DISPLAY(gdk)) {
      display = eglGetPlatformDisplayEXT(EGL_PLATFORM_WAYLAND_EXT, gdk_wayland_display_get_wl_display(gdk), nullptr);
    }
#endif
#ifdef GDK_WINDOWING_X11
    if (GDK_IS_X11_DISPLAY(gdk)) {
      display = eglGetPlatformDisplayEXT(EGL_PLATFORM_X11_EXT, gdk_x11_display_get_xdisplay(gdk), nullptr);
    }
#endif
  }
  return display != EGL_NO_DISPLAY && eglInitialize(display, nullptr, nullptr) ? display : EGL_NO_DISPLAY;
}

// What Flutter's EGL display samples in place, read on the GTK thread
// before a decoder starts. PB_MAPPER_VIDEO_DMABUF=0 keeps every picture in
// system memory.
PbVideoPresenter ReadPresenter() {
  PbVideoPresenter presenter{};
  if (g_strcmp0(g_getenv("PB_MAPPER_VIDEO_DMABUF"), "0") == 0) return presenter;
  const EGLDisplay display = FlutterDisplay();
  if (display != EGL_NO_DISPLAY) pb_video::ReadPresenter(display, &presenter);
  g_debug("The presenter samples dma-bufs: %s, %u layouts", presenter.dmabuf ? "yes" : "no", presenter.modifier_count);
  return presenter;
}

// The DRM device file of the GPU Flutter renders on, read on the GTK thread
// before the probe and before each decoder starts: the module tries the
// decoders on that GPU first. Empty when EGL does not say, and under the
// software renderer, which decodes nothing natively.
std::string PresentingDevice() {
  if (SoftwareRenderer()) return "";
  const std::string device = pb_video::PresentingDevice(FlutterDisplay());
  g_debug("Flutter renders on %s", device.empty() ? "a GPU EGL does not name" : device.c_str());
  return device;
}

// The fastest refresh among the monitors as they are set now: the viewer
// presents at its display's rate, and the window may move to the fastest.
// GTK thread only.
double DisplayMaxHz() {
  double fastest = 0;
  GdkDisplay* display = gdk_display_get_default();
  for (int i = 0; display && i < gdk_display_get_n_monitors(display); ++i) {
    fastest = std::max(fastest, gdk_monitor_get_refresh_rate(gdk_display_get_monitor(display, i)) / 1000.0);
  }
  return fastest > 1 ? fastest : 60.0;
}

// The monitor with the most of `view`'s window, as Flutter's embedder names
// the view's display, which it does only when the view's size or scale
// changes; null before the window is realized. GTK thread only.
GdkMonitor* WindowMonitor(GtkWidget* view) {
  GdkWindow* window = gtk_widget_get_window(gtk_widget_get_toplevel(view));
  return window ? gdk_display_get_monitor_at_window(gtk_widget_get_display(view), window) : nullptr;
}

// The refresh of the monitor `view`'s window is on now, exactly as GDK
// reads it (59.94 Hz as 59940 mHz); 0 when that says nothing. GTK thread
// only.
double WindowRefreshHz(GtkWidget* view) {
  GdkMonitor* monitor = WindowMonitor(view);
  const int millihertz = monitor ? gdk_monitor_get_refresh_rate(monitor) : 0;
  return millihertz > 1000 ? millihertz / 1000.0 : 0;
}

FlValue* Limit(const PbVideoCodecProbe& codec) {
  if (codec.max_width == 0 || codec.max_height == 0) return fl_value_new_null();
  FlValue* limit = fl_value_new_map();
  fl_value_set_string_take(limit, "max_width", fl_value_new_int(codec.max_width));
  fl_value_set_string_take(limit, "max_height", fl_value_new_int(codec.max_height));
  return limit;
}

std::string Describe(const char* codec, const PbVideoCodecProbe& probe) {
  if (probe.max_width == 0) return std::string(codec) + " none";
  return std::string(codec) + " " + probe.decoder + (probe.hardware ? ", hardware" : ", software") + ", up to " +
         std::to_string(probe.max_width) + "x" + std::to_string(probe.max_height);
}

// The `decoders` reply: which codecs this computer decodes natively, each up
// to the largest bound it takes, or null, and the fastest display refresh.
// It reads the GStreamer registry and creates no decoder: a bad driver can
// hang there. Off the GTK thread. `device`: the presenting GPU's
// (PresentingDevice).
FlValue* ProbeDecoders(double display_max_hz, const std::string& device) {
  PbVideoProbe probe{};
  const PbVideoModule* module = SoftwareRenderer() ? nullptr : LoadModule();
  if (module) {
    char error[256] = "";
    if (module->probe(device.c_str(), &probe, error, sizeof(error)) != 0) {
      probe = PbVideoProbe{};
      g_message("Native desktop video is unavailable: %s", error);
    } else {
      g_message("Native desktop video: %s; %s%s%s", Describe("H.264", probe.h264).c_str(),
                Describe("H.265", probe.h265).c_str(), *error ? "; " : "", error);
    }
  }
  FlValue* answer = fl_value_new_map();
  fl_value_set_string_take(answer, "h264", Limit(probe.h264));
  fl_value_set_string_take(answer, "h265", Limit(probe.h265));
  fl_value_set_string_take(answer, "display_max_hz", fl_value_new_float(display_max_hz));
  // This runner answers `refresh_hz` and calls `display_changed`, so Dart
  // follows the window's display through it.
  fl_value_set_string_take(answer, "window_refresh", fl_value_new_bool(TRUE));
  return answer;
}

bool WriteAll(int fd, const gchar* data, gsize size) {
  while (size > 0) {
    const ssize_t written = write(fd, data, size);
    if (written < 0 && errno == EINTR) continue;
    if (written <= 0) return false;
    data += written;
    size -= static_cast<gsize>(written);
  }
  return true;
}

// D23: the picture scaled (box filter) to at most `max_width` and encoded as
// JPEG q80. It is written beside the path, flushed, then moved over it, but
// not past the deadline: by then Dart has its answer and the old picture
// stays.
bool WriteSnapshot(const uint8_t* rgba, uint32_t width, uint32_t height, uint32_t max_width,
                   const std::string& path, Clock::time_point deadline) {
  g_autoptr(GdkPixbuf) picture =
      gdk_pixbuf_new_from_data(rgba, GDK_COLORSPACE_RGB, TRUE, 8, static_cast<int>(width), static_cast<int>(height),
                               static_cast<int>(width * 4), nullptr, nullptr);
  const uint32_t scaled_width = std::min(max_width, width);
  const uint32_t scaled_height = std::max<uint32_t>(
      1, static_cast<uint32_t>((static_cast<uint64_t>(height) * scaled_width + width / 2) / width));
  // Bilinear reduction averages every source pixel a target pixel covers.
  g_autoptr(GdkPixbuf) scaled = gdk_pixbuf_scale_simple(picture, static_cast<int>(scaled_width),
                                                        static_cast<int>(scaled_height), GDK_INTERP_BILINEAR);
  g_autofree gchar* jpeg = nullptr;
  gsize size = 0;
  if (!scaled ||
      !gdk_pixbuf_save_to_buffer(scaled, &jpeg, &size, "jpeg", nullptr, "quality", kSnapshotQuality, nullptr)) {
    return false;
  }
  g_autofree gchar* folder = g_path_get_dirname(path.c_str());
  g_mkdir_with_parents(folder, 0755);
  const std::string temporary = path + ".tmp";
  const int fd = open(temporary.c_str(), O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return false;
  const bool flushed = WriteAll(fd, jpeg, size) && fsync(fd) == 0;
  close(fd);
  if (!flushed || Clock::now() > deadline || rename(temporary.c_str(), path.c_str()) != 0) {
    unlink(temporary.c_str());
    return false;
  }
  return true;
}

// One of the stats' timings: count, total and longest, in constant space.
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

// A timing as the other runners report it: count, mean and maximum.
FlValue* TimingValue(const PbVideoTiming& timing) {
  FlValue* value = fl_value_new_map();
  const double mean = timing.samples == 0 ? 0.0 : timing.total_us / 1000.0 / static_cast<double>(timing.samples);
  fl_value_set_string_take(value, "samples", fl_value_new_int(static_cast<int64_t>(timing.samples)));
  fl_value_set_string_take(value, "mean_ms", fl_value_new_float(mean));
  fl_value_set_string_take(value, "max_ms", fl_value_new_float(timing.max_us / 1000.0));
  return value;
}

int64_t Number(FlValue* map, const char* key) {
  FlValue* value = fl_value_lookup_string(map, key);
  if (!value) throw std::runtime_error("Missing video field");
  if (fl_value_get_type(value) != FL_VALUE_TYPE_INT) throw std::runtime_error("Invalid video field");
  return fl_value_get_int(value);
}
}  // namespace

namespace {
// What a texture keeps besides its GObject fields.
struct TextureState {
  // Whether the video plane shows its pictures, which Flutter then never
  // populates: set before the decoder starts, never changed.
  bool plane_mode = false;
  // GTK thread only: the plane, referenced until the texture closes.
  PbPlaneArea* plane = nullptr;
  // Set by frame_ready on a GStreamer thread when a newer picture can be
  // acquired, cleared by the plane taking it.
  std::atomic<bool> plane_pending{false};
  // Flutter's converter, on its raster thread only.
  std::unique_ptr<pb_video::VideoGl> gl;
  // The thread that presents only: Flutter's raster thread, or the GTK
  // thread for the plane.
  Clock::time_point read_back{};
  bool failure_logged = false;
  // Guarded by the texture's mutex. The picture shown last, which only the
  // presenting thread replaces, and whether it is a dma-buf.
  PbVideoPicture* shown = nullptr;
  bool shown_dmabuf = false;
  // While it is a dma-buf: the RGBA still a snapshot writes, read back at
  // most once a second. `still_stale`: a picture shown since is not in it,
  // and a timer (`still_timer` while one is set) brings the raster thread
  // back to read it a second after the last read. `still_wanted`: a
  // snapshot waits for it now. `still_reads` counts the raster thread's
  // reads and attempts, which `still_read` signals.
  std::vector<uint8_t> still;
  uint32_t still_width = 0, still_height = 0;
  bool still_stale = false, still_timer = false, still_wanted = false;
  uint64_t still_reads = 0;
  // While a stats read asks for timings: each conversion's time on the
  // raster thread, and surface_consume, from frame_ready saying a newer
  // picture can be acquired (`ready`, while `waiting`) to populate taking
  // it.
  bool timing = false;
  Timing convert;
  Timing consume;
  Clock::time_point ready{};
  bool waiting = false;
};
}  // namespace

// One texture and its decoder. Flutter's raster thread calls populate,
// which converts each new picture and sends the presentation receipt of
// those it shows; or, in plane mode, the video plane does on the GTK thread.
G_DECLARE_FINAL_TYPE(PbVideoTexture, pb_video_texture, PB, VIDEO_TEXTURE, FlTextureGL)

struct _PbVideoTexture {
  FlTextureGL parent_instance;
  const PbVideoModule* module;
  PbVideoDecoder* decoder;
  FlTextureRegistrar* registrar;
  // Whether a snapshot is being written: one at a time.
  gint snapshotting;
  GMutex mutex;
  GCond still_read;
  TextureState* state;
  // Whether a stats read asked for timings, as state->timing: written under
  // mutex, read without it by frame_ready.
  gint timed;
};

G_DEFINE_TYPE(PbVideoTexture, pb_video_texture, fl_texture_gl_get_type())

// The GTK thread: the presenter takes the picture on screen again, which
// reads it back for the still: Flutter's populate, if the texture is still
// drawn, or the plane at once.
static void pb_video_texture_wake(PbVideoTexture* self) {
  if (!self->state->plane_mode) {
    fl_texture_registrar_mark_texture_frame_available(self->registrar, FL_TEXTURE(self));
  } else if (self->state->plane) {
    pb_plane_area_wake(self->state->plane);
  }
}

// The GTK thread, a second after the last still.
static gboolean pb_video_texture_still_due(gpointer data) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(data);
  g_mutex_lock(&self->mutex);
  self->state->still_timer = false;
  const bool stale = self->state->still_stale;
  g_mutex_unlock(&self->mutex);
  if (stale) pb_video_texture_wake(self);
  return G_SOURCE_REMOVE;
}

// The presenting thread, `gl`'s context current (Flutter's raster thread,
// or the GTK thread for the plane): converts the newest picture, if there
// is a new one, into `gl`'s RGBA texture. Only a converted picture earns
// its receipt; one that cannot be converted leaves the last one on screen
// and is reported, unless it is a dma-buf this context refused, after
// which the decoder sends system memory. True when `gl` holds another
// picture since.
static bool pb_video_texture_take(PbVideoTexture* self, pb_video::VideoGl* gl) {
  TextureState* state = self->state;
  PbVideoPicture* next = self->module->acquire(self->decoder);
  const bool timed = g_atomic_int_get(&self->timed) != 0;
  const Clock::time_point taken = timed ? Clock::now() : Clock::time_point{};
  g_mutex_lock(&self->mutex);
  const bool same = next && next == state->shown;
  bool dmabuf = state->shown_dmabuf, stale = state->still_stale;
  const bool wanted = state->still_wanted;
  g_mutex_unlock(&self->mutex);
  if (same) {
    self->module->release(next);
    next = nullptr;
  }
  bool converted = false;
  Clock::duration spent{};
  if (next) {
    const auto started = Clock::now();
    std::string failure;
    converted = gl->Convert(next, &failure);
    spent = Clock::now() - started;
    if (gl->TakeDmabufRefusal()) self->module->refuse_dmabuf(self->decoder);
    if (converted) {
      // Flutter, or the plane, draws it in this frame.
      self->module->present(self->decoder, next);
      dmabuf = gl->last_dmabuf();
      stale = dmabuf;
    } else if (!gl->last_refused()) {
      self->module->present_failed(self->decoder, failure.c_str());
      if (!state->failure_logged) {
        state->failure_logged = true;
        g_warning("Native desktop video: %s", failure.c_str());
      }
    }
  }
  // A dma-buf picture on screen that the still does not hold yet is read
  // back once a second has passed since the last read, or at once for a
  // waiting snapshot; else a timer brings this back then.
  if (!dmabuf) stale = false;
  const Clock::time_point now = Clock::now();
  const bool attempt = stale && (wanted || now - state->read_back >= kStillInterval);
  std::vector<uint8_t> still;
  uint32_t still_width = 0, still_height = 0;
  bool read = false;
  if (attempt) {
    state->read_back = now;
    read = gl->ReadBack(static_cast<uint32_t>(kSnapshotWidth), &still, &still_width, &still_height);
    // A still that cannot be read is not tried again for this picture.
    stale = false;
  }
  PbVideoPicture* previous = next;
  bool arm = false;
  g_mutex_lock(&self->mutex);
  if (converted) {
    previous = state->shown;
    state->shown = next;
    state->shown_dmabuf = dmabuf;
    if (state->timing) state->convert.Add(spent);
    // A newer picture shown: while timed, its wait since frame_ready. A
    // raster pass already under way can take a picture before its
    // frame_ready runs; that sample then counts from an earlier one, or is
    // skipped.
    if (timed && state->timing && state->waiting) {
      state->consume.Add(std::max(Clock::duration::zero(), taken - state->ready));
      state->waiting = false;
    }
  }
  if (read) {
    state->still.swap(still);
    state->still_width = still_width;
    state->still_height = still_height;
  }
  if (attempt || wanted) {
    ++state->still_reads;
    state->still_wanted = false;
    g_cond_broadcast(&self->still_read);
  }
  state->still_stale = stale;
  if (stale && !state->still_timer) state->still_timer = arm = true;
  g_mutex_unlock(&self->mutex);
  // The picture replaced, or the one that could not be shown: its planes
  // were uploaded, or the converter holds the dma-buf until the GPU has
  // read it.
  if (previous) self->module->release(previous);
  if (arm) {
    const auto due = std::chrono::duration_cast<std::chrono::milliseconds>(kStillInterval - (now - state->read_back));
    g_timeout_add_full(G_PRIORITY_DEFAULT, static_cast<guint>(std::max<int64_t>(due.count(), 0) + 1),
                       pb_video_texture_still_due, g_object_ref(self), g_object_unref);
  }
  return converted;
}

// Flutter's raster thread, its GL context current: converts the newest
// picture, if there is a new one, and hands Flutter the RGBA texture. A
// texture the plane shows takes nothing here: Flutter gets the black pixel.
static gboolean pb_video_texture_populate(FlTextureGL* texture, uint32_t* target, uint32_t* name, uint32_t* width,
                                          uint32_t* height, GError** error) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(texture);
  TextureState* state = self->state;
  if (!state->gl) state->gl.reset(new pb_video::VideoGl(self->module));
  if (!state->plane_mode) pb_video_texture_take(self, state->gl.get());
  std::string failure;
  if (!state->gl->Texture(name, width, height, &failure)) {
    g_set_error_literal(error, g_quark_from_static_string("pb-mapper-video"), 0, failure.c_str());
    return FALSE;
  }
  *target = GL_TEXTURE_2D;
  return TRUE;
}

// The last reference: the decoder thread stopped when the texture closed,
// and no populate, snapshot or still timer runs any more. The converter
// deletes its GL objects in Flutter's share group.
static void pb_video_texture_finalize(GObject* object) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(object);
  if (self->decoder) self->module->destroy(self->decoder);
  if (self->state) {
    self->state->gl.reset();
    if (self->state->shown) self->module->release(self->state->shown);
    if (self->state->plane) g_object_unref(self->state->plane);
    delete self->state;
  }
  g_clear_object(&self->registrar);
  g_cond_clear(&self->still_read);
  g_mutex_clear(&self->mutex);
  G_OBJECT_CLASS(pb_video_texture_parent_class)->finalize(object);
}

static void pb_video_texture_class_init(PbVideoTextureClass* klass) {
  FL_TEXTURE_GL_CLASS(klass)->populate = pb_video_texture_populate;
  G_OBJECT_CLASS(klass)->finalize = pb_video_texture_finalize;
}

static void pb_video_texture_init(PbVideoTexture* self) {
  g_mutex_init(&self->mutex);
  g_cond_init(&self->still_read);
  self->state = new TextureState();
}

// The GTK thread, at high priority: the plane draws the newer picture at
// its next frame.
static gboolean pb_video_texture_plane_ready(gpointer data) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(data);
  if (self->state->plane) pb_plane_area_ready(self->state->plane);
  return G_SOURCE_REMOVE;
}

// A GStreamer thread: a newer picture can be acquired. The module calls this
// just after publishing it, so its time is taken first; a raster pass
// already under way can still take the picture before it, and that sample
// then runs from an earlier frame_ready, or is skipped. For the plane it
// wakes the GTK thread once for each picture that waits, and never Flutter.
static void pb_video_texture_frame_ready(void* context) {
  PbVideoTexture* self = static_cast<PbVideoTexture*>(context);
  if (g_atomic_int_get(&self->timed)) {
    const Clock::time_point now = Clock::now();
    g_mutex_lock(&self->mutex);
    if (self->state->timing) {
      self->state->ready = now;
      self->state->waiting = true;
    }
    g_mutex_unlock(&self->mutex);
  }
  if (!self->state->plane_mode) {
    fl_texture_registrar_mark_texture_frame_available(self->registrar, FL_TEXTURE(self));
  } else if (!self->state->plane_pending.exchange(true)) {
    // The decoder thread runs only between attach and close, while the
    // channel holds the texture.
    g_main_context_invoke_full(nullptr, G_PRIORITY_HIGH, pb_video_texture_plane_ready, g_object_ref(self),
                               g_object_unref);
  }
}

// The plane's source (desktop_video_plane.h).
static bool pb_video_texture_plane_take(gpointer texture, pb_video::VideoGl* gl, bool wake) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(texture);
  const bool pending = self->state->plane_pending.exchange(false);
  return (pending || wake) && pb_video_texture_take(self, gl);
}

static bool pb_video_texture_plane_pending(gpointer texture) {
  return PB_VIDEO_TEXTURE(texture)->state->plane_pending.load();
}

// `plane`: the plane that shows its pictures, or null for Flutter's
// texture.
static PbVideoTexture* pb_video_texture_new(const PbVideoModule* module, const PbVideoExports* exports,
                                            const PbVideoPresenter& presenter, const std::string& device,
                                            FlTextureRegistrar* registrar, PbPlaneArea* plane) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(g_object_new(pb_video_texture_get_type(), nullptr));
  self->module = module;
  self->registrar = FL_TEXTURE_REGISTRAR(g_object_ref(registrar));
  if (plane) {
    self->state->plane_mode = true;
    self->state->plane = PB_PLANE_AREA(g_object_ref(plane));
  }
  self->decoder = module->create(exports, &presenter, device.c_str(), pb_video_texture_frame_ready, self);
  if (!self->decoder) {
    g_object_unref(self);
    return nullptr;
  }
  return self;
}

namespace {
// A snapshot of the picture on screen: in system memory the module scales
// and converts it; a dma-buf one is its still, read back again first when
// it is older (`refresh`, from `reads`).
struct SnapshotJob {
  PbVideoTexture* texture;
  bool refresh;
  uint64_t reads;
  std::string path;
  uint32_t max_width;
  Clock::time_point deadline;
  ~SnapshotJob() {
    g_atomic_int_set(&texture->snapshotting, 0);
    g_object_unref(texture);
  }
};

void SnapshotThread(GTask* task, gpointer, gpointer data, GCancellable*) {
  auto* job = static_cast<SnapshotJob*>(data);
  PbVideoTexture* texture = job->texture;
  const PbVideoModule* module = texture->module;
  PbVideoPicture* picture = nullptr;
  std::vector<uint8_t> rgba;
  uint32_t width = 0, height = 0;
  g_mutex_lock(&texture->mutex);
  TextureState* state = texture->state;
  if (job->refresh) {
    // Flutter's next frame, if the texture is still drawn: the old still
    // otherwise.
    const gint64 until = g_get_monotonic_time() + kStillWaitUs;
    while (state->still_wanted && state->still_reads == job->reads) {
      if (!g_cond_wait_until(&texture->still_read, &texture->mutex, until)) break;
    }
    state->still_wanted = false;
  }
  if (state->shown && !state->shown_dmabuf) {
    picture = module->retain(state->shown);
  } else if (state->shown) {
    rgba = state->still;
    width = state->still_width;
    height = state->still_height;
  }
  g_mutex_unlock(&texture->mutex);
  if (picture) {
    int32_t made = module->snapshot(picture, job->max_width, nullptr, 0, &width, &height);
    if (made == -2) {
      rgba.resize(static_cast<size_t>(width) * height * 4);
      made = module->snapshot(picture, job->max_width, rgba.data(), rgba.size(), &width, &height);
    }
    if (made != 0) rgba.clear();
    module->release(picture);
  }
  g_task_return_boolean(task, !rgba.empty() &&
                                  WriteSnapshot(rgba.data(), width, height, job->max_width, job->path, job->deadline));
}

void SnapshotDone(GObject*, GAsyncResult* result, gpointer data) {
  g_autoptr(FlMethodCall) call = FL_METHOD_CALL(data);
  g_autoptr(FlValue) written = fl_value_new_bool(g_task_propagate_boolean(G_TASK(result), nullptr));
  fl_method_call_respond_success(call, written, nullptr);
}

// Who waits for the running probe, and whether one runs. GTK thread only:
// the probe's completion runs there too.
struct ProbeState {
  std::vector<FlMethodCall*> waiting;
  bool probing = false;
};

struct ProbeJob {
  std::shared_ptr<ProbeState> state;
  double display_max_hz;
  std::string device;
};

void ProbeThread(GTask* task, gpointer, gpointer data, GCancellable*) {
  const auto* job = static_cast<const ProbeJob*>(data);
  g_task_return_pointer(task, ProbeDecoders(job->display_max_hz, job->device),
                        reinterpret_cast<GDestroyNotify>(fl_value_unref));
}

void ProbeDone(GObject*, GAsyncResult* result, gpointer) {
  auto* job = static_cast<ProbeJob*>(g_task_get_task_data(G_TASK(result)));
  g_autoptr(FlValue) answer = static_cast<FlValue*>(g_task_propagate_pointer(G_TASK(result), nullptr));
  std::vector<FlMethodCall*> waiting;
  waiting.swap(job->state->waiting);
  job->state->probing = false;
  for (FlMethodCall* call : waiting) {
    fl_method_call_respond_success(call, answer, nullptr);
    g_object_unref(call);
  }
}

// A `place` call's placement, in the Flutter view's physical pixels.
pb_video::Placement PlacementOf(FlValue* args) {
  FlValue* visible = fl_value_lookup_string(args, "visible");
  if (!visible || fl_value_get_type(visible) != FL_VALUE_TYPE_BOOL) throw std::runtime_error("Missing placement");
  pb_video::Placement placement;
  placement.visible = fl_value_get_bool(visible);
  if (!placement.visible) return placement;
  const auto coordinate = [args](const char* key, bool size) {
    const int64_t value = Number(args, key);
    if (value > kMaxPlaneCoordinate || value < (size ? 0 : -kMaxPlaneCoordinate)) {
      throw std::runtime_error("Invalid placement");
    }
    return static_cast<int32_t>(value);
  };
  placement.picture.x = coordinate("x", false);
  placement.picture.y = coordinate("y", false);
  placement.picture.width = coordinate("w", true);
  placement.picture.height = coordinate("h", true);
  placement.clip.x = coordinate("clipX", false);
  placement.clip.y = coordinate("clipY", false);
  placement.clip.width = coordinate("clipW", true);
  placement.clip.height = coordinate("clipH", true);
  return placement;
}

class DesktopVideo {
 public:
  // `plane`: the video plane under the view, or null.
  DesktopVideo(FlPluginRegistrar* registrar, FlView* view, GtkWidget* plane)
      : textures_(FL_TEXTURE_REGISTRAR(g_object_ref(fl_plugin_registrar_get_texture_registrar(registrar)))),
        plane_(plane ? PB_PLANE_AREA(g_object_ref(plane)) : nullptr),
        probe_(std::make_shared<ProbeState>()) {
    g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
    channel_ = fl_method_channel_new(fl_plugin_registrar_get_messenger(registrar), kChannel, FL_METHOD_CODEC(codec));
    fl_method_channel_set_method_call_handler(channel_, OnCall, this, nullptr);
    WatchDisplays(view);
  }
  DesktopVideo(const DesktopVideo&) = delete;
  DesktopVideo& operator=(const DesktopVideo&) = delete;
  ~DesktopVideo() {
    fl_method_channel_set_method_call_handler(channel_, nullptr, nullptr, nullptr);
    for (auto& entry : viewers_) Close(entry.second);
    viewers_.clear();
    g_object_unref(channel_);
    g_object_unref(textures_);
    if (plane_) g_object_unref(plane_);
  }

 private:
  static void OnCall(FlMethodChannel*, FlMethodCall* call, gpointer data) {
    try {
      static_cast<DesktopVideo*>(data)->Handle(call);
    } catch (const std::exception& e) {
      fl_method_call_respond_error(call, "desktop_video", e.what(), nullptr, nullptr);
    }
  }

  void Handle(FlMethodCall* call) {
    const gchar* method = fl_method_call_get_name(call);
    if (strcmp(method, "decoders") == 0) {
      Decoders(call);
      return;
    }
    if (strcmp(method, "refresh_hz") == 0) {
      // The refresh of the display the view's window is on, which Dart
      // sends the viewer; null when GDK says nothing. No arguments. Dart
      // follows the display from the first call, so `display_changed` goes
      // from then on, for a monitor other than this one.
      following_ = true;
      monitor_ = WindowMonitor(view_);
      const double hz = WindowRefreshHz(view_);
      g_autoptr(FlValue) result = hz > 1 ? fl_value_new_float(hz) : fl_value_new_null();
      fl_method_call_respond_success(call, result, nullptr);
      return;
    }
    FlValue* args = fl_method_call_get_args(call);
    if (!args || fl_value_get_type(args) != FL_VALUE_TYPE_MAP) {
      throw std::runtime_error("Missing desktop video arguments");
    }
    if (strcmp(method, "create") == 0) {
      CheckInstance(static_cast<uint64_t>(Number(args, "instance")));
      if (viewers_.size() >= kMaxTextures) throw std::runtime_error("Desktop texture limit reached");
      const PbVideoModule* module = SoftwareRenderer() ? nullptr : LoadModule();
      if (!module) throw std::runtime_error("Native video decoding is unavailable");
      // The plane for the first texture that asks for it, if it can draw;
      // every other texture, and every refusal, is Flutter's.
      FlValue* asked = fl_value_lookup_string(args, "presenter");
      const bool wants_plane = asked && fl_value_get_type(asked) == FL_VALUE_TYPE_STRING &&
                               strcmp(fl_value_get_string(asked), "plane") == 0;
      PbPlaneArea* plane =
          wants_plane && plane_ && !pb_plane_area_bound(plane_) && pb_plane_area_usable(plane_) ? plane_ : nullptr;
      // The plane imports dma-bufs only through EGL (Wayland): a GLX plane
      // takes every picture in system memory.
      const PbVideoPresenter presenter = plane && !pb_plane_area_egl(plane) ? PbVideoPresenter{} : ReadPresenter();
      PbVideoTexture* texture =
          pb_video_texture_new(module, &library_->exports, presenter, PresentingDevice(), textures_, plane);
      if (!texture) throw std::runtime_error("Cannot create a video decoder");
      if (!fl_texture_registrar_register_texture(textures_, FL_TEXTURE(texture))) {
        g_object_unref(texture);
        throw std::runtime_error("Cannot register video texture");
      }
      const int64_t id = fl_texture_get_id(FL_TEXTURE(texture));
      viewers_[id] = texture;
      if (plane) {
        pb_video::PlaneSource source;
        source.texture = texture;
        source.module = module;
        source.take = pb_video_texture_plane_take;
        source.pending = pb_video_texture_plane_pending;
        pb_plane_area_bind(plane, source);
      }
      g_autoptr(FlValue) result = fl_value_new_map();
      fl_value_set_string_take(result, "textureId", fl_value_new_int(id));
      fl_value_set_string_take(result, "plane", fl_value_new_bool(plane != nullptr));
      fl_method_call_respond_success(call, result, nullptr);
      return;
    }
    const auto found = viewers_.find(Number(args, "textureId"));
    if (found == viewers_.end()) throw std::runtime_error("Unknown desktop texture");
    PbVideoTexture* texture = found->second;
    if (strcmp(method, "attach") == 0) {
      // The texture and its last picture stay through a reconnect; the new
      // session's first IDR replaces them.
      const int64_t consumer = texture->module->attach(texture->decoder, static_cast<uint64_t>(Number(args, "source")));
      if (consumer <= 0) throw std::runtime_error("Video source unavailable (" + std::to_string(consumer) + ")");
      fl_method_call_respond_success(call, nullptr, nullptr);
    } else if (strcmp(method, "snapshot") == 0) {
      const int64_t width = fl_value_lookup_string(args, "maxWidth") ? Number(args, "maxWidth") : kSnapshotWidth;
      if (width <= 0) throw std::runtime_error("Invalid snapshot width");
      FlValue* path = fl_value_lookup_string(args, "path");
      if (!path || fl_value_get_type(path) != FL_VALUE_TYPE_STRING || !*fl_value_get_string(path)) {
        throw std::runtime_error("Missing snapshot path");
      }
      Snapshot(texture, fl_value_get_string(path), static_cast<uint32_t>(std::min<int64_t>(width, UINT32_MAX)), call);
    } else if (strcmp(method, "place") == 0) {
      // Where the viewer shows the plane's picture; nothing for Flutter's
      // texture, which Flutter places.
      const pb_video::Placement placement = PlacementOf(args);
      if (texture->state->plane) pb_plane_area_place(texture->state->plane, placement);
      fl_method_call_respond_success(call, nullptr, nullptr);
    } else if (strcmp(method, "grey") == 0) {
      // The reconnect's grey still on the plane, which no Flutter filter
      // reaches; Flutter's texture takes the viewer's own.
      FlValue* on = fl_value_lookup_string(args, "on");
      if (!on || fl_value_get_type(on) != FL_VALUE_TYPE_BOOL) throw std::runtime_error("Missing grey");
      if (texture->state->plane) pb_plane_area_grey(texture->state->plane, fl_value_get_bool(on));
      fl_method_call_respond_success(call, nullptr, nullptr);
    } else if (strcmp(method, "stats") == 0) {
      // `timings` is optional and true when absent, as on Windows and
      // macOS.
      FlValue* wanted = fl_value_lookup_string(args, "timings");
      const bool timings =
          !wanted || fl_value_get_type(wanted) != FL_VALUE_TYPE_BOOL || fl_value_get_bool(wanted);
      PbVideoStats stats{};
      texture->module->stats(texture->decoder, timings ? 1 : 0, &stats);
      PbVideoTiming convert{}, consume{};
      g_mutex_lock(&texture->mutex);
      TextureState* state = texture->state;
      if (timings != state->timing) {
        state->convert = Timing{};
        state->consume = Timing{};
        state->waiting = false;
      }
      state->timing = timings;
      g_atomic_int_set(&texture->timed, timings ? 1 : 0);
      if (timings) {
        convert = state->convert.Snapshot();
        consume = state->consume.Snapshot();
      }
      g_mutex_unlock(&texture->mutex);
      g_autoptr(FlValue) result = fl_value_new_map();
      fl_value_set_string_take(result, "submitted", fl_value_new_int(static_cast<int64_t>(stats.submitted)));
      fl_value_set_string_take(result, "decoded", fl_value_new_int(static_cast<int64_t>(stats.decoded)));
      fl_value_set_string_take(result, "presented", fl_value_new_int(static_cast<int64_t>(stats.presented)));
      fl_value_set_string_take(result, "dropped", fl_value_new_int(static_cast<int64_t>(stats.dropped)));
      fl_value_set_string_take(result, "configurations", fl_value_new_int(static_cast<int64_t>(stats.configurations)));
      fl_value_set_string_take(result, "recoveries", fl_value_new_int(static_cast<int64_t>(stats.recoveries)));
      fl_value_set_string_take(result, "lastRecovery", fl_value_new_string(stats.last_recovery));
      fl_value_set_string_take(result, "decoder", fl_value_new_string(stats.decoder));
      // Whether pictures come as dma-bufs, and whether videoconvert makes
      // them NV12.
      fl_value_set_string_take(result, "dmabuf", fl_value_new_bool((stats.flags & PB_VIDEO_STATS_DMABUF) != 0));
      fl_value_set_string_take(result, "videoconvert",
                               fl_value_new_bool((stats.flags & PB_VIDEO_STATS_CONVERTS) != 0));
      if (timings) {
        FlValue* measured = fl_value_new_map();
        fl_value_set_string_take(measured, "decode", TimingValue(stats.decode));
        fl_value_set_string_take(measured, "convert", TimingValue(convert));
        fl_value_set_string_take(measured, "surface_consume", TimingValue(consume));
        fl_value_set_string_take(result, "timings", measured);
      }
      // The display the view's window is on, the one the viewer follows;
      // the fastest where GDK says nothing of it.
      const double window_hz = WindowRefreshHz(view_);
      fl_value_set_string_take(result, "refreshHz", fl_value_new_float(window_hz > 1 ? window_hz : DisplayMaxHz()));
      if (texture->state->plane) {
        // The plane's own presentation: the refresh interval GDK's frames
        // carry, left out when none does, and, while timed, the intervals
        // between the frames that drew a newer picture.
        const pb_video::PlaneStats plane = pb_plane_area_stats(texture->state->plane, timings);
        if (plane.refresh_interval_ms > 0) {
          fl_value_set_string_take(result, "refreshIntervalMs", fl_value_new_float(plane.refresh_interval_ms));
        }
        if (timings && plane.intervals > 0) {
          fl_value_set_string_take(result, "planeIntervalP50Ms", fl_value_new_float(plane.interval_p50_ms));
          fl_value_set_string_take(result, "planeIntervalP95Ms", fl_value_new_float(plane.interval_p95_ms));
        }
      }
      fl_method_call_respond_success(call, result, nullptr);
    } else if (strcmp(method, "dispose") == 0) {
      viewers_.erase(found);
      Close(texture);
      fl_method_call_respond_success(call, nullptr, nullptr);
    } else {
      fl_method_call_respond_not_implemented(call, nullptr);
    }
  }

  // `decoders` takes no arguments and probes off the GTK thread, one probe
  // at a time: calls meanwhile share its answer. Dart keeps it.
  void Decoders(FlMethodCall* call) {
    probe_->waiting.push_back(FL_METHOD_CALL(g_object_ref(call)));
    if (probe_->probing) return;
    probe_->probing = true;
    GTask* task = g_task_new(nullptr, nullptr, ProbeDone, nullptr);
    g_task_set_task_data(task, new ProbeJob{probe_, DisplayMaxHz(), PresentingDevice()},
                         [](gpointer job) { delete static_cast<ProbeJob*>(job); });
    g_task_run_in_thread(task, ProbeThread);
    g_object_unref(task);
  }

  // What may change the display the view's window is on, which Flutter's
  // embedder does not follow (it names the view's display again only when
  // its size or scale changes, and reads the monitors' rates only when one
  // is added or removed): the window's configure-event, which X11 sends on
  // a move (Wayland tells GTK 3 nothing of a move between outputs of one
  // scale), a monitor added or removed, and a monitor's refresh rate. Each
  // handler finds this through `view`, and is disconnected when the view
  // goes, before this does.
  void WatchDisplays(FlView* view) {
    view_ = GTK_WIDGET(view);
    GtkWidget* toplevel = gtk_widget_get_toplevel(view_);
    if (gtk_widget_is_toplevel(toplevel)) {
      g_signal_connect_object(toplevel, "configure-event", G_CALLBACK(OnConfigure), view_,
                              static_cast<GConnectFlags>(0));
    }
    GdkDisplay* display = gtk_widget_get_display(view_);
    g_signal_connect_object(display, "monitor-added", G_CALLBACK(OnMonitorAdded), view_,
                            static_cast<GConnectFlags>(0));
    g_signal_connect_object(display, "monitor-removed", G_CALLBACK(OnMonitorRemoved), view_,
                            static_cast<GConnectFlags>(0));
    for (int i = 0; i < gdk_display_get_n_monitors(display); ++i) WatchMonitor(gdk_display_get_monitor(display, i));
  }

  void WatchMonitor(GdkMonitor* monitor) {
    g_signal_connect_object(monitor, "notify::refresh-rate", G_CALLBACK(OnRefreshRate), view_,
                            static_cast<GConnectFlags>(0));
  }

  static DesktopVideo* Of(gpointer view) {
    return static_cast<DesktopVideo*>(g_object_get_data(G_OBJECT(view), kViewData));
  }

  // Seen, not handled: the window goes on with it.
  static gboolean OnConfigure(GtkWidget*, GdkEvent*, gpointer view) {
    if (DesktopVideo* video = Of(view)) video->DisplayMaybeChanged(true);
    return FALSE;
  }

  static void OnMonitorAdded(GdkDisplay*, GdkMonitor* monitor, gpointer view) {
    if (DesktopVideo* video = Of(view)) {
      video->WatchMonitor(monitor);
      video->DisplayMaybeChanged(false);
    }
  }

  static void OnMonitorRemoved(GdkDisplay*, GdkMonitor*, gpointer view) {
    if (DesktopVideo* video = Of(view)) video->DisplayMaybeChanged(false);
  }

  static void OnRefreshRate(GObject*, GParamSpec*, gpointer view) {
    if (DesktopVideo* video = Of(view)) video->DisplayMaybeChanged(false);
  }

  // The window moved or resized, or a monitor came, went or changed its
  // rate: once Dart follows the display, when the window is on another
  // monitor than before, or a monitor changed, Dart is told
  // (`display_changed`, no arguments) and reads `refresh_hz` again. A move
  // within one monitor, as each step of a drag, tells nothing.
  void DisplayMaybeChanged(bool moved) {
    if (!following_) return;
    GdkMonitor* now = WindowMonitor(view_);
    if (moved && now == monitor_) return;
    monitor_ = now;
    fl_method_channel_invoke_method(channel_, "display_changed", nullptr, nullptr, nullptr, nullptr);
  }

  // D9: Dart and this runner must reach one library instance, or the
  // decoder would pull from a registry Dart never filled. Fails closed.
  void CheckInstance(uint64_t dart) {
    if (!library_) library_.reset(new VideoLibrary(LoadVideoLibrary()));
    const uint64_t native = library_->instance_token();
    g_debug("Video library instance: Dart 0x%" G_GINT64_MODIFIER "x, runner 0x%" G_GINT64_MODIFIER "x",
            static_cast<guint64>(dart), static_cast<guint64>(native));
    if (native != dart) throw std::runtime_error("native video library loaded twice");
  }

  // The picture shown last, written by a worker thread; the reply comes
  // back on this one. A dma-buf picture's still older than the picture is
  // read back first, at the raster thread's next populate.
  void Snapshot(PbVideoTexture* texture, const char* path, uint32_t max_width, FlMethodCall* call) {
    bool shown = false, refresh = false;
    uint64_t reads = 0;
    if (g_atomic_int_compare_and_exchange(&texture->snapshotting, 0, 1)) {
      g_mutex_lock(&texture->mutex);
      TextureState* state = texture->state;
      shown = state->shown != nullptr;
      if (shown && state->shown_dmabuf && state->still_stale) {
        state->still_wanted = refresh = true;
        reads = state->still_reads;
      }
      g_mutex_unlock(&texture->mutex);
      if (!shown) g_atomic_int_set(&texture->snapshotting, 0);
    }
    if (!shown) {
      g_autoptr(FlValue) result = fl_value_new_bool(FALSE);
      fl_method_call_respond_success(call, result, nullptr);
      return;
    }
    if (refresh) pb_video_texture_wake(texture);
    auto* job = new SnapshotJob{PB_VIDEO_TEXTURE(g_object_ref(texture)),
                                refresh,
                                reads,
                                path,
                                max_width,
                                Clock::now() + kSnapshotDeadline};
    GTask* task = g_task_new(nullptr, nullptr, SnapshotDone, g_object_ref(call));
    g_task_set_task_data(task, job, [](gpointer data) { delete static_cast<SnapshotJob*>(data); });
    g_task_run_in_thread(task, SnapshotThread);
    g_object_unref(task);
  }

  // Detaching stops the decoder thread at once; the plane lets the texture
  // go, hidden, and the texture goes after Flutter's last possible
  // populate.
  void Close(PbVideoTexture* texture) {
    texture->module->detach(texture->decoder);
    TextureState* state = texture->state;
    if (state->plane) {
      pb_plane_area_unbind(state->plane);
      g_object_unref(state->plane);
      state->plane = nullptr;
    }
    fl_texture_registrar_unregister_texture(textures_, FL_TEXTURE(texture));
    g_timeout_add(kTextureReleaseMs, [](gpointer data) -> gboolean {
      g_object_unref(data);
      return G_SOURCE_REMOVE;
    }, texture);
  }

  FlTextureRegistrar* textures_;
  // The video plane, for the first texture that asks for it; null where
  // the runner has none.
  PbPlaneArea* plane_ = nullptr;
  FlMethodChannel* channel_ = nullptr;
  std::shared_ptr<ProbeState> probe_;
  // Resolved by the first create: Dart has loaded the library by then.
  std::unique_ptr<const VideoLibrary> library_;
  // This runner's reference to each live texture.
  std::map<int64_t, PbVideoTexture*> viewers_;
  // Flutter's view, which holds this (no reference of its own).
  GtkWidget* view_ = nullptr;
  // The monitor the view's window was on when last looked at, once Dart
  // follows; compared, never read: it may be gone.
  GdkMonitor* monitor_ = nullptr;
  // Dart asked `refresh_hz`, so it listens for `display_changed`.
  bool following_ = false;
};
}  // namespace

void desktop_video_register(FlView* view, GtkWidget* plane) {
  g_autoptr(FlPluginRegistrar) registrar =
      fl_plugin_registry_get_registrar_for_plugin(FL_PLUGIN_REGISTRY(view), "PbMapperDesktopVideo");
  g_object_set_data_full(G_OBJECT(view), kViewData, new DesktopVideo(registrar, view, plane),
                         [](gpointer data) { delete static_cast<DesktopVideo*>(data); });
}
