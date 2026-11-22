#ifndef RUNNER_DESKTOP_KEY_CAPTURE_H_
#define RUNNER_DESKTOP_KEY_CAPTURE_H_

#include <flutter/encodable_value.h>
#include <flutter/flutter_engine.h>
#include <flutter/method_channel.h>
#include <windows.h>

#include <atomic>
#include <memory>
#include <thread>

// The `pb_mapper/key_capture` channel. While the remote picture fills the
// screen with the keyboard and control, Dart turns capture on
// (`setCaptured(true)`), and this computer's own shortcuts act on the remote
// computer alone. A low-level keyboard hook, on a thread of its own, keeps
// them from this computer's system and posts each to the window, which hands
// it to Dart as `key` {vk, scan, down, repeat}:
//
// - the Windows key and Alt, each with every key pressed while it is held
//   (Alt+Tab, Alt+F4, Win+D, Win+Space); AltGr is left alone, since it types
//   characters on this computer's layout;
// - Esc while Ctrl is held (the Start menu, Task Manager);
// - F10, which opens the window's menu, unless this computer's input method
//   composes (`setComposing`), and Print Screen.
//
// Every other key reaches the window as before, so this computer's input
// method still composes. Only while this window is in front; a key taken goes
// up through Dart too, wherever and whenever it is released, and when the
// secure desktop takes the keyboard. Capture stops by itself once Dart no
// longer takes the keys (a viewer gone, a hot restart). Ctrl+Alt+Del and
// Win+L act on this computer too: Windows handles them before any hook.
class DesktopKeyCapture {
 public:
  DesktopKeyCapture(flutter::FlutterEngine* engine, HWND window);
  ~DesktopKeyCapture();

  // A key the hook took, posted to `window`: true once handled.
  bool HandleMessage(UINT message, WPARAM wparam, LPARAM lparam);

 private:
  // False when the hook could not be installed.
  bool SetCaptured(bool captured);

  HWND window_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> channel_;
  std::thread thread_;
  DWORD thread_id_ = 0;
  // Read by the hook's thread, which ends before this does.
  std::atomic<bool> composing_{false};
  // Gone with this object, so a reply that comes later finds nothing.
  std::shared_ptr<DesktopKeyCapture*> self_ =
      std::make_shared<DesktopKeyCapture*>(this);
};

#endif
