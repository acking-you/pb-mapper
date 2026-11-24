// The `pb_mapper/desktop_pointer` channel of the Linux runner, exactly as
// ui/lib/src/ffi/desktop_pointer.dart calls it, like the Windows runner
// (ui/windows/runner/desktop_pointer.cpp) and the macOS one
// (ui/darwin/DesktopPointer.swift). It makes a remote computer's pointer
// bitmap this computer's own cursor, which then moves with the hardware and
// changes the moment Dart sets it, with no Flutter frame.
//
// `cursor_create {id, rgba, width, height, hot_x, hot_y, scale}` makes a
// GdkCursor from straight RGBA in physical pixels, made for `scale` of them a
// logical pixel, replacing any cursor held as `id`; `cursor_set {id}` sets it
// on the window the engine sets its own cursors on (the toplevel's,
// fl_view.cc), so it stays until Dart activates a system cursor, and
// `cursor_delete {id}` frees it.
//
// `lock` holds the pointer over the remote picture for a game or a 3D view,
// and posts the mouse's raw motion to Dart once a turn of the main loop as
// `lock_motion {dx, dy}`:
// - X11: a pointer grab on the toplevel with a blank cursor, XInput2's raw
//   motion selected on the root window (the device's unaccelerated counts,
//   read in a GDK event filter), and the pointer warped back to where it was
//   held after each turn's motion, so it never leaves the picture;
// - Wayland: zwp_locked_pointer_v1 on the toplevel's surface, which keeps the
//   pointer where it is, and zwp_relative_pointer_v1's unaccelerated motion,
//   from client code wayland-scanner generates (runner/CMakeLists.txt).
// `unlock` lets go; so does the toplevel losing the focus, or the compositor
// ending the lock, which it tells Dart as `lock_lost`, and the view going
// away. A build without XInput2 or the Wayland protocols, or a display
// without them, answers `lock` with an error, and Dart leaves the pointer
// free.
#define G_LOG_DOMAIN "pb-mapper-pointer"

#include "desktop_pointer.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <map>
#include <stdexcept>
#include <string>

// A GTK built without a backend holds the pointer on no display of it.
#if defined(PB_MAPPER_LOCK_X11) && !defined(GDK_WINDOWING_X11)
#undef PB_MAPPER_LOCK_X11
#endif
#if defined(PB_MAPPER_LOCK_WAYLAND) && !defined(GDK_WINDOWING_WAYLAND)
#undef PB_MAPPER_LOCK_WAYLAND
#endif

#ifdef PB_MAPPER_LOCK_X11
#include <X11/extensions/XInput2.h>
#include <gdk/gdkx.h>
#endif
#ifdef PB_MAPPER_LOCK_WAYLAND
#include <gdk/gdkwayland.h>

#include "pointer-constraints-unstable-v1-client-protocol.h"
#include "relative-pointer-unstable-v1-client-protocol.h"
#endif

