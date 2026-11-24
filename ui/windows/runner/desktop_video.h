#ifndef RUNNER_DESKTOP_VIDEO_H_
#define RUNNER_DESKTOP_VIDEO_H_

#include <flutter/flutter_engine.h>
#include <windows.h>
#include <memory>

// Decoder resources are allocated only when a remote video texture is opened.
// `window` is the top-level window, whose messages HandleMessage sees;
// `view` is Flutter's view in it, whose display's refresh `refresh_hz`
// answers.
class DesktopVideo {
 public:
  DesktopVideo(flutter::FlutterEngine* engine, HWND window, HWND view);
  ~DesktopVideo();
  // True for a message only this handles; a window move or display change it
  // watches goes on to the window.
  bool HandleMessage(UINT message);

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
#endif
