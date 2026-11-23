// The `pb_mapper/desktop_pointer` channel of the Linux runner, exactly as
// ui/lib/src/ffi/desktop_pointer.dart calls it, like the Windows runner
// (ui/windows/runner/desktop_pointer.cpp) and the macOS one
// (ui/darwin/DesktopPointer.swift). It makes a remote computer's pointer
// bitmap this computer's own cursor, which then moves with the hardware and
// changes the moment Dart sets it, with no Flutter frame.
//
// `cursor_create {id, rgba, width, height, hot_x, hot_y}` makes a GdkCursor
// from straight RGBA in physical pixels, `cursor_set {id}` sets it on the
// window the engine sets its own cursors on (the toplevel's, fl_view.cc), so
// it stays until Dart activates a system cursor, and `cursor_delete {id}`
// frees it.
#define G_LOG_DOMAIN "pb-mapper-pointer"

#include "desktop_pointer.h"

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <map>
#include <stdexcept>
#include <string>

namespace {
constexpr char kChannel[] = "pb_mapper/desktop_pointer";
// No side Dart sends is longer: 128 logical pixels at the display's scale.
constexpr int64_t kMaxSide = 1024;

int64_t Number(FlValue* map, const char* key) {
  FlValue* value = fl_value_lookup_string(map, key);
  if (!value || fl_value_get_type(value) != FL_VALUE_TYPE_INT) {
    throw std::runtime_error(std::string("Missing desktop pointer field ") + key);
  }
  return fl_value_get_int(value);
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
      if (found != cursors_.end()) throw std::runtime_error("Cursor id in use");
      GdkCursor* cursor = MakeCursor(fl_value_get_uint8_list(rgba), static_cast<int>(width),
                                     static_cast<int>(height), static_cast<int>(hot_x), static_cast<int>(hot_y));
      if (!cursor) throw std::runtime_error("Cannot create the cursor");
      cursors_[id] = cursor;
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

  // The bitmap at the view's scale: a cairo ARGB32 surface (premultiplied,
  // one native-endian word a pixel) of whole logical pixels, padded with
  // transparent ones where the size is not (a Wayland cursor buffer must be
  // a multiple of its scale), its hotspot in logical pixels.
  GdkCursor* MakeCursor(const uint8_t* rgba, int width, int height, int hot_x, int hot_y) const {
    const int scale = std::max(1, gtk_widget_get_scale_factor(GTK_WIDGET(view_)));
    const int padded_width = (width + scale - 1) / scale * scale;
    const int padded_height = (height + scale - 1) / scale * scale;
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
      for (int x = 0; x < padded_width; ++x) {
        if (x >= width || y >= height) {
          row[x] = 0;
          continue;
        }
        const uint8_t* pixel = rgba + (static_cast<size_t>(y) * static_cast<size_t>(width) + static_cast<size_t>(x)) * 4;
        const uint32_t alpha = pixel[3];
        const uint32_t red = (pixel[0] * alpha + 127) / 255;
        const uint32_t green = (pixel[1] * alpha + 127) / 255;
        const uint32_t blue = (pixel[2] * alpha + 127) / 255;
        row[x] = (alpha << 24) | (red << 16) | (green << 8) | blue;
      }
    }
    cairo_surface_mark_dirty(surface);
    cairo_surface_set_device_scale(surface, scale, scale);
    // GDK draws the surface into a cursor of its own.
    GdkCursor* cursor =
        gdk_cursor_new_from_surface(gtk_widget_get_display(GTK_WIDGET(view_)), surface,
                                    static_cast<double>(hot_x) / scale, static_cast<double>(hot_y) / scale);
    cairo_surface_destroy(surface);
    return cursor;
  }

  FlView* view_;
  FlMethodChannel* channel_ = nullptr;
  // This runner's reference to each cursor Dart made.
  std::map<int64_t, GdkCursor*> cursors_;
};
}  // namespace

void desktop_pointer_register(FlView* view) {
  g_autoptr(FlPluginRegistrar) registrar =
      fl_plugin_registry_get_registrar_for_plugin(FL_PLUGIN_REGISTRY(view), "PbMapperDesktopPointer");
  g_object_set_data_full(G_OBJECT(view), "pb-mapper-desktop-pointer", new DesktopPointer(view, registrar),
                         [](gpointer data) { delete static_cast<DesktopPointer*>(data); });
}