namespace {
constexpr char kChannel[] = "pb_mapper/desktop_pointer";
// No side Dart sends is longer: 128 logical pixels at the display's scale.
constexpr int64_t kMaxSide = 1024;
// No display's scale is larger.
constexpr double kMaxScale = 8;

int64_t Number(FlValue* map, const char* key) {
  FlValue* value = fl_value_lookup_string(map, key);
  if (!value || fl_value_get_type(value) != FL_VALUE_TYPE_INT) {
    throw std::runtime_error(std::string("Missing desktop pointer field ") + key);
  }
  return fl_value_get_int(value);
}

// The device pixel ratio Dart made the bitmap for, as GDK takes a cursor
// surface's scale: a whole number (the Wayland backend truncates it, and
// GTK 3's own scale factors are whole).
int Scale(FlValue* map) {
  FlValue* value = fl_value_lookup_string(map, "scale");
  double scale = 0;
  if (value && fl_value_get_type(value) == FL_VALUE_TYPE_FLOAT) {
    scale = fl_value_get_float(value);
  } else if (value && fl_value_get_type(value) == FL_VALUE_TYPE_INT) {
    scale = static_cast<double>(fl_value_get_int(value));
  }
  if (!std::isfinite(scale) || scale <= 0 || scale > kMaxScale) {
    throw std::runtime_error("Invalid cursor scale");
  }
  return std::max(1, static_cast<int>(std::lround(scale)));
}

class DesktopPointer {
 public:
  DesktopPointer(FlView* view, FlPluginRegistrar* registrar) : view_(view) {
    g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
    channel_ = fl_method_channel_new(fl_plugin_registrar_get_messenger(registrar), kChannel, FL_METHOD_CODEC(codec));
    fl_method_channel_set_method_call_handler(channel_, OnCall, this, nullptr);
  }
  DesktopPointer(const DesktopPointer&) = delete;
  DesktopPointer& operator=(const DesktopPointer&) = delete;
  ~DesktopPointer() {
    fl_method_channel_set_method_call_handler(channel_, nullptr, nullptr, nullptr);
    Unlock();
    if (post_source_ != 0) g_source_remove(post_source_);
#ifdef PB_MAPPER_LOCK_WAYLAND
    if (relative_manager_) zwp_relative_pointer_manager_v1_destroy(relative_manager_);
    if (constraints_) zwp_pointer_constraints_v1_destroy(constraints_);
#endif
    for (auto& entry : cursors_) g_object_unref(entry.second);
    g_object_unref(channel_);
  }

 private:
  static void OnCall(FlMethodChannel*, FlMethodCall* call, gpointer data) {
    try {
      static_cast<DesktopPointer*>(data)->Handle(call);
    } catch (const std::exception& e) {
      fl_method_call_respond_error(call, "desktop_pointer", e.what(), nullptr, nullptr);
    }
  }

  void Handle(FlMethodCall* call) {
    const gchar* method = fl_method_call_get_name(call);
    if (strcmp(method, "lock") == 0) {
      if (!Lock()) throw std::runtime_error("Cannot hold the pointer on this display");
      fl_method_call_respond_success(call, nullptr, nullptr);
      return;
    }
    if (strcmp(method, "unlock") == 0) {
      Unlock();
      fl_method_call_respond_success(call, nullptr, nullptr);
      return;
    }
    const bool create = strcmp(method, "cursor_create") == 0;
    const bool set = strcmp(method, "cursor_set") == 0;
    if (!create && !set && strcmp(method, "cursor_delete") != 0) {
      fl_method_call_respond_not_implemented(call, nullptr);
      return;
    }
    FlValue* args = fl_method_call_get_args(call);
    if (!args || fl_value_get_type(args) != FL_VALUE_TYPE_MAP) {
      throw std::runtime_error("Missing desktop pointer arguments");
    }
    const int64_t id = Number(args, "id");
    const auto found = cursors_.find(id);
    if (create) {
      const int64_t width = Number(args, "width"), height = Number(args, "height");
      const int64_t hot_x = Number(args, "hot_x"), hot_y = Number(args, "hot_y");
      if (width < 1 || height < 1 || width > kMaxSide || height > kMaxSide || hot_x < 0 || hot_y < 0 ||
          hot_x >= width || hot_y >= height) {
        throw std::runtime_error("Invalid cursor size or hotspot");
      }
      FlValue* rgba = fl_value_lookup_string(args, "rgba");
      if (!rgba || fl_value_get_type(rgba) != FL_VALUE_TYPE_UINT8_LIST ||
          fl_value_get_length(rgba) != static_cast<size_t>(width * height * 4)) {
        throw std::runtime_error("Invalid cursor pixels");
      }
      const int scale = Scale(args);
      GdkCursor* cursor =
          MakeCursor(fl_value_get_uint8_list(rgba), static_cast<int>(width), static_cast<int>(height),
                     static_cast<int>(hot_x), static_cast<int>(hot_y), scale);
      if (!cursor) throw std::runtime_error("Cannot create the cursor");
      if (found != cursors_.end()) {
        // An id held already is one a hot restart of Dart's counter reuses:
        // the old cursor goes (a window showing it keeps its own reference).
        g_object_unref(found->second);
        found->second = cursor;
      } else {
        cursors_[id] = cursor;
      }
    } else if (set) {
      if (found == cursors_.end()) throw std::runtime_error("Unknown cursor");
      GtkWidget* toplevel = gtk_widget_get_toplevel(GTK_WIDGET(view_));
      GdkWindow* window = toplevel ? gtk_widget_get_window(toplevel) : nullptr;
      if (!window) throw std::runtime_error("No window to set the cursor on");
      gdk_window_set_cursor(window, found->second);
    } else if (found != cursors_.end()) {
      // A window keeps its own reference to the cursor it shows.
      g_object_unref(found->second);
      cursors_.erase(found);
    }
    fl_method_call_respond_success(call, nullptr, nullptr);
  }

