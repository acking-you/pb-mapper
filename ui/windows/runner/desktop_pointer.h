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
//
// `lock` holds the pointer over the remote picture for a game or a 3D view:
// it confines the pointer to the pixel it is on (ClipCursor), hides it
// (ShowCursor), and registers the top-level window for raw mouse input, whose
// relative motion (WM_INPUT, the mouse's own counts) it sums and posts to
// Dart once a turn of the message loop as `lock_motion {dx, dy}`. Legacy mouse
// messages still come, so a click reaches Flutter where the pointer is.
// `unlock` lets go; so does the window going inactive, which it tells Dart as
// `lock_lost`, and this object going away. Each path frees the clip, shows
// the pointer and removes the registration.
class DesktopPointer {
 public:
  DesktopPointer(flutter::FlutterEngine* engine, HWND window);
  ~DesktopPointer();
  DesktopPointer(const DesktopPointer&) = delete;
  DesktopPointer& operator=(const DesktopPointer&) = delete;

  // A message of the top-level window: raw input and the window going
  // inactive are read and go on to their default handling (false); the
  // runner's own motion message is handled here (true).
  bool HandleMessage(UINT message, WPARAM wparam, LPARAM lparam);

 private:
  void Destroy(HCURSOR cursor);
  void DestroyRetired();
  // False when Windows would not deliver raw mouse input.
  bool Lock();
  void Unlock();

  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> channel_;
  std::map<int64_t, HCURSOR> cursors_;
  // Deleted while still the current cursor, which DestroyIcon may refuse:
  // destroyed by a later call, once another cursor shows.
  std::vector<HCURSOR> retired_;
  // SPI_GETWHEELSCROLLLINES as the runner started.
  UINT wheel_lines_ = 3;
  // The top-level window: raw input's target, and where motion is posted.
  HWND window_;
  bool locked_ = false;
  // Raw motion not yet posted to Dart, and whether a post is queued.
  int64_t pending_x_ = 0;
  int64_t pending_y_ = 0;
  bool posted_ = false;
};

#endif  // RUNNER_DESKTOP_POINTER_H_
