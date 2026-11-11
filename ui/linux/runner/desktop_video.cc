// The `pb_mapper/desktop_video` channel of the Linux runner: native H.264
// and H.265 into Flutter textures, as the Windows runner
// (ui/windows/runner/desktop_video.cpp) and the Android one (DesktopVideo.kt)
// do it. Decoding is GStreamer's, in libpb_mapper_desktop_video.so
// (desktop_video_gst.cc), which this runner loads with dlopen at the first
// probe: without it, or without GStreamer, `decoders` answers none and
// every session is JPEG, which Dart decodes. The app never needs GStreamer
// to start.
#define G_LOG_DOMAIN "pb-mapper-video"

#include "desktop_video.h"

#include <dlfcn.h>
#include <fcntl.h>
#include <link.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

#include "desktop_video_module.h"

namespace {
using Clock = std::chrono::steady_clock;

constexpr char kChannel[] = "pb_mapper/desktop_video";
constexpr char kFfiFile[] = "libpb_mapper_ffi.so";
constexpr int64_t kSnapshotWidth = 320;
constexpr char kSnapshotQuality[] = "80";
constexpr auto kSnapshotDeadline = std::chrono::milliseconds(300);
constexpr size_t kMaxTextures = 2;
// Flutter's raster thread looks a texture up and then populates it without
// holding a reference; a populate already running when the texture is
// unregistered still reads it. The last reference goes this much later.
constexpr guint kTextureReleaseMs = 1000;
// Shown until the first picture: a texture always hands Flutter pixels.
const uint8_t kBlack[4] = {0, 0, 0, 255};

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
// hang there. Off the GTK thread.
FlValue* ProbeDecoders(double display_max_hz) {
  PbVideoProbe probe{};
  const PbVideoModule* module = SoftwareRenderer() ? nullptr : LoadModule();
  if (module) {
    char error[256] = "";
    if (module->probe(&probe, error, sizeof(error)) != 0) {
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

int64_t Number(FlValue* map, const char* key) {
  FlValue* value = fl_value_lookup_string(map, key);
  if (!value) throw std::runtime_error("Missing video field");
  if (fl_value_get_type(value) != FL_VALUE_TYPE_INT) throw std::runtime_error("Invalid video field");
  return fl_value_get_int(value);
}
}  // namespace

// One texture and its decoder. Flutter's raster thread calls copy_pixels,
// whose acquisition of a new picture is its presentation receipt.
G_DECLARE_FINAL_TYPE(PbVideoTexture, pb_video_texture, PB, VIDEO_TEXTURE, FlPixelBufferTexture)

struct _PbVideoTexture {
  FlPixelBufferTexture parent_instance;
  const PbVideoModule* module;
  PbVideoDecoder* decoder;
  FlTextureRegistrar* registrar;
  // Whether a snapshot is being written: one at a time.
  gint snapshotting;
  GMutex mutex;
  // Guarded by mutex: the picture handed to Flutter last, which is the one
  // on screen. Only the raster thread replaces it.
  PbVideoPicture* shown;
};

G_DEFINE_TYPE(PbVideoTexture, pb_video_texture, fl_pixel_buffer_texture_get_type())

static gboolean pb_video_texture_copy_pixels(FlPixelBufferTexture* texture, const uint8_t** buffer, uint32_t* width,
                                             uint32_t* height, GError** error) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(texture);
  PbVideoPicture* next = self->module->acquire(self->decoder);
  PbVideoPicture* previous = nullptr;
  g_mutex_lock(&self->mutex);
  if (next) {
    previous = self->shown;
    self->shown = next;
  }
  PbVideoPicture* picture = self->shown;
  g_mutex_unlock(&self->mutex);
  // Flutter uploaded the previous picture when it last called.
  if (previous) self->module->release(previous);
  if (!picture) {
    *buffer = kBlack;
    *width = 1;
    *height = 1;
    return TRUE;
  }
  *buffer = self->module->pixels(picture, width, height);
  return TRUE;
}

// The last reference: the decoder thread stopped when the texture closed.
static void pb_video_texture_finalize(GObject* object) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(object);
  if (self->decoder) self->module->destroy(self->decoder);
  if (self->shown) self->module->release(self->shown);
  g_clear_object(&self->registrar);
  g_mutex_clear(&self->mutex);
  G_OBJECT_CLASS(pb_video_texture_parent_class)->finalize(object);
}

static void pb_video_texture_class_init(PbVideoTextureClass* klass) {
  FL_PIXEL_BUFFER_TEXTURE_CLASS(klass)->copy_pixels = pb_video_texture_copy_pixels;
  G_OBJECT_CLASS(klass)->finalize = pb_video_texture_finalize;
}

static void pb_video_texture_init(PbVideoTexture* self) { g_mutex_init(&self->mutex); }

// A GStreamer thread: a newer picture can be acquired.
static void pb_video_texture_frame_ready(void* context) {
  PbVideoTexture* self = static_cast<PbVideoTexture*>(context);
  fl_texture_registrar_mark_texture_frame_available(self->registrar, FL_TEXTURE(self));
}

static PbVideoTexture* pb_video_texture_new(const PbVideoModule* module, const PbVideoExports* exports,
                                            FlTextureRegistrar* registrar) {
  PbVideoTexture* self = PB_VIDEO_TEXTURE(g_object_new(pb_video_texture_get_type(), nullptr));
  self->module = module;
  self->registrar = FL_TEXTURE_REGISTRAR(g_object_ref(registrar));
  self->decoder = module->create(exports, pb_video_texture_frame_ready, self);
  if (!self->decoder) {
    g_object_unref(self);
    return nullptr;
  }
  return self;
}

namespace {
struct SnapshotJob {
  PbVideoTexture* texture;
  PbVideoPicture* picture;
  std::string path;
  uint32_t max_width;
  Clock::time_point deadline;
  ~SnapshotJob() {
    texture->module->release(picture);
    g_atomic_int_set(&texture->snapshotting, 0);
    g_object_unref(texture);
  }
};

void SnapshotThread(GTask* task, gpointer, gpointer data, GCancellable*) {
  const auto* job = static_cast<const SnapshotJob*>(data);
  uint32_t width = 0, height = 0;
  const uint8_t* rgba = job->texture->module->pixels(job->picture, &width, &height);
  g_task_return_boolean(task, WriteSnapshot(rgba, width, height, job->max_width, job->path, job->deadline));
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
};

void ProbeThread(GTask* task, gpointer, gpointer data, GCancellable*) {
  const auto* job = static_cast<const ProbeJob*>(data);
  g_task_return_pointer(task, ProbeDecoders(job->display_max_hz), reinterpret_cast<GDestroyNotify>(fl_value_unref));
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

class DesktopVideo {
 public:
  explicit DesktopVideo(FlPluginRegistrar* registrar)
      : textures_(FL_TEXTURE_REGISTRAR(g_object_ref(fl_plugin_registrar_get_texture_registrar(registrar)))),
        probe_(std::make_shared<ProbeState>()) {
    g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
    channel_ = fl_method_channel_new(fl_plugin_registrar_get_messenger(registrar), kChannel, FL_METHOD_CODEC(codec));
    fl_method_channel_set_method_call_handler(channel_, OnCall, this, nullptr);
  }
  DesktopVideo(const DesktopVideo&) = delete;
  DesktopVideo& operator=(const DesktopVideo&) = delete;
  ~DesktopVideo() {
    fl_method_channel_set_method_call_handler(channel_, nullptr, nullptr, nullptr);
    for (auto& entry : viewers_) Close(entry.second);
    viewers_.clear();
    g_object_unref(channel_);
    g_object_unref(textures_);
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
    FlValue* args = fl_method_call_get_args(call);
    if (!args || fl_value_get_type(args) != FL_VALUE_TYPE_MAP) {
      throw std::runtime_error("Missing desktop video arguments");
    }
    if (strcmp(method, "create") == 0) {
      CheckInstance(static_cast<uint64_t>(Number(args, "instance")));
      if (viewers_.size() >= kMaxTextures) throw std::runtime_error("Desktop texture limit reached");
      const PbVideoModule* module = LoadModule();
      if (!module) throw std::runtime_error("Native video decoding is unavailable");
      PbVideoTexture* texture = pb_video_texture_new(module, &library_->exports, textures_);
      if (!texture) throw std::runtime_error("Cannot create a video decoder");
      if (!fl_texture_registrar_register_texture(textures_, FL_TEXTURE(texture))) {
        g_object_unref(texture);
        throw std::runtime_error("Cannot register video texture");
      }
      const int64_t id = fl_texture_get_id(FL_TEXTURE(texture));
      viewers_[id] = texture;
      g_autoptr(FlValue) result = fl_value_new_int(id);
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
    } else if (strcmp(method, "stats") == 0) {
      PbVideoStats stats{};
      texture->module->stats(texture->decoder, &stats);
      g_autoptr(FlValue) result = fl_value_new_map();
      fl_value_set_string_take(result, "submitted", fl_value_new_int(static_cast<int64_t>(stats.submitted)));
      fl_value_set_string_take(result, "decoded", fl_value_new_int(static_cast<int64_t>(stats.decoded)));
      fl_value_set_string_take(result, "presented", fl_value_new_int(static_cast<int64_t>(stats.presented)));
      fl_value_set_string_take(result, "dropped", fl_value_new_int(static_cast<int64_t>(stats.dropped)));
      fl_value_set_string_take(result, "configurations", fl_value_new_int(static_cast<int64_t>(stats.configurations)));
      fl_value_set_string_take(result, "recoveries", fl_value_new_int(static_cast<int64_t>(stats.recoveries)));
      fl_value_set_string_take(result, "lastRecovery", fl_value_new_string(stats.last_recovery));
      fl_value_set_string_take(result, "decoder", fl_value_new_string(stats.decoder));
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
    g_task_set_task_data(task, new ProbeJob{probe_, DisplayMaxHz()},
                         [](gpointer job) { delete static_cast<ProbeJob*>(job); });
    g_task_run_in_thread(task, ProbeThread);
    g_object_unref(task);
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

  // The picture Flutter took last, written by a worker thread; the reply
  // comes back on this one.
  void Snapshot(PbVideoTexture* texture, const char* path, uint32_t max_width, FlMethodCall* call) {
    PbVideoPicture* picture = nullptr;
    if (g_atomic_int_compare_and_exchange(&texture->snapshotting, 0, 1)) {
      g_mutex_lock(&texture->mutex);
      picture = texture->shown ? texture->module->retain(texture->shown) : nullptr;
      g_mutex_unlock(&texture->mutex);
      if (!picture) g_atomic_int_set(&texture->snapshotting, 0);
    }
    if (!picture) {
      g_autoptr(FlValue) result = fl_value_new_bool(FALSE);
      fl_method_call_respond_success(call, result, nullptr);
      return;
    }
    auto* job = new SnapshotJob{PB_VIDEO_TEXTURE(g_object_ref(texture)), picture, path, max_width,
                                Clock::now() + kSnapshotDeadline};
    GTask* task = g_task_new(nullptr, nullptr, SnapshotDone, g_object_ref(call));
    g_task_set_task_data(task, job, [](gpointer data) { delete static_cast<SnapshotJob*>(data); });
    g_task_run_in_thread(task, SnapshotThread);
    g_object_unref(task);
  }

  // Detaching stops the decoder thread at once; the texture goes after
  // Flutter's last possible populate.
  void Close(PbVideoTexture* texture) {
    texture->module->detach(texture->decoder);
    fl_texture_registrar_unregister_texture(textures_, FL_TEXTURE(texture));
    g_timeout_add(kTextureReleaseMs, [](gpointer data) -> gboolean {
      g_object_unref(data);
      return G_SOURCE_REMOVE;
    }, texture);
  }

  FlTextureRegistrar* textures_;
  FlMethodChannel* channel_ = nullptr;
  std::shared_ptr<ProbeState> probe_;
  // Resolved by the first create: Dart has loaded the library by then.
  std::unique_ptr<const VideoLibrary> library_;
  // This runner's reference to each live texture.
  std::map<int64_t, PbVideoTexture*> viewers_;
};
}  // namespace

void desktop_video_register(FlView* view) {
  g_autoptr(FlPluginRegistrar) registrar =
      fl_plugin_registry_get_registrar_for_plugin(FL_PLUGIN_REGISTRY(view), "PbMapperDesktopVideo");
  g_object_set_data_full(G_OBJECT(view), "pb-mapper-desktop-video", new DesktopVideo(registrar),
                         [](gpointer data) { delete static_cast<DesktopVideo*>(data); });
}