  // The bitmap at `scale`: a cairo ARGB32 surface (premultiplied, one
  // native-endian word a pixel) of whole logical pixels, its hotspot on a
  // whole logical pixel too. GDK keeps a cursor's hotspot in whole logical
  // pixels on X11 and Wayland, and a Wayland cursor buffer must be a
  // multiple of its scale, so transparent pixels pad the bitmap on the left
  // and top until the hotspot falls on a logical pixel's corner, and on the
  // right and bottom to whole logical pixels.
  GdkCursor* MakeCursor(const uint8_t* rgba, int width, int height, int hot_x, int hot_y, int scale) const {
    const int pad_x = (scale - hot_x % scale) % scale;
    const int pad_y = (scale - hot_y % scale) % scale;
    const int padded_width = (pad_x + width + scale - 1) / scale * scale;
    const int padded_height = (pad_y + height + scale - 1) / scale * scale;
    cairo_surface_t* surface = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, padded_width, padded_height);
    if (cairo_surface_status(surface) != CAIRO_STATUS_SUCCESS) {
      cairo_surface_destroy(surface);
      return nullptr;
    }
    cairo_surface_flush(surface);
    unsigned char* data = cairo_image_surface_get_data(surface);
    const int stride = cairo_image_surface_get_stride(surface);
    for (int y = 0; y < padded_height; ++y) {
      auto* row = reinterpret_cast<uint32_t*>(data + static_cast<size_t>(y) * static_cast<size_t>(stride));
      const int source_y = y - pad_y;
      for (int x = 0; x < padded_width; ++x) {
        const int source_x = x - pad_x;
        if (source_x < 0 || source_y < 0 || source_x >= width || source_y >= height) {
          row[x] = 0;
          continue;
        }
        const uint8_t* pixel =
            rgba +
            (static_cast<size_t>(source_y) * static_cast<size_t>(width) + static_cast<size_t>(source_x)) * 4;
        const uint32_t alpha = pixel[3];
        const uint32_t red = (pixel[0] * alpha + 127) / 255;
        const uint32_t green = (pixel[1] * alpha + 127) / 255;
        const uint32_t blue = (pixel[2] * alpha + 127) / 255;
        row[x] = (alpha << 24) | (red << 16) | (green << 8) | blue;
      }
    }
    cairo_surface_mark_dirty(surface);
    cairo_surface_set_device_scale(surface, scale, scale);
    // GDK draws the surface into a cursor of its own. Both hotspot
    // coordinates are whole numbers, which GDK keeps exactly.
    GdkCursor* cursor = gdk_cursor_new_from_surface(gtk_widget_get_display(GTK_WIDGET(view_)), surface,
                                                    static_cast<double>((hot_x + pad_x) / scale),
                                                    static_cast<double>((hot_y + pad_y) / scale));
    cairo_surface_destroy(surface);
    return cursor;
  }

