#ifndef RUNNER_DESKTOP_IME_H_
#define RUNNER_DESKTOP_IME_H_

#include <flutter/flutter_engine.h>
#include <flutter/method_channel.h>
#include <flutter/encodable_value.h>
#include <windows.h>
#include <memory>

// The `pb_mapper/ime` channel: `setEnabled(bool)` turns the input method of
// the Flutter view's window off while the remote picture takes keys by
// place, so this computer's input method composes nothing, and on again.
// It is on again when this goes, with the window.
class DesktopIme {
 public:
  DesktopIme(flutter::FlutterEngine* engine, HWND view);
  ~DesktopIme();

 private:
  void SetEnabled(bool enabled);

  HWND view_;
  bool enabled_ = true;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> channel_;
};
#endif
