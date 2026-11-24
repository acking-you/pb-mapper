#include "desktop_pointer.h"

#include <flutter/standard_method_codec.h>

#include <stdexcept>
#include <string>
#include <utility>
#include <variant>

namespace {
using Value = flutter::EncodableValue;
using Map = flutter::EncodableMap;

// No side Dart sends is longer: 128 logical pixels at the display's scale.
constexpr int64_t kMaxSide = 1024;

int64_t Number(const Map& map, const char* key) {
  const auto it = map.find(Value(key));
  if (it != map.end()) {
    if (const auto* value = std::get_if<int32_t>(&it->second)) return *value;
    if (const auto* value = std::get_if<int64_t>(&it->second)) return *value;
  }
  throw std::runtime_error(std::string("Missing desktop pointer field ") + key);
}

// A cursor from straight RGBA: the colour as a 32-bpp top-down DIB section
// with its alpha (BITMAPV5HEADER with an alpha mask, as Microsoft's
// alpha-blended cursor sample makes it; the system blends straight alpha),
// and a monochrome AND mask from the alpha for a display that ignores it:
// transparent below half. CreateIconIndirect copies both bitmaps.
HCURSOR MakeCursor(const std::vector<uint8_t>& rgba, int width, int height, int hot_x, int hot_y) {
  BITMAPV5HEADER header{};
  header.bV5Size = sizeof(header);
  header.bV5Width = width;
  header.bV5Height = -height;
  header.bV5Planes = 1;
  header.bV5BitCount = 32;
  header.bV5Compression = BI_BITFIELDS;
  header.bV5RedMask = 0x00FF0000;
  header.bV5GreenMask = 0x0000FF00;
  header.bV5BlueMask = 0x000000FF;
  header.bV5AlphaMask = 0xFF000000;
  void* bits = nullptr;
  HBITMAP color = ::CreateDIBSection(nullptr, reinterpret_cast<const BITMAPINFO*>(&header),
                                     DIB_RGB_COLORS, &bits, nullptr, 0);
  if (!color) return nullptr;
  if (!bits) {
    ::DeleteObject(color);
    return nullptr;
  }
  // Monochrome rows are WORD-aligned; a set bit keeps the screen.
  const size_t mask_stride = (static_cast<size_t>(width) + 15) / 16 * 2;
  std::vector<uint8_t> mask(mask_stride * static_cast<size_t>(height), 0);
  auto* out = static_cast<uint8_t*>(bits);
  for (int y = 0; y < height; ++y) {
    for (int x = 0; x < width; ++x) {
      const size_t at = (static_cast<size_t>(y) * static_cast<size_t>(width) + static_cast<size_t>(x)) * 4;
      const uint8_t alpha = rgba[at + 3];
      if (alpha) {
        out[at] = rgba[at + 2];
        out[at + 1] = rgba[at + 1];
        out[at + 2] = rgba[at];
      } else {
        // A transparent pixel carries no colour, which the mask's XOR
        // would otherwise draw.
        out[at] = out[at + 1] = out[at + 2] = 0;
      }
      out[at + 3] = alpha;
      if (alpha < 128) {
        uint8_t& byte = mask[static_cast<size_t>(y) * mask_stride + static_cast<size_t>(x / 8)];
        byte = static_cast<uint8_t>(byte | (0x80u >> (x % 8)));
      }
    }
  }
  HBITMAP and_mask = ::CreateBitmap(width, height, 1, 1, mask.data());
  HCURSOR cursor = nullptr;
  if (and_mask) {
    ICONINFO info{};
    info.fIcon = FALSE;
    info.xHotspot = static_cast<DWORD>(hot_x);
    info.yHotspot = static_cast<DWORD>(hot_y);
    info.hbmMask = and_mask;
    info.hbmColor = color;
    cursor = ::CreateIconIndirect(&info);
    ::DeleteObject(and_mask);
  }
  ::DeleteObject(color);
  return cursor;
}
}  // namespace

DesktopPointer::DesktopPointer(flutter::FlutterEngine* engine) {
  channel_ = std::make_unique<flutter::MethodChannel<Value>>(
      engine->messenger(), "pb_mapper/desktop_pointer", &flutter::StandardMethodCodec::GetInstance());
  channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    const std::string& method = call.method_name();
    if (method != "cursor_create" && method != "cursor_set" && method != "cursor_delete") {
      result->NotImplemented();
      return;
    }
    try {
      DestroyRetired();
      const auto* args = call.arguments() ? std::get_if<Map>(call.arguments()) : nullptr;
      if (!args) throw std::runtime_error("Missing desktop pointer arguments");
      const int64_t id = Number(*args, "id");
      if (method == "cursor_create") {
        const int64_t width = Number(*args, "width"), height = Number(*args, "height");
        const int64_t hot_x = Number(*args, "hot_x"), hot_y = Number(*args, "hot_y");
        if (width < 1 || height < 1 || width > kMaxSide || height > kMaxSide || hot_x < 0 ||
            hot_y < 0 || hot_x >= width || hot_y >= height) {
          throw std::runtime_error("Invalid cursor size or hotspot");
        }
        const auto it = args->find(Value("rgba"));
        const auto* rgba = it == args->end() ? nullptr : std::get_if<std::vector<uint8_t>>(&it->second);
        if (!rgba || rgba->size() != static_cast<size_t>(width * height * 4)) {
          throw std::runtime_error("Invalid cursor pixels");
        }
        HCURSOR cursor = MakeCursor(*rgba, static_cast<int>(width), static_cast<int>(height),
                                    static_cast<int>(hot_x), static_cast<int>(hot_y));
        if (!cursor) throw std::runtime_error("Cannot create the cursor");
        const auto held = cursors_.find(id);
        if (held != cursors_.end()) {
          // An id held already is one a hot restart of Dart's counter
          // reuses: the old cursor goes.
          const HCURSOR old = held->second;
          held->second = cursor;
          Destroy(old);
        } else {
          cursors_.emplace(id, cursor);
        }
      } else if (method == "cursor_set") {
        const auto found = cursors_.find(id);
        if (found == cursors_.end()) throw std::runtime_error("Unknown cursor");
        ::SetCursor(found->second);
      } else {
        const auto found = cursors_.find(id);
        if (found != cursors_.end()) {
          const HCURSOR cursor = found->second;
          cursors_.erase(found);
          Destroy(cursor);
        }
      }
      result->Success();
    } catch (const std::exception& e) {
      result->Error("desktop_pointer", e.what());
    }
  });
}

DesktopPointer::~DesktopPointer() {
  channel_->SetMethodCallHandler(nullptr);
  for (const auto& entry : cursors_) ::DestroyIcon(entry.second);
  for (const HCURSOR cursor : retired_) ::DestroyIcon(cursor);
}

void DesktopPointer::Destroy(HCURSOR cursor) {
  if (::GetCursor() == cursor) {
    retired_.push_back(cursor);
    return;
  }
  ::DestroyIcon(cursor);
}

void DesktopPointer::DestroyRetired() {
  if (retired_.empty()) return;
  const HCURSOR current = ::GetCursor();
  std::vector<HCURSOR> kept;
  for (const HCURSOR cursor : retired_) {
    if (cursor == current) {
      kept.push_back(cursor);
    } else {
      ::DestroyIcon(cursor);
    }
  }
  retired_ = std::move(kept);
}