  // Holds the pointer, or keeps holding it; false where it cannot.
  bool Lock() {
    if (locked_) return true;
    GtkWidget* toplevel = gtk_widget_get_toplevel(GTK_WIDGET(view_));
    GdkWindow* window = toplevel ? gtk_widget_get_window(toplevel) : nullptr;
    if (!window) return false;
    GdkDisplay* display = gdk_window_get_display(window);
    bool held = false;
#ifdef PB_MAPPER_LOCK_X11
    if (!held && GDK_IS_X11_DISPLAY(display)) held = LockX11(display, window);
#endif
#ifdef PB_MAPPER_LOCK_WAYLAND
    if (!held && GDK_IS_WAYLAND_DISPLAY(display)) held = LockWayland(display, window);
#endif
    // A build with neither holds nothing.
    (void)display;
    if (!held) return false;
    locked_ = true;
    pending_x_ = 0;
    pending_y_ = 0;
    toplevel_ = toplevel;
    focus_handler_ = g_signal_connect(toplevel, "focus-out-event", G_CALLBACK(OnFocusOut), this);
    return true;
  }

  // Lets the pointer go, if held: shown again, where it was held.
  void Unlock() {
    if (!locked_) return;
    locked_ = false;
    if (toplevel_ && focus_handler_ != 0) g_signal_handler_disconnect(toplevel_, focus_handler_);
    toplevel_ = nullptr;
    focus_handler_ = 0;
#ifdef PB_MAPPER_LOCK_X11
    UnlockX11();
#endif
#ifdef PB_MAPPER_LOCK_WAYLAND
    UnlockWayland();
#endif
    pending_x_ = 0;
    pending_y_ = 0;
  }

  // The toplevel lost the focus, or the compositor ended the lock: let go,
  // and tell Dart.
  void Lost() {
    if (!locked_) return;
    Unlock();
    fl_method_channel_invoke_method(channel_, "lock_lost", nullptr, nullptr, nullptr, nullptr);
  }

  static gboolean OnFocusOut(GtkWidget*, GdkEvent*, gpointer data) {
    static_cast<DesktopPointer*>(data)->Lost();
    return FALSE;
  }

  // Raw motion while held, summed until the next turn of the main loop.
  void Moved(double dx, double dy) {
    if (!locked_) return;
    pending_x_ += dx;
    pending_y_ += dy;
    if (post_source_ == 0) post_source_ = g_idle_add_full(G_PRIORITY_DEFAULT, OnPost, this, nullptr);
  }

  static gboolean OnPost(gpointer data) {
    auto* self = static_cast<DesktopPointer*>(data);
    self->post_source_ = 0;
    self->Post();
    return G_SOURCE_REMOVE;
  }

