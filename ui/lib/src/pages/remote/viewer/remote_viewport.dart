import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import '../../../state/viewer_prefs.dart';

/// Fit to the window.
const kMinScale = 1.0;
const kMaxScale = 8.0;

/// How close to the visible edge the followed cursor may come, as a share
/// of the visible rect.
const kFollowMargin = 0.15;

/// A fit that would enlarge the picture by less than this, in device
/// pixels, shows it at one device pixel per video pixel instead, a little
/// smaller than its room: bilinear sampling at a factor this close to 1
/// blends every pixel with its neighbours and blurs text.
const kExactFitLimit = 1.04;

/// Zoom and pan of the remote picture: local only, never sent to the host,
/// and independent of the encoder (P1-4).
///
/// The picture is laid out at [scene], the fit of [layoutSize] less its
/// [frame], and transformed by [scale] and then [offset]: a normalised
/// point `n` sits at `offset + n · scene · scale`. The soft keyboard and
/// the modifier bar only occlude ([occlusion]); they never change
/// [layoutSize] or [scene], so nothing relayouts and the texture is
/// untouched while they move.
///
/// While the picture shows one device pixel per video or remote pixel,
/// [offset] puts its edge on a device pixel's, counted from the stage's
/// place in the view ([setOrigin]): half a pixel off, sampling would
/// spread every 1-pixel stroke over two pixels at half contrast.
final class RemoteViewport extends ChangeNotifier {
  Size _layoutSize = Size.zero;
  double _aspectRatio = 16 / 9;
  EdgeInsets _frame = EdgeInsets.zero;
  EdgeInsets _occlusion = EdgeInsets.zero;
  double _scale = kMinScale;
  Offset _offset = Offset.zero;
  ViewerScaleMode _mode = ViewerScaleMode.fit;

  /// The remote display's size: original size shows one of its pixels per
  /// physical pixel.
  Size _display = Size.zero;

  /// The video's size: the picture's own pixels.
  Size _video = Size.zero;
  double _dpr = 1;

  /// The stage's top left in the view, in logical pixels; null while it is
  /// unknown, or while the stage is scaled or rotated there.
  Offset? _origin;

  /// [offset] before it snaps to device pixels: a pan goes on from here,
  /// so moves smaller than a device pixel add up.
  Offset _free = Offset.zero;

  /// The navigation saved when the soft keyboard opened.
  (double, Offset, EdgeInsets)? _beforeKeyboard;
  bool _navigated = false;

  /// The pinch in progress: its start list, start scale and the scene
  /// point under the start midpoint.
  (List<Offset>, double, Offset)? _pinch;

  final _visibleRegion = ValueNotifier<Rect?>(null);
  bool _notifyQueued = false;

  /// From the surface's layout; the IME never changes it.
  Size get layoutSize => _layoutSize;

  /// What stands around the picture for good: a phone's gutters, which it
  /// fits and pans between; zero on desktop.
  EdgeInsets get frame => _frame;

  /// What covers the bottom of the stage: the IME and the modifier bar on
  /// a phone; zero on desktop.
  EdgeInsets get occlusion => _occlusion;

  /// The video's width over its height.
  double get aspectRatio => _aspectRatio;
  set aspectRatio(double value) {
    if (!value.isFinite || value <= 0 || value == _aspectRatio) return;
    _relayout(() => _aspectRatio = value);
  }

  /// The picture fitted into [layoutSize] less [frame], before [scale];
  /// at one device pixel per video pixel where the fit would enlarge it by
  /// less than [kExactFitLimit].
  Size get scene {
    final room = _frameRect.size;
    if (room.isEmpty) return Size.zero;
    final exact = _exactScene(room);
    if (exact != null) return exact;
    final width = math.min(room.width, room.height * _aspectRatio);
    return Size(width, width / _aspectRatio);
  }

  /// The picture as fitted, unzoomed: [scene] centred in the frame.
  Rect get fittedRect {
    final room = _frameRect;
    final s = scene;
    return Rect.fromLTWH(
      room.left + (room.width - s.width) / 2,
      room.top + (room.height - s.height) / 2,
      s.width,
      s.height,
    );
  }

  double get scale => _scale;
  Offset get offset => _offset;

  /// The stage minus its frame and what occludes it, side by side: the
  /// keyboard rises over the bottom gutter rather than adding to it.
  Rect get visibleRect {
    final size = _layoutSize;
    final f = _frame, o = _occlusion;
    final r = Rect.fromLTRB(
      math.max(f.left, o.left),
      math.max(f.top, o.top),
      size.width - math.max(f.right, o.right),
      size.height - math.max(f.bottom, o.bottom),
    );
    return Rect.fromLTRB(
      r.left,
      r.top,
      math.max(r.left, r.right),
      math.max(r.top, r.bottom),
    );
  }

