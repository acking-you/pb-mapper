#ifndef RUNNER_DESKTOP_POINTER_H_
#define RUNNER_DESKTOP_POINTER_H_

#include <flutter/encodable_value.h>
#include <flutter/flutter_engine.h>
#include <flutter/method_channel.h>
#include <windows.h>

#include <cstdint>
#include <map>
#include <memory>
#include <vector>

// The `pb_mapper/desktop_pointer` channel (ui/lib/src/ffi/desktop_pointer.dart):
// a remote computer's pointer bitmap as this computer's own cursor, which
// moves with the hardware and changes the moment Dart sets it.
// `cursor_create {id, rgba, width, height, hot_x, hot_y, scale}` makes one
// from straight RGBA in physical pixels, replacing any cursor held as `id`
// (`scale`, the device pixel ratio Dart made it for, does not matter here: a
// Windows cursor is physical pixels), `cursor_set {id}` shows it with
// ::SetCursor on this, the platform thread, and `cursor_delete {id}` frees
// it. The engine answers WM_SETCURSOR over the client area without setting a
// cursor, so the one set here stays until Dart activates a system cursor.
// `wheel_lines` answers the lines a wheel notch scrolls
// (SPI_GETWHEELSCROLLLINES, 3 if it cannot be read), read as the runner
// starts, which is when Flutter's embedder reads it for the scroll distance it
// reports: Dart turns that distance back into notches with it.
class DesktopPointer {
 public:
  explicit DesktopPointer(flutter::FlutterEngine* engine);
  ~DesktopPointer();
  DesktopPointer(const DesktopPointer&) = delete;
  DesktopPointer& operator=(const DesktopPointer&) = delete;

 private:
  void Destroy(HCURSOR cursor);
  void DestroyRetired();

  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> channel_;
  std::map<int64_t, HCURSOR> cursors_;
  // Deleted while still the current cursor, which DestroyIcon may refuse:
  // destroyed by a later call, once another cursor shows.
  std::vector<HCURSOR> retired_;
  // SPI_GETWHEELSCROLLLINES as the runner started.
  UINT wheel_lines_ = 3;
};

#endif  // RUNNER_DESKTOP_POINTER_H_