  // The whole counts summed so far go to Dart; what falls short waits.
  void Post() {
    if (!locked_) return;
#ifdef PB_MAPPER_LOCK_X11
    WarpBack();
#endif
    const double dx = std::trunc(pending_x_), dy = std::trunc(pending_y_);
    pending_x_ -= dx;
    pending_y_ -= dy;
    if (dx == 0 && dy == 0) return;
    g_autoptr(FlValue) motion = fl_value_new_map();
    fl_value_set_string_take(motion, "dx", fl_value_new_int(static_cast<int64_t>(dx)));
    fl_value_set_string_take(motion, "dy", fl_value_new_int(static_cast<int64_t>(dy)));
    fl_method_channel_invoke_method(channel_, "lock_motion", motion, nullptr, nullptr, nullptr);
  }

#ifdef PB_MAPPER_LOCK_X11
  bool LockX11(GdkDisplay* display, GdkWindow* window) {
    Display* xdisplay = gdk_x11_display_get_xdisplay(display);
    int first_event = 0, first_error = 0;
    if (!XQueryExtension(xdisplay, "XInputExtension", &xi_opcode_, &first_event, &first_error)) return false;
    GdkSeat* seat = gdk_display_get_default_seat(display);
    GdkDevice* pointer = seat ? gdk_seat_get_pointer(seat) : nullptr;
    if (!pointer) return false;
    GdkScreen* screen = nullptr;
    gint x = 0, y = 0;
    gdk_device_get_position(pointer, &screen, &x, &y);
    if (!screen) return false;
    GdkCursor* blank = gdk_cursor_new_for_display(display, GDK_BLANK_CURSOR);
    const GdkGrabStatus grabbed =
        gdk_seat_grab(seat, window, GDK_SEAT_CAPABILITY_ALL_POINTING, TRUE, blank, nullptr, nullptr, nullptr);
    if (blank) g_object_unref(blank);
    if (grabbed != GDK_GRAB_SUCCESS) return false;
    // Raw motion of every master pointer, which only the root window gets.
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(bits, XI_RawMotion);
    XIEventMask mask;
    mask.deviceid = XIAllMasterDevices;
    mask.mask_len = static_cast<int>(sizeof(bits));
    mask.mask = bits;
    gdk_x11_display_error_trap_push(display);
    XISelectEvents(xdisplay, DefaultRootWindow(xdisplay), &mask, 1);
    if (gdk_x11_display_error_trap_pop(display) != 0) {
      gdk_seat_ungrab(seat);
      return false;
    }
    gdk_window_add_filter(nullptr, OnXEvent, this);
    x11_display_ = display;
    x11_seat_ = seat;
    x11_pointer_ = pointer;
    x11_screen_ = screen;
    hold_x_ = x;
    hold_y_ = y;
    return true;
  }

  void UnlockX11() {
    if (!x11_display_) return;
    gdk_window_remove_filter(nullptr, OnXEvent, this);
    Display* xdisplay = gdk_x11_display_get_xdisplay(x11_display_);
    unsigned char none[XIMaskLen(XI_LASTEVENT)] = {0};
    XIEventMask mask;
    mask.deviceid = XIAllMasterDevices;
    mask.mask_len = static_cast<int>(sizeof(none));
    mask.mask = none;
    gdk_x11_display_error_trap_push(x11_display_);
    XISelectEvents(xdisplay, DefaultRootWindow(xdisplay), &mask, 1);
    gdk_x11_display_error_trap_pop_ignored(x11_display_);
    gdk_seat_ungrab(x11_seat_);
    gdk_device_warp(x11_pointer_, x11_screen_, hold_x_, hold_y_);
    x11_display_ = nullptr;
    x11_seat_ = nullptr;
    x11_pointer_ = nullptr;
    x11_screen_ = nullptr;
  }

  // Back to where the pointer was held: a warp makes no raw motion.
  void WarpBack() {
    if (x11_pointer_) gdk_device_warp(x11_pointer_, x11_screen_, hold_x_, hold_y_);
  }