  /// The visible part of the picture, normalised; null while all of it is
  /// visible.
  ValueListenable<Rect?> get visibleRegion => _visibleRegion;

  ViewerScaleMode get mode => _mode;

  /// The picture point under [local], or null outside the picture unless
  /// [clamp] pulls it to the nearest edge.
  Offset? toNormalized(Offset local, {bool clamp = false}) {
    final s = scene;
    if (s.isEmpty) return null;
    final n = Offset(
      (local.dx - _offset.dx) / (s.width * _scale),
      (local.dy - _offset.dy) / (s.height * _scale),
    );
    if (!clamp && (n.dx < 0 || n.dy < 0 || n.dx > 1 || n.dy > 1)) return null;
    return Offset(n.dx.clamp(0.0, 1.0), n.dy.clamp(0.0, 1.0));
  }

  Offset toLocal(Offset normalized) {
    final s = scene;
    return _offset +
        Offset(
          normalized.dx * s.width * _scale,
          normalized.dy * s.height * _scale,
        );
  }

  /// A resize or rotation keeps the scale and the point at the centre of
  /// the visible rect. A [frame] (a phone's gutters, which follow the size)
  /// changes with the size in the same step: either one alone, applied
  /// against the other's old value, can leave no room for the picture and
  /// so lose the zoom. Null keeps the frame.
  void layout(Size size, {EdgeInsets? frame}) {
    if (size == _layoutSize && (frame == null || frame == _frame)) return;
    _relayout(() {
      _layoutSize = size;
      if (frame != null) _frame = frame;
    });
  }

  /// Re-bounds the offset only: the scene and the scale stay.
  void setOcclusion(EdgeInsets occlusion) {
    if (occlusion == _occlusion) return;
    _occlusion = occlusion;
    _set(_scale, _free, force: true);
  }

  void zoomAt(Offset focal, double factor) {
    if (!factor.isFinite || factor <= 0) return;
    final next = (_scale * factor).clamp(kMinScale, kMaxScale);
    final anchor = (focal - _offset) / _scale;
    _navigated = true;
    _set(next, focal - anchor * next);
  }

  void panBy(Offset delta) {
    _navigated = true;
    _set(_scale, _free + delta);
  }

  /// Two or more fingers: scale by their spread, pan by their midpoint. A
  /// new [start] list begins a new pinch; with one finger it only pans.
  void pinchPan(List<Offset> start, List<Offset> now) {
    if (start.isEmpty || now.length != start.length) return;
    var pinch = _pinch;
    if (pinch == null || !identical(pinch.$1, start)) {
      pinch = (start, _scale, (_centroid(start) - _offset) / _scale);
      _pinch = pinch;
    }
    final (_, s0, anchor) = pinch;
    final d0 = _spread(start), d = _spread(now);
    final next = start.length < 2 || d0 <= 0
        ? _scale
        : (s0 * d / d0).clamp(kMinScale, kMaxScale);
    _navigated = true;
    _set(next, _centroid(now) - anchor * next);
  }

  /// The pinch's fingers lifted: the picture may settle on device pixels,
  /// which it never does under the fingers.
  void endPinch() {
    if (_pinch == null) return;
    _pinch = null;
    _set(_scale, _free);
  }

  /// The smallest pan that brings [normalized] at least [margin] of the
  /// visible rect inside its edges. Not a navigation of the user's.
  void ensureVisible(Offset normalized, {double margin = kFollowMargin}) {
    final p = toLocal(normalized);
    final v = visibleRect;
    double axis(double p, double lo, double hi, double margin) {
      final inLo = lo + margin, inHi = hi - margin;
      if (inLo > inHi) return (lo + hi) / 2 - p;
      if (p < inLo) return inLo - p;
      if (p > inHi) return inHi - p;
      return 0;
    }

    final dx = axis(p.dx, v.left, v.right, v.width * margin);
    final dy = axis(p.dy, v.top, v.bottom, v.height * margin);
    if (dx == 0 && dy == 0) return;
    _set(_scale, _offset + Offset(dx, dy));
  }

  /// The soft keyboard opens: the navigation is remembered for
  /// [endKeyboard].
  void beginKeyboard() {
    _beforeKeyboard ??= (_scale, _offset, _occlusion);
    _navigated = false;
  }

  /// The soft keyboard closed: the navigation from before it opened comes
  /// back exactly, unless the user zoomed or panned meanwhile.
  void endKeyboard() {
    final before = _beforeKeyboard;
    if (before == null) return;
    _beforeKeyboard = null;
    _occlusion = before.$3;
    if (_navigated) {
      _set(_scale, _free, force: true);
    } else {
      _set(before.$1, before.$2, force: true);
    }
  }

