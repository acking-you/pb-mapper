// The video plane (desktop_video_plane.h). The runner puts it under the
// Flutter view in a GtkOverlay (my_application.cc); it stays hidden, and
// the view opaque, until a viewer places a visible picture on it.
//
// Its GtkGLArea renders on GDK's frame clock whenever GTK draws the
// window. The compositor paces that clock at the display's refresh on
// Wayland and on an X11 desktop whose compositor reports its frames
// (GNOME's); on any other X11 desktop GDK paces it at 60 a second. GTK
// draws for a newer picture (queued at high priority from the decoder's
// thread through desktop_video.cc), or because Flutter or an overlay drew.
// Only the first takes and converts a picture; every render draws the last
// converted one into the placed rectangle, cut to its clip, scaled there by
// the filter PlanPlane picks (desktop_video_gl.h): area-averaged when it
// shows smaller than its own size, and enlarged with the viewer's
// enhancement when it asks for one. Flutter's view above it is transparent
// there, where the viewer leaves a hole.
#define G_LOG_DOMAIN "pb-mapper-video"

#include "desktop_video_plane.h"

#include <algorithm>
#include <cmath>
#include <deque>
#include <memory>
#include <string>
#include <vector>

// Last: Xlib's macros (None, Status) would reach everything after.
#ifdef GDK_WINDOWING_WAYLAND
#include <gdk/gdkwayland.h>
#endif

namespace {

// How long a newer picture may wait for a frame before the plane takes it
// without one: well inside Rust's stall watchdog (700 ms), which a covered
// window would otherwise set off with keyframe requests.
constexpr gint64 kStallUs = 100 * G_TIME_SPAN_MILLISECOND;
// The presentation intervals the stats keep, and the frames that drew a
// newer picture whose timings GDK has not completed yet.
constexpr size_t kMaxIntervals = 360;
constexpr size_t kMaxAwaiting = 64;

struct PlaneState {
  pb_video::PlaneSource source;
  // The converter in the plane's context, made at the first take; freed
  // when the texture goes or the context does.
  std::unique_ptr<pb_video::VideoGl> gl;
  pb_video::PlaneGl draw;
  // -1 until pb_plane_area_usable has looked.
  int usable = -1;
  bool failure_logged = false;
  pb_video::Placement placement;
  // Whether the widget shows and the view is transparent.
  bool shown = false;
  bool grey = false;
  pb_video::Enhance enhance = pb_video::Enhance::kOff;
  // Changes whenever the converter holds another picture, or goes.
  uint64_t serial = 0;
  // Since when a newer picture has waited for a frame, 0 when none does;
  // the timer that takes it after kStallUs.
  gint64 pending_since = 0;
  guint stall_timer = 0;
  // While timed: the frame counters of the frames that drew a newer
  // picture, until GDK completes their timings; the time the last one was
  // shown; the intervals between them, in milliseconds.
  bool timing = false;
  std::deque<gint64> awaiting;
  gint64 last_shown = 0;
  std::deque<double> intervals;
};

}  // namespace

struct _PbPlaneArea {
  GtkGLArea parent_instance;
  // A weak pointer: null once the view goes.
  FlView* view;
  PlaneState* state;
};

typedef struct {
  GtkGLAreaClass parent_class;
} PbPlaneAreaClass;

G_DEFINE_TYPE(PbPlaneArea, pb_plane_area, GTK_TYPE_GL_AREA)