  // Every X event, ahead of GDK: a raw motion is the mouse's own counts, its
  // first two valuators. GDK has read the cookie's data for its filters;
  // one it has not is read, and freed, here.
  static GdkFilterReturn OnXEvent(GdkXEvent* native, GdkEvent*, gpointer data) {
    auto* self = static_cast<DesktopPointer*>(data);
    auto* xevent = static_cast<XEvent*>(native);
    XGenericEventCookie* cookie = &xevent->xcookie;
    if (xevent->type != GenericEvent || cookie->extension != self->xi_opcode_ || cookie->evtype != XI_RawMotion) {
      return GDK_FILTER_CONTINUE;
    }
    const bool read_here = cookie->data == nullptr && XGetEventData(cookie->display, cookie);
    if (const auto* raw = static_cast<const XIRawEvent*>(cookie->data)) {
      double moved[2] = {0, 0};
      const double* value = raw->raw_values;
      for (int axis = 0; axis < raw->valuators.mask_len * 8; ++axis) {
        if (!XIMaskIsSet(raw->valuators.mask, axis)) continue;
        if (axis < 2) moved[axis] = *value;
        ++value;
      }
      self->Moved(moved[0], moved[1]);
    }
    if (read_here) XFreeEventData(cookie->display, cookie);
    return GDK_FILTER_REMOVE;
  }
#endif

#ifdef PB_MAPPER_LOCK_WAYLAND
  bool LockWayland(GdkDisplay* display, GdkWindow* window) {
    if (!BindWayland(display)) return false;
    wl_surface* surface = gdk_wayland_window_get_wl_surface(window);
    GdkSeat* seat = gdk_display_get_default_seat(display);
    GdkDevice* device = seat ? gdk_seat_get_pointer(seat) : nullptr;
    wl_pointer* pointer = device ? gdk_wayland_device_get_wl_pointer(device) : nullptr;
    if (!surface || !pointer) return false;
    // A oneshot lock ends for good once the compositor ends it: a new one is
    // made only after it is destroyed, as the protocol requires.
    static const zwp_locked_pointer_v1_listener locked_listener = {OnLocked, OnUnlocked};
    static const zwp_relative_pointer_v1_listener relative_listener = {OnRelativeMotion};
    locked_pointer_ = zwp_pointer_constraints_v1_lock_pointer(constraints_, surface, pointer, nullptr,
                                                              ZWP_POINTER_CONSTRAINTS_V1_LIFETIME_ONESHOT);
    relative_ = zwp_relative_pointer_manager_v1_get_relative_pointer(relative_manager_, pointer);
    zwp_locked_pointer_v1_add_listener(locked_pointer_, &locked_listener, this);
    zwp_relative_pointer_v1_add_listener(relative_, &relative_listener, this);
    // Hidden while held. The cursor the window showed comes back as the lock
    // ends, unless the engine set another one meanwhile.
    blank_cursor_ = gdk_cursor_new_for_display(display, GDK_BLANK_CURSOR);
    if (blank_cursor_) {
      GdkCursor* shown = gdk_window_get_cursor(window);
      saved_cursor_ = shown ? GDK_CURSOR(g_object_ref(shown)) : nullptr;
      cursor_window_ = GDK_WINDOW(g_object_ref(window));
      gdk_window_set_cursor(window, blank_cursor_);
    }
    wayland_display_ = gdk_wayland_display_get_wl_display(display);
    wl_display_flush(wayland_display_);
    return true;
  }

  void UnlockWayland() {
    if (locked_pointer_) zwp_locked_pointer_v1_destroy(locked_pointer_);
    if (relative_) zwp_relative_pointer_v1_destroy(relative_);
    locked_pointer_ = nullptr;
    relative_ = nullptr;
    if (cursor_window_) {
      if (gdk_window_get_cursor(cursor_window_) == blank_cursor_) {
        gdk_window_set_cursor(cursor_window_, saved_cursor_);
      }
      g_object_unref(cursor_window_);
      cursor_window_ = nullptr;
    }
    if (saved_cursor_) g_object_unref(saved_cursor_);
    if (blank_cursor_) g_object_unref(blank_cursor_);
    saved_cursor_ = nullptr;
    blank_cursor_ = nullptr;
    if (wayland_display_) wl_display_flush(wayland_display_);
    wayland_display_ = nullptr;
  }