  /// Fit, or one remote pixel per physical pixel (follows the pointer
  /// through [autoScrollTo]).
  void setMode(ViewerScaleMode mode) =>
      _relayout(() => _mode = mode, rescale: true);

  /// The remote display's size, the video's and this screen's device pixel
  /// ratio, which original size and an exact fit are measured in. A zoom
  /// of the user's stays in fit mode, as through any relayout.
  void setPixels({
    required Size display,
    required Size video,
    required double dpr,
  }) {
    final ratio = dpr.isFinite && dpr > 0 ? dpr : 1.0;
    if (display == _display && video == _video && ratio == _dpr) return;
    _relayout(() {
      _display = display;
      _video = video;
      _dpr = ratio;
    });
  }

  /// The stage's top left in the view, in logical pixels: the device
  /// pixels the picture snaps to are counted from the view's own. Null
  /// while the stage is scaled or rotated there: nothing snaps.
  void setOrigin(Offset? origin) {
    final was = _origin;
    if (origin == was) return;
    // The same place, computed along another path.
    if (origin != null &&
        was != null &&
        (origin - was).distanceSquared < 1e-12) {
      return;
    }
    _origin = origin;
    _set(_scale, _free);
  }

  /// In original-size mode the view follows the pointer: its position in
  /// the visible rect is the position of the view over the picture.
  void autoScrollTo(Offset pointer) {
    if (_mode != ViewerScaleMode.original) return;
    final s = scene * _scale;
    final v = visibleRect;
    double axis(double pointer, double lo, double hi, double extent) {
      final span = hi - lo;
      if (extent <= span || span <= 0) return 0;
      return lo - ((pointer - lo) / span).clamp(0.0, 1.0) * (extent - span);
    }

    _set(
      _scale,
      Offset(
        axis(pointer.dx, v.left, v.right, s.width),
        axis(pointer.dy, v.top, v.bottom, s.height),
      ),
    );
  }

  @override
  void dispose() {
    // A viewer that closes under a pinch ends it while the frame tears the
    // tree down: the change it queued must not reach this after the frame.
    _notifyQueued = false;
    _visibleRegion.dispose();
    super.dispose();
  }

  // ---------------------------------------------------------------- inner

  Rect get _frameRect {
    final r = _frame.deflateRect(Offset.zero & _layoutSize);
    return Rect.fromLTRB(
      r.left,
      r.top,
      math.max(r.left, r.right),
      math.max(r.top, r.bottom),
    );
  }

  /// One remote pixel per physical pixel in original-size mode.
  double get _modeScale {
    if (_mode != ViewerScaleMode.original) return kMinScale;
    final width = scene.width * _dpr;
    if (width <= 0 || _display.isEmpty) return kMinScale;
    return (_display.width / width).clamp(kMinScale, kMaxScale);
  }

  /// The picture at one device pixel per video pixel, when the fit of
  /// [room] would enlarge it by at least 1 and less than [kExactFitLimit];
  /// null otherwise, or while the video's size has not the picture's
  /// aspect (it is not yet this picture's).
  Size? _exactScene(Size room) {
    final video = _video;
    if (video.isEmpty) return null;
    final fit = math.min(
      room.width * _dpr / video.width,
      room.height * _dpr / video.height,
    );
    if (fit < 1 || fit >= kExactFitLimit) return null;
    final width = video.width / _dpr;
    final height = width / _aspectRatio;
    if ((height * _dpr - video.height).abs() >= 0.5) return null;
    return Size(width, height);
  }

  /// One device pixel per video pixel, or per remote pixel, within half a
  /// device pixel across the picture.
  bool _pixelExact(double scale) {
    final width = scene.width * scale * _dpr;
    bool exact(Size pixels) =>
        !pixels.isEmpty && (width - pixels.width).abs() < 0.5;
    return exact(_video) || exact(_display);
  }

  /// Applies a change to the layout, the aspect or the mode while keeping
  /// the picture point at the centre of the visible rect. A zoom of the
  /// user's stays in fit mode; original size follows the new scene. The
  /// point is taken where the picture was before it snapped: taken where
  /// it shows, each relayout of a live resize would add its snap's
  /// rounding and the picture would slide.
  void _relayout(VoidCallback change, {bool rescale = false}) {
    final before = scene;
    final centre = before.isEmpty
        ? const Offset(0.5, 0.5)
        : _beforeSnap(visibleRect.center);
    change();
    _pinch = null;
    final next = rescale || before.isEmpty || _mode == ViewerScaleMode.original
        ? _modeScale
        : _scale;
    final s = scene;
    _set(
      next,
      visibleRect.center -
          Offset(centre.dx * s.width, centre.dy * s.height) * next,
      force: true,
    );
  }

