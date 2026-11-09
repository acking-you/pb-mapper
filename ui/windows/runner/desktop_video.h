#ifndef RUNNER_DESKTOP_VIDEO_H_
#define RUNNER_DESKTOP_VIDEO_H_

#include <flutter/flutter_engine.h>
#include <windows.h>
#include <memory>

// Decoder resources are allocated only when a remote video texture is opened.
class DesktopVideo {
 public:
  DesktopVideo(flutter::FlutterEngine* engine, HWND window);
  ~DesktopVideo();
  bool HandleMessage(UINT message);

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
#endif
