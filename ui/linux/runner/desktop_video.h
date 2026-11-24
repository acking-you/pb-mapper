#ifndef RUNNER_DESKTOP_VIDEO_H_
#define RUNNER_DESKTOP_VIDEO_H_

#include <flutter_linux/flutter_linux.h>

// Registers the `pb_mapper/desktop_video` method channel (native H.264 and
// H.265 into Flutter textures, and the refresh of the display `view`'s
// window is on, which a viewer follows) on `view`'s engine, for as long as
// the view lives. `plane`, the video plane under the view
// (pb_plane_area_new) or null, shows the pictures of the first texture
// that asks for it.
void desktop_video_register(FlView* view, GtkWidget* plane);

#endif  // RUNNER_DESKTOP_VIDEO_H_