  /// The picture point under [local], clamped to the picture, with the
  /// picture at [_free]; the scene must not be empty.
  Offset _beforeSnap(Offset local) {
    final s = scene * _scale;
    return Offset(
      ((local.dx - _free.dx) / s.width).clamp(0.0, 1.0),
      ((local.dy - _free.dy) / s.height).clamp(0.0, 1.0),
    );
  }

  /// Per axis, a picture smaller than the visible span is centred in it;
  /// a larger one never leaves a gap at the edges.
  Offset _bound(Offset offset, double scale) {
    final s = scene * scale;
    final v = visibleRect;
    double axis(double offset, double lo, double hi, double extent) {
      if (extent <= hi - lo) return lo + (hi - lo - extent) / 2;
      return offset.clamp(hi - extent, lo);
    }

    return Offset(
      axis(offset.dx, v.left, v.right, s.width),
      axis(offset.dy, v.top, v.bottom, s.height),
    );
  }

  /// Moves a pixel-exact picture by less than a device pixel, so its edge
  /// falls on one and each device pixel samples one texel whole. Under a
  /// pinch's fingers, or with the stage's place unknown, nothing moves. A
  /// picture larger than the visible span stays within its bounds where a
  /// device pixel's edge lies within them, so no sliver of ground shows.
  Offset _snap(Offset offset, double scale) {
    final origin = _origin;
    if (origin == null || _pinch != null || !_pixelExact(scale)) {
      return offset;
    }
    final s = scene * scale;
    final v = visibleRect;
    final dpr = _dpr;
    double axis(double at, double origin, double lo, double hi, double extent) {
      double toOffset(double device) => device / dpr - origin;
      final snapped = toOffset(((origin + at) * dpr).roundToDouble());
      if (extent <= hi - lo) return snapped;
      final min = hi - extent, max = lo;
      if (snapped > max) {
        final inside = toOffset(((origin + max) * dpr + 1e-9).floorToDouble());
        return inside >= min ? inside : snapped;
      }
      if (snapped < min) {
        final inside = toOffset(((origin + min) * dpr - 1e-9).ceilToDouble());
        return inside <= max ? inside : snapped;
      }
      return snapped;
    }

    return Offset(
      axis(offset.dx, origin.dx, v.left, v.right, s.width),
      axis(offset.dy, origin.dy, v.top, v.bottom, s.height),
    );
  }

  /// [force]: something besides the transform changed (the layout, the
  /// occlusion), so listeners hear of it even if the transform did not.
  void _set(double scale, Offset offset, {bool force = false}) {
    final bounded = _bound(offset, scale);
    final shown = _snap(bounded, scale);
    _free = bounded;
    if (!force && scale == _scale && shown == _offset) return;
    _scale = scale;
    _offset = shown;
    _changed();
  }

  /// Listeners elsewhere in the tree must not be marked dirty from inside
  /// a build or layout (the surface lays the viewport out in its
  /// `LayoutBuilder`), so a change made then is published after the frame.
  void _changed() {
    if (SchedulerBinding.instance.schedulerPhase !=
        SchedulerPhase.persistentCallbacks) {
      _notifyQueued = false;
      _publishRegion();
      notifyListeners();
      return;
    }
    if (_notifyQueued) return;
    _notifyQueued = true;
    SchedulerBinding.instance.addPostFrameCallback((_) {
      if (!_notifyQueued) return;
      _notifyQueued = false;
      _publishRegion();
      notifyListeners();
    });
  }

  void _publishRegion() {
    final s = scene * _scale;
    if (s.isEmpty) {
      _visibleRegion.value = null;
      return;
    }
    final v = visibleRect;
    final region = Rect.fromLTRB(
      ((v.left - _offset.dx) / s.width).clamp(0.0, 1.0),
      ((v.top - _offset.dy) / s.height).clamp(0.0, 1.0),
      ((v.right - _offset.dx) / s.width).clamp(0.0, 1.0),
      ((v.bottom - _offset.dy) / s.height).clamp(0.0, 1.0),
    );
    const whole = 0.005;
    final all =
        region.left <= whole &&
        region.top <= whole &&
        region.right >= 1 - whole &&
        region.bottom >= 1 - whole;
    _visibleRegion.value = all ? null : region;
  }

  static Offset _centroid(List<Offset> points) {
    var sum = Offset.zero;
    for (final p in points) {
      sum += p;
    }
    return sum / points.length.toDouble();
  }

  static double _spread(List<Offset> points) {
    if (points.length < 2) return 0;
    final c = _centroid(points);
    var sum = 0.0;
    for (final p in points) {
      sum += (p - c).distance;
    }
    return sum / points.length;
  }
}
