// The video plane of the Linux viewer: a GtkGLArea under the Flutter view,
// which shows one texture's pictures in the rectangle Dart places, where
// the viewer leaves a transparent hole, painted by GTK's frame clock (at
// the display's refresh where the compositor paces it: Wayland, or an X11
// compositor that reports its frames) instead of by Flutter's frames, which
// its Linux embedder times at 60 a second. desktop_video.cc decides which
// texture it shows and takes the pictures (the receipt); this file is the
// widget, its GL context and its frame timings.
#ifndef RUNNER_DESKTOP_VIDEO_PLANE_H_
#define RUNNER_DESKTOP_VIDEO_PLANE_H_

#include <flutter_linux/flutter_linux.h>
#include <gtk/gtk.h>

#include "desktop_video_gl.h"
#include "desktop_video_module.h"

typedef struct _PbPlaneArea PbPlaneArea;

GType pb_plane_area_get_type(void);
#define PB_PLANE_AREA(obj) (G_TYPE_CHECK_INSTANCE_CAST((obj), pb_plane_area_get_type(), PbPlaneArea))

// A hidden plane for the window `view` lies over. While it shows, `view`'s
// background is transparent; otherwise it is opaque black, as the runner
// sets it.
GtkWidget* pb_plane_area_new(FlView* view);

namespace pb_video {

// What a plane shows: one texture's pictures.
struct PlaneSource {
  // The texture (a GObject), referenced while the plane shows it.
  gpointer texture = nullptr;
  const PbVideoModule* module = nullptr;
  // GTK thread, the plane's context current, `gl` its converter: when a
  // newer picture waits, or `wake` (a still is due), takes the newest into
  // `gl` and presents it, which is its receipt. True when `gl` holds
  // another picture since.
  bool (*take)(gpointer texture, VideoGl* gl, bool wake) = nullptr;
  // Whether a newer picture waits. Any thread.
  bool (*pending)(gpointer texture) = nullptr;
};

// Where Dart's viewer shows the picture, in the Flutter view's physical
// pixels (GDK's: the view and the plane fill the same overlay): the
// picture's rectangle and the part of it on screen.
struct Placement {
  bool visible = false;
  PlaneRect picture, clip;
};

// The plane's presentation, while its timings are on.
struct PlaneStats {
  // From the presentation (or, where GDK has none, the frame) times of the
  // frames that drew a newer picture: p50 and p95 of the intervals between
  // them, over the last few hundred; zero before two.
  size_t intervals = 0;
  double interval_p50_ms = 0, interval_p95_ms = 0;
  // The refresh interval the newest of GDK's recent frame timings carries:
  // the one an X11 compositor reports, or on Wayland that of the output's
  // mode (where GDK puts 1/60 s for an output it does not know). 0 when
  // none carries one, as on an X11 desktop whose compositor reports no
  // frame timings, where GDK paces its frames at 1/60 s.
  double refresh_interval_ms = 0;
};

}  // namespace pb_video

// The GTK thread, all of them.
//
// Whether the plane can show pictures: its GL area realizes, with no error,
// in GL ES or desktop GL 3.2 or later, and builds its program. It is
// realized the first time.
bool pb_plane_area_usable(PbPlaneArea* plane);
// Whether its context is EGL (Wayland), so that it may import the
// decoder's dma-bufs; GDK 3 draws X11 through GLX, which imports none.
bool pb_plane_area_egl(PbPlaneArea* plane);
// Whether it shows a texture's pictures.
bool pb_plane_area_bound(PbPlaneArea* plane);
// Shows `source`'s pictures once a viewer places them.
void pb_plane_area_bind(PbPlaneArea* plane, const pb_video::PlaneSource& source);
// Hides the plane, restores the view's opaque background, frees its
// converter, which releases the pictures it holds, and lets the texture go.
void pb_plane_area_unbind(PbPlaneArea* plane);
// A newer picture waits: the placed picture is drawn again at the next
// frame. If no frame takes it within 100 ms (the window covered, or on
// another workspace, where the compositor stops GDK's frames), the plane
// takes it itself, which keeps the receipts coming.
void pb_plane_area_ready(PbPlaneArea* plane);
// Takes the newest picture now, and the still a snapshot or the still timer
// wants.
void pb_plane_area_wake(PbPlaneArea* plane);
void pb_plane_area_place(PbPlaneArea* plane, const pb_video::Placement& placement);
// The reconnect's grey still, through the grey still's matrix.
void pb_plane_area_grey(PbPlaneArea* plane, bool on);
// `timings` starts the interval figures, or keeps them; false drops them.
pb_video::PlaneStats pb_plane_area_stats(PbPlaneArea* plane, bool timings);

#endif  // RUNNER_DESKTOP_VIDEO_PLANE_H_