  // The compositor's pointer-constraints and relative-pointer managers, bound
  // once on a queue of this runner's own, then moved to GDK's, which
  // dispatches what their objects say.
  bool BindWayland(GdkDisplay* display) {
    if (relative_manager_ && constraints_) return true;
    wl_display* wayland = gdk_wayland_display_get_wl_display(display);
    if (!wayland) return false;
    wl_event_queue* queue = wl_display_create_queue(wayland);
    if (!queue) return false;
    wl_registry* registry = wl_display_get_registry(wayland);
    wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(registry), queue);
    static const wl_registry_listener registry_listener = {OnGlobal, OnGlobalRemove};
    wl_registry_add_listener(registry, &registry_listener, this);
    wl_display_roundtrip_queue(wayland, queue);
    wl_registry_destroy(registry);
    if (relative_manager_) wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(relative_manager_), nullptr);
    if (constraints_) wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(constraints_), nullptr);
    wl_event_queue_destroy(queue);
    return relative_manager_ && constraints_;
  }

  static void OnGlobal(void* data, wl_registry* registry, uint32_t name, const char* interface, uint32_t) {
    auto* self = static_cast<DesktopPointer*>(data);
    if (strcmp(interface, zwp_relative_pointer_manager_v1_interface.name) == 0 && !self->relative_manager_) {
      self->relative_manager_ = static_cast<zwp_relative_pointer_manager_v1*>(
          wl_registry_bind(registry, name, &zwp_relative_pointer_manager_v1_interface, 1));
    } else if (strcmp(interface, zwp_pointer_constraints_v1_interface.name) == 0 && !self->constraints_) {
      self->constraints_ = static_cast<zwp_pointer_constraints_v1*>(
          wl_registry_bind(registry, name, &zwp_pointer_constraints_v1_interface, 1));
    }
  }

  static void OnGlobalRemove(void*, wl_registry*, uint32_t) {}

  static void OnRelativeMotion(void* data, zwp_relative_pointer_v1*, uint32_t, uint32_t, wl_fixed_t, wl_fixed_t,
                               wl_fixed_t dx_unaccel, wl_fixed_t dy_unaccel) {
    static_cast<DesktopPointer*>(data)->Moved(wl_fixed_to_double(dx_unaccel), wl_fixed_to_double(dy_unaccel));
  }

  static void OnLocked(void*, zwp_locked_pointer_v1*) {}

  static void OnUnlocked(void* data, zwp_locked_pointer_v1*) { static_cast<DesktopPointer*>(data)->Lost(); }
#endif

  FlView* view_;
  FlMethodChannel* channel_ = nullptr;
  // This runner's reference to each cursor Dart made.
  std::map<int64_t, GdkCursor*> cursors_;
  // The pointer is held; the toplevel whose focus-out-event lets it go.
  bool locked_ = false;
  GtkWidget* toplevel_ = nullptr;
  gulong focus_handler_ = 0;
  // Raw motion not yet posted to Dart, and the idle source that posts it.
  double pending_x_ = 0;
  double pending_y_ = 0;
  guint post_source_ = 0;
#ifdef PB_MAPPER_LOCK_X11
  int xi_opcode_ = -1;
  // While held: the display, seat and pointer grabbed, and where it was held.
  GdkDisplay* x11_display_ = nullptr;
  GdkSeat* x11_seat_ = nullptr;
  GdkDevice* x11_pointer_ = nullptr;
  GdkScreen* x11_screen_ = nullptr;
  gint hold_x_ = 0;
  gint hold_y_ = 0;
#endif
#ifdef PB_MAPPER_LOCK_WAYLAND
  zwp_relative_pointer_manager_v1* relative_manager_ = nullptr;
  zwp_pointer_constraints_v1* constraints_ = nullptr;
  // While held.
  zwp_locked_pointer_v1* locked_pointer_ = nullptr;
  zwp_relative_pointer_v1* relative_ = nullptr;
  wl_display* wayland_display_ = nullptr;
  // The window hidden while held, its blank cursor, and what it showed.
  GdkWindow* cursor_window_ = nullptr;
  GdkCursor* blank_cursor_ = nullptr;
  GdkCursor* saved_cursor_ = nullptr;
#endif
};
}  // namespace

void desktop_pointer_register(FlView* view) {
  g_autoptr(FlPluginRegistrar) registrar =
      fl_plugin_registry_get_registrar_for_plugin(FL_PLUGIN_REGISTRY(view), "PbMapperDesktopPointer");
  g_object_set_data_full(G_OBJECT(view), "pb-mapper-desktop-pointer", new DesktopPointer(view, registrar),
                         [](gpointer data) { delete static_cast<DesktopPointer*>(data); });
}
