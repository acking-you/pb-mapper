#ifndef RUNNER_DESKTOP_VIDEO_H_
#define RUNNER_DESKTOP_VIDEO_H_

#include <flutter_linux/flutter_linux.h>

// Registers the `pb_mapper/desktop_video` method channel (native H.264 and
// H.265 into Flutter textures) on `view`'s engine, for as long as the view
// lives.
void desktop_video_register(FlView* view);

#endif  // RUNNER_DESKTOP_VIDEO_H_