namespace {

PlaneState* StateOf(PbPlaneArea* self) { return self->state; }

// Shows or hides the widget, and makes the view's background transparent
// while it shows: FlView paints no background whose four components are
// all zero. Either takes effect at the view's next draw.
void Show(PbPlaneArea* self, bool show) {
  PlaneState* state = StateOf(self);
  if (show == state->shown) return;
  state->shown = show;
  if (show) {
    gtk_widget_show(GTK_WIDGET(self));
  } else {
    gtk_widget_hide(GTK_WIDGET(self));
  }
  if (self->view) {
    const GdkRGBA background = {0, 0, 0, show ? 0.0 : 1.0};
    fl_view_set_background_color(self->view, &background);
    gtk_widget_queue_draw(GTK_WIDGET(self->view));
  }
}

// The placed picture's part on screen, in the widget's own pixels, drawn
// again at the next frame.
void QueuePicture(PbPlaneArea* self) {
  PlaneState* state = StateOf(self);
  GtkWidget* widget = GTK_WIDGET(self);
  const pb_video::PlaneRect& picture = state->placement.picture;
  const pb_video::PlaneRect& clip = state->placement.clip;
  const int64_t left = std::max<int64_t>(picture.x, clip.x);
  const int64_t top = std::max<int64_t>(picture.y, clip.y);
  const int64_t right = std::min<int64_t>(static_cast<int64_t>(picture.x) + picture.width,
                                          static_cast<int64_t>(clip.x) + clip.width);
  const int64_t bottom = std::min<int64_t>(static_cast<int64_t>(picture.y) + picture.height,
                                           static_cast<int64_t>(clip.y) + clip.height);
  if (right <= left || bottom <= top) return;
  const double scale = std::max(1, gtk_widget_get_scale_factor(widget));
  const int x = static_cast<int>(std::max(0.0, std::floor(left / scale)));
  const int y = static_cast<int>(std::max(0.0, std::floor(top / scale)));
  const int x1 = std::min(gtk_widget_get_allocated_width(widget), static_cast<int>(std::ceil(right / scale)));
  const int y1 = std::min(gtk_widget_get_allocated_height(widget), static_cast<int>(std::ceil(bottom / scale)));
  if (x1 > x && y1 > y) gtk_widget_queue_draw_area(widget, x, y, x1 - x, y1 - y);
}

pb_video::Owner OwnerOf(PbPlaneArea* self) {
  return pb_plane_area_egl(self) ? pb_video::Owner::kOwnEgl : pb_video::Owner::kOwnGlx;
}

// With the plane's context current: the source's newest picture into the
// plane's converter. True when it holds another picture since.
bool Take(PbPlaneArea* self, bool wake) {
  PlaneState* state = StateOf(self);
  if (!state->source.texture) return false;
  if (!state->gl) state->gl.reset(new pb_video::VideoGl(state->source.module, false, OwnerOf(self)));
  const bool took = state->source.take(state->source.texture, state->gl.get(), wake);
  // Whatever waited is taken; a newer one says so again.
  state->pending_since = 0;
  if (took) ++state->serial;
  return took;
}

// Frees the converter, which releases the pictures it holds, and the
// drawing's enlargement texture, in the plane's context, or forgets the
// converter's objects when the context is gone.
void FreeConverter(PbPlaneArea* self) {
  PlaneState* state = StateOf(self);
  GtkGLArea* area = GTK_GL_AREA(self);
  const bool context = gtk_widget_get_realized(GTK_WIDGET(self)) && gtk_gl_area_get_context(area);
  if (context) {
    gtk_gl_area_make_current(area);
    if (state->gl) state->gl->Free();
    state->draw.Trim();
    gdk_gl_context_clear_current();
  } else if (state->gl) {
    state->gl->Free(true);
  }
  if (state->gl) ++state->serial;
  state->gl.reset();
}

// Takes the newest picture outside a frame, the plane's context made
// current and released again, and draws it at the next frame.
void TakeNow(PbPlaneArea* self, bool wake) {
  PlaneState* state = StateOf(self);
  GtkGLArea* area = GTK_GL_AREA(self);
  if (!state->source.texture || !gtk_widget_get_realized(GTK_WIDGET(self)) || !gtk_gl_area_get_context(area) ||
      gtk_gl_area_get_error(area)) {
    return;
  }
  gtk_gl_area_make_current(area);
  const bool took = Take(self, wake);
  gdk_gl_context_clear_current();
  if (took && state->shown) QueuePicture(self);
}

gboolean StallDue(gpointer data) {
  PbPlaneArea* self = PB_PLANE_AREA(data);
  PlaneState* state = StateOf(self);
  state->stall_timer = 0;
  if (!state->source.texture || state->pending_since == 0 || !state->source.pending(state->source.texture)) {
    return G_SOURCE_REMOVE;
  }
  const gint64 waited = g_get_monotonic_time() - state->pending_since;
  if (waited < kStallUs) {
    state->stall_timer = g_timeout_add(static_cast<guint>((kStallUs - waited) / G_TIME_SPAN_MILLISECOND) + 1,
                                       StallDue, self);
    return G_SOURCE_REMOVE;
  }
  // No frame came for it: the receipt, and the picture kept for the next.
  TakeNow(self, false);
  return G_SOURCE_REMOVE;
}

void StopStallTimer(PlaneState* state) {
  if (state->stall_timer != 0) g_source_remove(state->stall_timer);
  state->stall_timer = 0;
}

void ResetTimings(PlaneState* state) {
  state->awaiting.clear();
  state->last_shown = 0;
  state->intervals.clear();
}

// The intervals between the frames that drew a newer picture whose timings
// GDK has completed since: presentation times where the compositor reports
// them, else frame times. A frame GDK no longer keeps breaks the series.
void ResolveTimings(PbPlaneArea* self) {
  PlaneState* state = StateOf(self);
  GdkFrameClock* clock = gtk_widget_get_frame_clock(GTK_WIDGET(self));
  if (!clock) return;
  while (!state->awaiting.empty()) {
    GdkFrameTimings* timings = gdk_frame_clock_get_timings(clock, state->awaiting.front());
    if (!timings) {
      state->awaiting.pop_front();
      state->last_shown = 0;
      continue;
    }
    if (!gdk_frame_timings_get_complete(timings)) break;
    gint64 shown = gdk_frame_timings_get_presentation_time(timings);
    if (shown == 0) shown = gdk_frame_timings_get_frame_time(timings);
    if (state->last_shown != 0 && shown > state->last_shown) {
      state->intervals.push_back(static_cast<double>(shown - state->last_shown) / G_TIME_SPAN_MILLISECOND);
      if (state->intervals.size() > kMaxIntervals) state->intervals.pop_front();
    }
    state->last_shown = shown;
    state->awaiting.pop_front();
  }
}

// The refresh interval the newest of GDK's recent frames carries, in
// milliseconds; 0 when none carries one. gdk_frame_clock_get_refresh_info
// answers that case with its own 1/60 s, which would read as a measured
// 60 Hz display.
double RefreshIntervalMs(GdkFrameClock* clock) {
  const gint64 oldest = gdk_frame_clock_get_history_start(clock);
  for (gint64 frame = gdk_frame_clock_get_frame_counter(clock); frame >= oldest; --frame) {
    GdkFrameTimings* timings = gdk_frame_clock_get_timings(clock, frame);
    if (!timings) break;
    const gint64 refresh = gdk_frame_timings_get_refresh_interval(timings);
    if (refresh > 0) return static_cast<double>(refresh) / G_TIME_SPAN_MILLISECOND;
  }
  return 0;
}

void LogFailure(PlaneState* state, const std::string& failure) {
  if (state->failure_logged) return;
  state->failure_logged = true;
  g_warning("The video plane cannot draw: %s", failure.c_str());
}

}  // namespace

