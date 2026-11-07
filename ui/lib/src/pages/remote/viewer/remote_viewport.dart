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

/// Zoom and pan of the remote picture: local only, never sent to the host,
/// and independent of the encoder (P1-4).
///
/// The picture is laid out at [scene], the fit of [layoutSize], and
/// transformed by [scale] and then [offset]: a normalised point `n` sits at
/// `offset + n · scene · scale`. The soft keyboard and the modifier bar
/// only occlude ([occlusion]); they never change [layoutSize] or [scene],
/// so nothing relayouts and the texture is untouched while they move.
final class RemoteViewport extends ChangeNotifier {
  Size _layoutSize = Size.zero;
  double _aspectRatio = 16 / 9;
  EdgeInsets _occlusion = EdgeInsets.zero;
  double _scale = kMinScale;
  Offset _offset = Offset.zero;
  ViewerScaleMode _mode = ViewerScaleMode.fit;
  Size _remotePixels = Size.zero;
  double _dpr = 1;

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

  /// What covers the bottom of the stage: the IME and the modifier bar on
  /// a phone; zero on desktop.
  EdgeInsets get occlusion => _occlusion;

  /// The video's width over its height.
  double get aspectRatio => _aspectRatio;
  set aspectRatio(double value) {
    if (!value.isFinite || value <= 0 || value == _aspectRatio) return;
    _relayout(() => _aspectRatio = value);
  }

  /// The picture fitted into [layoutSize], before [scale].
  Size get scene {
    if (_layoutSize.isEmpty) return Size.zero;
    final width = math.min(
      _layoutSize.width,
      _layoutSize.height * _aspectRatio,
    );
    return Size(width, width / _aspectRatio);
  }

  double get scale => _scale;
  Offset get offset => _offset;

  /// The stage minus what occludes it.
  Rect get visibleRect {
    final r = _occlusion.deflateRect(Offset.zero & _layoutSize);
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
  /// the visible rect.
  void layout(Size size) {
    if (size == _layoutSize) return;
    _relayout(() => _layoutSize = size);
  }

  /// Re-bounds the offset only: the scene and the scale stay.
  void setOcclusion(EdgeInsets occlusion) {
    if (occlusion == _occlusion) return;
    _occlusion = occlusion;
    _set(_scale, _offset, force: true);
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
    _set(_scale, _offset + delta);
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
      _set(_scale, _offset, force: true);
    } else {
      _set(before.$1, before.$2, force: true);
    }
  }

  /// Fit, or one remote pixel per physical pixel (follows the pointer
  /// through [autoScrollTo]).
  void setMode(
    ViewerScaleMode mode, {
    required Size remotePixels,
    required double dpr,
  }) {
    _relayout(() {
      _mode = mode;
      _remotePixels = remotePixels;
      _dpr = dpr > 0 ? dpr : 1;
    }, rescale: true);
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
    _visibleRegion.dispose();
    super.dispose();
  }

  // ---------------------------------------------------------------- inner

  /// One remote pixel per physical pixel in original-size mode.
  double get _modeScale {
    if (_mode != ViewerScaleMode.original) return kMinScale;
    final width = scene.width * _dpr;
    if (width <= 0 || _remotePixels.isEmpty) return kMinScale;
    return (_remotePixels.width / width).clamp(kMinScale, kMaxScale);
  }

  /// Applies a change to the layout, the aspect or the mode while keeping
  /// the picture point at the centre of the visible rect. A zoom of the
  /// user's stays in fit mode; original size follows the new scene.
  void _relayout(VoidCallback change, {bool rescale = false}) {
    final before = scene;
    final centre = before.isEmpty
        ? const Offset(0.5, 0.5)
        : toNormalized(visibleRect.center, clamp: true)!;
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

  /// [force]: something besides the transform changed (the layout, the
  /// occlusion), so listeners hear of it even if the transform did not.
  void _set(double scale, Offset offset, {bool force = false}) {
    final bounded = _bound(offset, scale);
    if (!force && scale == _scale && bounded == _offset) return;
    _scale = scale;
    _offset = bounded;
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
