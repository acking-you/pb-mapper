import 'dart:async';

import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

import '../../../ffi/desktop_video.dart';

/// Where a runner draws the picture on its own plane under the Flutter view
/// (the Linux video plane, [DesktopVideoTexture.plane]): a transparent hole
/// in the picture's place, and that place, reported to the plane.
///
/// It paints only the hole: its rectangle with [BlendMode.clear], which
/// clears the viewer's ground under it down to the window, so the plane
/// shows through; the cursors and the overlays paint over it as over the
/// texture it stands in for. It is a layer of its own that is never
/// raster-cached, since a cached picture would clear only its own pixels.
///
/// Each time Flutter composites a frame, it works out from the layers above
/// it where the hole lands, in the view's physical pixels and cut to their
/// clips, and when that changed tells [onPlace] once the frame is done:
/// [VideoPlacement.hidden] when the hole was not composited (it left the
/// tree, or a route covers it) or lies under an opacity, a filter or a save
/// layer, through which no hole reaches the window, as a route transition
/// draws it, or under a rotation, which the plane cannot follow.
class NativeVideoHole extends LeafRenderObjectWidget {
  const NativeVideoHole({super.key, required this.onPlace});

  final ValueChanged<VideoPlacement> onPlace;

  @override
  RenderNativeVideoHole createRenderObject(BuildContext context) =>
      RenderNativeVideoHole(onPlace: onPlace);

  @override
  void updateRenderObject(
    BuildContext context,
    RenderNativeVideoHole renderObject,
  ) => renderObject.onPlace = onPlace;
}

/// The render object of a [NativeVideoHole].
class RenderNativeVideoHole extends RenderBox {
  RenderNativeVideoHole({required ValueChanged<VideoPlacement> onPlace})
    : _onPlace = onPlace;

  ValueChanged<VideoPlacement> _onPlace;
  set onPlace(ValueChanged<VideoPlacement> value) => _onPlace = value;

  /// What [_onPlace] heard last; null before the first.
  VideoPlacement? _sent;

  /// Where the last composite put the hole.
  VideoPlacement _measured = VideoPlacement.hidden;
  bool _sendDue = false;

  /// The layer whose compositing is observed, and how to stop.
  ContainerLayer? _observed;
  VoidCallback? _stopObserving;

  @override
  bool get isRepaintBoundary => true;

  @override
  bool get sizedByParent => true;

  @override
  Size computeDryLayout(covariant BoxConstraints constraints) =>
      constraints.biggest;

  /// Opaque to hits, as the texture was.
  @override
  bool hitTestSelf(Offset position) => true;

  @override
  void paint(PaintingContext context, Offset offset) {
    context.setWillChangeHint();
    context.canvas.drawRect(
      offset & size,
      Paint()..blendMode = BlendMode.clear,
    );
    _observe(layer);
  }

  void _observe(ContainerLayer? target) {
    if (identical(target, _observed)) return;
    _stopObserving?.call();
    _observed = target;
    _stopObserving = target?.addCompositionCallback(_composited);
  }

  void _stop() {
    _stopObserving?.call();
    _stopObserving = null;
    _observed = null;
  }

  void _composited(Layer layer) {
    _measured = holePlacement(layer, size);
    _schedule();
  }

  /// After the frame: the placement goes out once the scene is built, and
  /// never from inside the compositing that measured it.
  void _schedule() {
    if (_sendDue || _measured == _sent) return;
    _sendDue = true;
    scheduleMicrotask(() {
      _sendDue = false;
      if (_measured == _sent) return;
      _sent = _measured;
      _onPlace(_measured);
    });
  }

  @override
  void detach() {
    _stop();
    _measured = VideoPlacement.hidden;
    _schedule();
    super.detach();
  }

  @override
  void dispose() {
    _stop();
    super.dispose();
  }
}

/// Where the content of [layer], [size] logical pixels from its origin,
/// lands in the view as the frame just composited puts it: in physical
/// pixels (the view's root layer scales by the device pixel ratio), cut to
/// every clip above it, each snapped to whole pixels. Hidden when [layer] is
/// not in the composited tree, or under a layer through which a cleared hole
/// does not reach the window or whose transform does not only scale and
/// move.
@visibleForTesting
VideoPlacement holePlacement(Layer layer, Size size) {
  if (!layer.attached || layer is! ContainerLayer) return VideoPlacement.hidden;
  final content = layer.firstChild;
  if (content == null) return VideoPlacement.hidden;
  final chain = <ContainerLayer>[];
  for (ContainerLayer? at = layer; at != null; at = at.parent) {
    chain.add(at);
  }
  final transform = Matrix4.identity();
  Rect? clip;
  for (var i = chain.length - 1; i >= 0; i--) {
    final at = chain[i];
    if (!_holeReachesThrough(at)) return VideoPlacement.hidden;
    final bounds = at.describeClipBounds();
    if (bounds != null) {
      final placed = MatrixUtils.transformRect(transform, bounds);
      clip = clip == null ? placed : clip.intersect(placed);
    }
    at.applyTransform(i > 0 ? chain[i - 1] : content, transform);
  }
  if (!_scalesAndMoves(transform)) return VideoPlacement.hidden;
  final picture = _snap(
    MatrixUtils.transformRect(transform, Offset.zero & size),
  );
  final shown = clip == null ? picture : _snap(picture.intersect(clip));
  if (picture.width <= 0 ||
      picture.height <= 0 ||
      shown.width <= 0 ||
      shown.height <= 0) {
    return VideoPlacement.hidden;
  }
  return VideoPlacement(visible: true, picture: picture, clip: shown);
}

/// Whether what a layer composites keeps a cleared hole in the window's
/// pixels: not inside an opacity, a filter, a shader mask or a save layer,
/// which composite their children as one picture over what lies below.
bool _holeReachesThrough(ContainerLayer layer) => switch (layer) {
  OpacityLayer(:final alpha) => alpha == null || alpha == 255,
  ColorFilterLayer() ||
  ImageFilterLayer() ||
  ShaderMaskLayer() ||
  BackdropFilterLayer() => false,
  ClipRectLayer(:final clipBehavior) ||
  ClipRRectLayer(:final clipBehavior) ||
  ClipPathLayer(
    :final clipBehavior,
  ) => clipBehavior != Clip.antiAliasWithSaveLayer,
  _ => true,
};

/// Whether [transform] only scales, by a positive factor, and moves in the
/// view's plane, which the plane can follow.
bool _scalesAndMoves(Matrix4 transform) {
  final m = transform.storage;
  const epsilon = 1e-9;
  return m[1].abs() < epsilon &&
      m[4].abs() < epsilon &&
      m[3].abs() < epsilon &&
      m[7].abs() < epsilon &&
      (m[15] - 1).abs() < epsilon &&
      m[0] > 0 &&
      m[5] > 0;
}

Rect _snap(Rect rect) => Rect.fromLTRB(
  rect.left.roundToDouble(),
  rect.top.roundToDouble(),
  rect.right.roundToDouble(),
  rect.bottom.roundToDouble(),
);