// GDK's paint, the area's framebuffer bound and its context current.
static gboolean pb_plane_area_render(GtkGLArea* area, GdkGLContext*) {
  PbPlaneArea* self = PB_PLANE_AREA(area);
  PlaneState* state = StateOf(self);
  GtkWidget* widget = GTK_WIDGET(area);
  GLint framebuffer = 0;
  glGetIntegerv(GL_FRAMEBUFFER_BINDING, &framebuffer);
  // A redraw that Flutter or an overlay caused takes nothing: the last
  // picture is drawn again.
  bool took = false;
  if (state->source.texture && state->source.pending(state->source.texture)) took = Take(self, false);
  glBindFramebuffer(GL_FRAMEBUFFER, static_cast<GLuint>(framebuffer));
  std::string failure;
  GLuint name = 0;
  uint32_t width = 0, height = 0;
  if (state->gl && !state->gl->Texture(&name, &width, &height, &failure)) {
    LogFailure(state, failure);
    name = 0;
  }
  if (!state->draw.Setup(&failure)) LogFailure(state, failure);
  const int scale = gtk_widget_get_scale_factor(widget);
  pb_video::PlaneFrame frame;
  frame.texture = name;
  frame.width = width;
  frame.height = height;
  frame.serial = state->serial;
  frame.picture = state->placement.picture;
  frame.clip = state->placement.clip;
  frame.grey = state->grey;
  frame.enhance = state->enhance;
  frame.target_width = gtk_widget_get_allocated_width(widget) * scale;
  frame.target_height = gtk_widget_get_allocated_height(widget) * scale;
  std::string fallback;
  state->draw.Draw(frame, &fallback);
  // Once for each filter the context cannot run.
  if (!fallback.empty()) g_warning("The video plane scales bilinear instead: %s", fallback.c_str());
  if (state->timing) {
    GdkFrameClock* clock = gtk_widget_get_frame_clock(widget);
    if (took && clock) {
      state->awaiting.push_back(gdk_frame_clock_get_frame_counter(clock));
      if (state->awaiting.size() > kMaxAwaiting) {
        state->awaiting.pop_front();
        state->last_shown = 0;
      }
    }
    ResolveTimings(self);
  }
  return TRUE;
}

// GtkGLArea maps an input-only window over its whole allocation, above the
// Flutter view's when the plane shows after it: it would take every click.
// The plane takes no input; its window goes.
static void pb_plane_area_map(GtkWidget* widget) {
  GTK_WIDGET_CLASS(pb_plane_area_parent_class)->map(widget);
  GdkWindow* parent = gtk_widget_get_parent_window(widget);
  for (GList* child = parent ? gdk_window_peek_children(parent) : nullptr; child; child = child->next) {
    GdkWindow* window = GDK_WINDOW(child->data);
    gpointer owner = nullptr;
    gdk_window_get_user_data(window, &owner);
    if (owner == widget && gdk_window_is_input_only(window)) gdk_window_hide(window);
  }
}

// Before the area's context goes: what the plane made in it.
static void pb_plane_area_unrealize(GtkWidget* widget) {
  PbPlaneArea* self = PB_PLANE_AREA(widget);
  PlaneState* state = StateOf(self);
  GtkGLArea* area = GTK_GL_AREA(widget);
  const bool context = gtk_gl_area_get_context(area) != nullptr;
  if (context) gtk_gl_area_make_current(area);
  if (state->gl) state->gl->Free(!context);
  state->gl.reset();
  state->draw.Free(!context);
  state->usable = -1;
  GTK_WIDGET_CLASS(pb_plane_area_parent_class)->unrealize(widget);
}

static void pb_plane_area_dispose(GObject* object) {
  PbPlaneArea* self = PB_PLANE_AREA(object);
  if (StateOf(self)->source.texture) pb_plane_area_unbind(self);
  if (self->view) {
    g_object_remove_weak_pointer(G_OBJECT(self->view), reinterpret_cast<gpointer*>(&self->view));
    self->view = nullptr;
  }
  G_OBJECT_CLASS(pb_plane_area_parent_class)->dispose(object);
}

static void pb_plane_area_finalize(GObject* object) {
  PbPlaneArea* self = PB_PLANE_AREA(object);
  delete self->state;
  G_OBJECT_CLASS(pb_plane_area_parent_class)->finalize(object);
}

static void pb_plane_area_class_init(PbPlaneAreaClass* klass) {
  GTK_GL_AREA_CLASS(klass)->render = pb_plane_area_render;
  GTK_WIDGET_CLASS(klass)->map = pb_plane_area_map;
  GTK_WIDGET_CLASS(klass)->unrealize = pb_plane_area_unrealize;
  G_OBJECT_CLASS(klass)->dispose = pb_plane_area_dispose;
  G_OBJECT_CLASS(klass)->finalize = pb_plane_area_finalize;
}

static void pb_plane_area_init(PbPlaneArea* self) {
  self->view = nullptr;
  self->state = new PlaneState();
}

GtkWidget* pb_plane_area_new(FlView* view) {
  PbPlaneArea* self = PB_PLANE_AREA(g_object_new(pb_plane_area_get_type(), nullptr));
  // Opaque, and drawn at every draw of the window it shows in.
  gtk_gl_area_set_has_alpha(GTK_GL_AREA(self), FALSE);
  gtk_gl_area_set_auto_render(GTK_GL_AREA(self), TRUE);
  self->view = view;
  if (view) g_object_add_weak_pointer(G_OBJECT(view), reinterpret_cast<gpointer*>(&self->view));
  return GTK_WIDGET(self);
}

bool pb_plane_area_usable(PbPlaneArea* self) {
  PlaneState* state = StateOf(self);
  if (state->usable >= 0) return state->usable == 1;
  GtkWidget* widget = GTK_WIDGET(self);
  GtkGLArea* area = GTK_GL_AREA(self);
  if (!gtk_widget_get_realized(widget)) gtk_widget_realize(widget);
  std::string failure;
  GError* error = gtk_widget_get_realized(widget) ? gtk_gl_area_get_error(area) : nullptr;
  bool usable = false;
  if (!gtk_widget_get_realized(widget)) {
    failure = "its GL area does not realize";
  } else if (error) {
    failure = error->message;
  } else if (!gtk_gl_area_get_context(area)) {
    failure = "it has no GL context";
  } else {
    gtk_gl_area_make_current(area);
    usable = state->draw.Setup(&failure);
    gdk_gl_context_clear_current();
  }
  if (usable) {
    int major = 0, minor = 0;
    GdkGLContext* context = gtk_gl_area_get_context(area);
    gdk_gl_context_get_version(context, &major, &minor);
    g_debug("The video plane draws in %s %d.%d through %s", gdk_gl_context_get_use_es(context) ? "GL ES" : "GL", major,
            minor, pb_plane_area_egl(self) ? "EGL" : "GLX");
  } else {
    g_message("The video plane is unavailable, so pictures go through Flutter: %s", failure.c_str());
  }
  state->usable = usable ? 1 : 0;
  return usable;
}

bool pb_plane_area_egl(PbPlaneArea* self) {
#ifdef GDK_WINDOWING_WAYLAND
  return GDK_IS_WAYLAND_DISPLAY(gtk_widget_get_display(GTK_WIDGET(self)));
#else
  return false;
#endif
}

bool pb_plane_area_bound(PbPlaneArea* self) { return StateOf(self)->source.texture != nullptr; }

void pb_plane_area_bind(PbPlaneArea* self, const pb_video::PlaneSource& source) {
  PlaneState* state = StateOf(self);
  if (state->source.texture) pb_plane_area_unbind(self);
  state->source = source;
  g_object_ref(state->source.texture);
  state->failure_logged = false;
}

void pb_plane_area_unbind(PbPlaneArea* self) {
  PlaneState* state = StateOf(self);
  Show(self, false);
  StopStallTimer(state);
  FreeConverter(self);
  gpointer texture = state->source.texture;
  state->source = pb_video::PlaneSource{};
  state->placement = pb_video::Placement{};
  state->grey = false;
  state->enhance = pb_video::Enhance::kOff;
  state->pending_since = 0;
  state->timing = false;
  ResetTimings(state);
  // The drawing's GPU times too: the next texture's start from nothing.
  state->draw.Time(false);
  if (texture) g_object_unref(texture);
}

void pb_plane_area_ready(PbPlaneArea* self) {
  PlaneState* state = StateOf(self);
  if (!state->source.texture) return;
  if (state->pending_since == 0) state->pending_since = g_get_monotonic_time();
  if (state->shown) QueuePicture(self);
  if (state->stall_timer == 0) {
    state->stall_timer = g_timeout_add(static_cast<guint>(kStallUs / G_TIME_SPAN_MILLISECOND), StallDue, self);
  }
}

void pb_plane_area_wake(PbPlaneArea* self) { TakeNow(self, true); }

void pb_plane_area_place(PbPlaneArea* self, const pb_video::Placement& placement) {
  PlaneState* state = StateOf(self);
  if (!state->source.texture) return;
  state->placement = placement;
  Show(self, placement.visible);
  if (placement.visible) gtk_widget_queue_draw(GTK_WIDGET(self));
}

void pb_plane_area_grey(PbPlaneArea* self, bool on) {
  PlaneState* state = StateOf(self);
  if (!state->source.texture || state->grey == on) return;
  state->grey = on;
  if (state->shown) QueuePicture(self);
}

void pb_plane_area_enhance(PbPlaneArea* self, pb_video::Enhance level) {
  PlaneState* state = StateOf(self);
  if (!state->source.texture || state->enhance == level) return;
  state->enhance = level;
  // Drawn again at once, though the desktop may be still.
  if (state->shown) QueuePicture(self);
}

pb_video::PlaneStats pb_plane_area_stats(PbPlaneArea* self, bool timings) {
  PlaneState* state = StateOf(self);
  pb_video::PlaneStats stats;
  if (timings != state->timing) ResetTimings(state);
  state->timing = timings;
  state->draw.Time(timings);
  stats.filter = pb_video::PlaneFilterName(state->draw.last_filter());
  GdkFrameClock* clock = gtk_widget_get_realized(GTK_WIDGET(self)) ? gtk_widget_get_frame_clock(GTK_WIDGET(self))
                                                                     : nullptr;
  if (clock) stats.refresh_interval_ms = RefreshIntervalMs(clock);
  if (!timings) return stats;
  stats.gpu = state->draw.GpuTimes();
  ResolveTimings(self);
  std::vector<double> sorted(state->intervals.begin(), state->intervals.end());
  stats.intervals = sorted.size();
  if (sorted.empty()) return stats;
  std::sort(sorted.begin(), sorted.end());
  stats.interval_p50_ms = sorted[sorted.size() / 2];
  stats.interval_p95_ms = sorted[std::min(sorted.size() - 1, sorted.size() * 95 / 100)];
  return stats;
}
