import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../../../ffi/desktop_pointer.dart';
import '../../../state/desktop_store.dart';
import 'remote_input.dart';
import 'remote_viewport.dart';

/// What is painted at the host's pointer.
enum RemotePointerPaint {
  /// Nothing: the local pointer is there, in the host's shape.
  none,

  /// The generic arrow, as for a host that sends no shape.
  arrow,

  /// The host's shape, its bitmap.
  shape,
}

/// How the host's pointer shows (D11, D12): the local OS pointer over the
/// picture, and what the cursor layer paints where the host's pointer is.
///
/// A host that sends no shape (before 0.12) keeps the generic arrow, painted,
/// and the local pointer as it is. A touch viewer paints the host's shape.
/// A desktop viewer in control gives the local pointer the host's shape
/// while the host's pointer is under it, so one pointer shows, not two: a
/// standard shape this computer's OS has as the system's own pointer, any
/// other as the host's bitmap made this computer's pointer
/// ([nativeBitmap]), or, where it cannot be, painted under a hidden
/// pointer. Viewing only, or once the host's pointer is elsewhere (someone
/// there moved it, or the local pointer left the picture), the host's shape
/// is painted where the host's pointer is.
@immutable
class RemotePointerLook {
  const RemotePointerLook(this.local, this.paint);

  /// What a host that sends no shape gets: today's arrow.
  static const generic = RemotePointerLook(
    MouseCursor.defer,
    RemotePointerPaint.arrow,
  );

  /// [mouse]: a desktop viewer, whose local pointer hovers the picture.
  /// [atLocalPointer]: the host's pointer is where the local one is.
  /// [kind]: the host's shape, null from a host that sends none.
  /// [nativeBitmap]: the host's bitmap can be this computer's own pointer
  /// (`pb_mapper/desktop_pointer`). [platform]: the viewer's OS, by default
  /// this one. [relative]: moves go as relative moves (`RemotePointerLock`),
  /// so the local pointer has no place on the picture: it is hidden, and
  /// the host's pointer painted where the host reports it.
  factory RemotePointerLook.of({
    required bool mouse,
    required bool controlling,
    required bool atLocalPointer,
    required DesktopCursorKind? kind,
    bool nativeBitmap = false,
    TargetPlatform? platform,
    bool relative = false,
  }) {
    if (relative) {
      return RemotePointerLook(
        SystemMouseCursors.none,
        kind == null ? RemotePointerPaint.arrow : RemotePointerPaint.shape,
      );
    }
    if (kind == null) return generic;
    if (!mouse || !controlling) {
      return const RemotePointerLook(
        MouseCursor.defer,
        RemotePointerPaint.shape,
      );
    }
    final system = systemCursorOf(kind, platform);
    if (!atLocalPointer) {
      return RemotePointerLook(
        system ?? SystemMouseCursors.basic,
        RemotePointerPaint.shape,
      );
    }
    if (system != null) {
      return RemotePointerLook(system, RemotePointerPaint.none);
    }
    return nativeBitmap
        ? const RemotePointerLook(null, RemotePointerPaint.none)
        : const RemotePointerLook(
            SystemMouseCursors.none,
            RemotePointerPaint.shape,
          );
  }

  /// The local pointer over the picture; null for the host's bitmap as
  /// this computer's own pointer ([RemoteShapeCursor]).
  final MouseCursor? local;
  final RemotePointerPaint paint;

  @override
  bool operator ==(Object other) =>
      other is RemotePointerLook &&
      other.local == local &&
      other.paint == paint;

  @override
  int get hashCode => Object.hash(local, paint);
}

/// The system pointer of [kind] on [platform] (by default this computer's
/// OS); null where that OS has none, so that only its bitmap shows it.
///
/// Flutter's macOS embedder knows 21 of its cursor kinds and shows any
/// other as the arrow (FlutterMouseCursorPlugin.mm), so a Mac has no busy,
/// progress, help, move or diagonal resize pointer here.
MouseCursor? systemCursorOf(
  DesktopCursorKind kind, [
  TargetPlatform? platform,
]) => switch (kind) {
  DesktopCursorKind.arrow => SystemMouseCursors.basic,
  DesktopCursorKind.ibeam => SystemMouseCursors.text,
  DesktopCursorKind.hand => SystemMouseCursors.click,
  DesktopCursorKind.crosshair => SystemMouseCursors.precise,
  DesktopCursorKind.notAllowed => SystemMouseCursors.forbidden,
  DesktopCursorKind.resizeNs => SystemMouseCursors.resizeUpDown,
  DesktopCursorKind.resizeEw => SystemMouseCursors.resizeLeftRight,
  DesktopCursorKind.wait ||
  DesktopCursorKind.progress ||
  DesktopCursorKind.help ||
  DesktopCursorKind.move ||
  DesktopCursorKind.resizeNwse ||
  DesktopCursorKind.resizeNesw
      when (platform ?? defaultTargetPlatform) == TargetPlatform.macOS =>
    null,
  DesktopCursorKind.wait => SystemMouseCursors.wait,
  DesktopCursorKind.progress => SystemMouseCursors.progress,
  DesktopCursorKind.help => SystemMouseCursors.help,
  DesktopCursorKind.move => SystemMouseCursors.move,
  DesktopCursorKind.resizeNwse => SystemMouseCursors.resizeUpLeftDownRight,
  DesktopCursorKind.resizeNesw => SystemMouseCursors.resizeUpRightDownLeft,
  DesktopCursorKind.upArrow || DesktopCursorKind.custom => null,
};

/// The smallest and largest a native pointer bitmap shows a host pixel, in
/// logical pixels, whatever the picture's scale, and the longest side it
/// gets: legible on a small picture, never huge on a zoomed one (airhand's
/// bounds, client/src/lib/session.ts).
const kMinBitmapScale = 0.35;
const kMaxBitmapScale = 2.0;
const kMaxBitmapSide = 128.0;

/// Native pointer bitmaps one viewer keeps, as many as the store keeps
/// shapes: a pointer going back and forth between them makes each once.
const kNativeBitmapCache = 16;

/// The host's pointer as one viewer shows it: its [RemotePointerLook],
/// kept current as control, the host's shape and the local pointer's hover
/// change, and what the painter needs of the picture's scale.
///
/// While a desktop viewer controls, the local pointer over the picture is
/// [shapeCursor], whose session shows [value]'s [RemotePointerLook.local]
/// itself, the moment it or the bitmap's scale changes: a system pointer
/// through `SystemChannels.mouseCursor`, the host's bitmap through
/// [DesktopPointerCursors]. Nothing rebuilds and no frame is waited for. A
/// call to the runner that fails paints the host's shape from then on, as
/// a viewer without the runner does.
final class RemotePointer extends ValueNotifier<RemotePointerLook> {
  /// [native]: the runner's pointers; by default its channel where this
  /// OS's runner serves it ([nativePointerCursors]).
  RemotePointer({
    required this.mouse,
    required this.cursor,
    required this.controlling,
    required this.geometry,
    required this.viewport,
    this.relative,
    DesktopPointerCursors? native,
  }) : _native =
           native ?? (nativePointerCursors ? desktopPointerCursors : null),
       super(RemotePointerLook.generic) {
    cursor.addListener(_update);
    controlling.addListener(_update);
    relative?.addListener(_update);
    _update();
  }

  /// A desktop viewer: see [RemotePointerLook.of].
  final bool mouse;
  final DesktopCursor cursor;

  /// Control is live: the local pointer moves the host's.
  final ValueListenable<bool> controlling;

  /// Moves go as relative moves (`RemotePointerLock.relativeListenable`):
  /// the host's pointer is painted, the local one hidden.
  final ValueListenable<bool>? relative;
  final ValueListenable<DesktopGeometry?> geometry;
  final RemoteViewport viewport;

  /// The local pointer over the picture while this viewer controls. One
  /// instance, so the mouse tracker keeps its session while the pointer
  /// stays over the picture.
  late final MouseCursor shapeCursor = RemoteShapeCursor._(this);

  final DesktopPointerCursors? _native;
  bool _nativeFailed = false;
  bool _disposed = false;
  bool _over = false;
  double _dpr = 1;

  /// The session showing [shapeCursor], and what it showed last: a system
  /// kind or one of [_bitmaps].
  _RemoteShapeSession? _session;
  String? _shownKind;
  _NativeBitmap? _shownBitmap;

  /// The bitmaps the runner holds, the latest used last.
  final _bitmaps = <_NativeBitmap>[];

  /// Shared by every viewer, so the runner never sees an id twice in one
  /// run. A hot restart starts it again while the runner keeps its
  /// cursors; the runner replaces a cursor whose id comes again.
  static int _nextBitmap = 1;

  /// The ids of the bitmaps the runner holds for this pointer, the latest
  /// used last.
  @visibleForTesting
  List<int> get debugBitmapIds => [for (final bitmap in _bitmaps) bitmap.id];

  /// The local pointer hovers the picture. Leaving it, or the window, ends
  /// what [shapeCursor] showed: the mouse tracker drops a removed device's
  /// session without disposing it.
  set over(bool over) {
    if (over == _over) return;
    _over = over;
    if (!over) _detach(null);
    _update();
  }

  /// Physical pixels per logical one where the picture is.
  set devicePixelRatio(double dpr) {
    if (!(dpr > 0) || dpr == _dpr) return;
    _dpr = dpr;
    _apply();
  }

  void _update() {
    final shape = cursor.shape;
    value = RemotePointerLook.of(
      mouse: mouse,
      controlling: controlling.value,
      atLocalPointer: _over && cursor.followsLocal,
      kind: shape?.definition.kind,
      nativeBitmap: _native != null && !_nativeFailed,
      relative: mouse && controlling.value && (relative?.value ?? false),
    );
    _apply();
    // Another computer, or another viewer of it: its bitmaps are gone.
    if (shape == null && _bitmaps.isNotEmpty) _forgetBitmaps();
  }

  void _attach(_RemoteShapeSession session) {
    if (_disposed) return;
    if (_session == null) viewport.addListener(_apply);
    _session = session;
    // A new session: whatever this computer's pointer shows now is not
    // ours.
    _shownKind = null;
    _shownBitmap = null;
    _apply();
  }

  /// [session] ends, or with null whichever is attached.
  void _detach(_RemoteShapeSession? session) {
    final attached = _session;
    if (attached == null ||
        (session != null && !identical(session, attached))) {
      return;
    }
    _session = null;
    viewport.removeListener(_apply);
  }

  /// Shows [value]'s local pointer, unless it shows already. Runs on every
  /// hover, so what it does not change costs a few comparisons.
  void _apply() {
    final session = _session;
    if (session == null || _disposed) return;
    final local = value.local;
    if (local != null) {
      final kind = local is SystemMouseCursor
          ? local.kind
          : SystemMouseCursors.basic.kind;
      if (kind == _shownKind) return;
      _shownKind = kind;
      _shownBitmap = null;
      SystemChannels.mouseCursor.invokeMethod<void>('activateSystemCursor', {
        'device': session.device,
        'kind': kind,
      }).ignore();
      return;
    }
    final native = _native, shape = cursor.shape?.definition;
    if (native == null || shape == null) return;
    final scale = _bitmapScale(shape);
    final shown = _shownBitmap;
    if (shown != null && shown.fits(shape, scale, _dpr)) return;
    final bitmap = _bitmapFor(native, shape, scale);
    _shownBitmap = bitmap;
    _shownKind = null;
    native.set(bitmap.id).then<void>((_) {}, onError: _onNativeFailure);
  }

  /// How many of this computer's physical pixels show one of the host's:
  /// the picture's scale, within [kMinBitmapScale] and [kMaxBitmapScale]
  /// and to at most [kMaxBitmapSide] a side, in logical pixels.
  double _bitmapScale(DesktopCursorShape shape) {
    final display = geometry.value?.displayWidth ?? 0;
    final shown =
        viewport.toLocal(const Offset(1, 0)).dx -
        viewport.toLocal(Offset.zero).dx;
    final picture = display > 0 && shown > 0 ? shown / display : 1.0;
    final logical = math.min(
      picture.clamp(kMinBitmapScale, kMaxBitmapScale),
      kMaxBitmapSide / math.max(shape.width, shape.height),
    );
    return logical * _dpr;
  }

  /// The runner's bitmap of [shape] at about [scale] for the current device
  /// pixel ratio, made if it has none.
  _NativeBitmap _bitmapFor(
    DesktopPointerCursors native,
    DesktopCursorShape shape,
    double scale,
  ) {
    for (var i = _bitmaps.length - 1; i >= 0; i--) {
      final held = _bitmaps[i];
      if (!held.fits(shape, scale, _dpr)) continue;
      if (i != _bitmaps.length - 1) _bitmaps.add(_bitmaps.removeAt(i));
      return held;
    }
    final made = _NativeBitmap(_nextBitmap++, shape, scale, _dpr);
    native
        .create(
          made.id,
          scalePointerBitmap(shape, scale),
          devicePixelRatio: _dpr,
        )
        .then<void>((_) {}, onError: _onNativeFailure);
    _bitmaps.add(made);
    while (_bitmaps.length > kNativeBitmapCache) {
      native.delete(_bitmaps.removeAt(0).id).ignore();
    }
    return made;
  }

  void _forgetBitmaps() {
    final native = _native;
    for (final bitmap in _bitmaps) {
      native?.delete(bitmap.id).ignore();
    }
    _bitmaps.clear();
    _shownBitmap = null;
  }

  /// The runner refused: paint the host's shape from now on.
  void _onNativeFailure(Object error) {
    if (_disposed || _nativeFailed) return;
    _nativeFailed = true;
    debugPrint('Desktop pointer: painting the host shape instead ($error)');
    _shownBitmap = null;
    _update();
  }

  @override
  void dispose() {
    _detach(null);
    _disposed = true;
    cursor.removeListener(_update);
    controlling.removeListener(_update);
    relative?.removeListener(_update);
    _forgetBitmaps();
    super.dispose();
  }
}

/// One bitmap the runner holds: [shape] at [scale] physical pixels a host
/// pixel, made for [devicePixelRatio], which on macOS and Linux fixes its
/// size in logical pixels.
final class _NativeBitmap {
  _NativeBitmap(this.id, this.shape, this.scale, this.devicePixelRatio);

  final int id;
  final DesktopCursorShape shape;
  final double scale;
  final double devicePixelRatio;

  /// Shows [other] at [at], to within 2 %, at [dpr]: the same physical
  /// size at another ratio is another logical size.
  bool fits(DesktopCursorShape other, double at, double dpr) =>
      dpr == devicePixelRatio &&
      (at / scale - 1).abs() < 0.02 &&
      (identical(other, shape) || other.sameAs(shape));
}

/// [RemotePointer.shapeCursor]: its session shows the pointer's look
/// itself for as long as the mouse tracker keeps it, so a change of the
/// host's shape waits for no rebuild and no frame.
final class RemoteShapeCursor extends MouseCursor {
  const RemoteShapeCursor._(this._pointer);

  final RemotePointer _pointer;

  @override
  MouseCursorSession createSession(int device) =>
      _RemoteShapeSession(this, device);

  @override
  String get debugDescription => 'RemoteShapeCursor';
}

final class _RemoteShapeSession extends MouseCursorSession {
  _RemoteShapeSession(RemoteShapeCursor super.cursor, super.device);

  RemotePointer get _pointer => (cursor as RemoteShapeCursor)._pointer;

  @override
  Future<void> activate() async => _pointer._attach(this);

  @override
  void dispose() => _pointer._detach(this);
}

/// The picture under the local pointer: it takes the host's shape there
/// ([RemotePointer.shapeCursor]) while this viewer controls. Only control
/// rebuilds it.
class RemotePointerRegion extends StatelessWidget {
  const RemotePointerRegion({
    super.key,
    required this.pointer,
    required this.child,
  });

  final RemotePointer pointer;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    pointer.devicePixelRatio = MediaQuery.maybeDevicePixelRatioOf(context) ?? 1;
    return ValueListenableBuilder<bool>(
      valueListenable: pointer.controlling,
      child: child,
      builder: (context, controlling, child) => MouseRegion(
        cursor: controlling ? pointer.shapeCursor : MouseCursor.defer,
        opaque: false,
        onEnter: (_) => pointer.over = true,
        onExit: (_) => pointer.over = false,
        child: child,
      ),
    );
  }
}

/// The remote cursor over the picture (D11, D12): this viewer's own, which
/// follows the host's reports only as [RemoteInput.shownCursor] says, so it
/// never jumps back to where a lagging report left it, drawn as [pointer]
/// says; without one, the generic arrow.
///
/// Its own layer: the arrow moves with every stroke, hover and report, and
/// repaints nothing around it. It repaints only when what it paints
/// changes, so a hover while it paints nothing (in control, the local
/// pointer shows the host's shape) asks for no frame: over a still
/// picture, moving the mouse draws nothing.
class RemoteCursorLayer extends StatefulWidget {
  const RemoteCursorLayer({
    super.key,
    required this.viewport,
    required this.input,
    this.pointer,
  });

  final RemoteViewport viewport;
  final RemoteInput input;
  final RemotePointer? pointer;

  @override
  State<RemoteCursorLayer> createState() => _RemoteCursorLayerState();
}

class _RemoteCursorLayerState extends State<RemoteCursorLayer> {
  /// What the painter last drew.
  RemoteCursorPaint? _painted;

  /// Ticks for the painter when what it would draw now is not what it last
  /// drew: from nothing to nothing it never does.
  final _stale = ValueNotifier(0);

  /// What may change what is drawn: the view, the cursor, its look and the
  /// display's size the shape's scale is read against.
  late Listenable _sources;

  @override
  void initState() {
    super.initState();
    _listen();
  }

  @override
  void didUpdateWidget(covariant RemoteCursorLayer oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (identical(oldWidget.viewport, widget.viewport) &&
        identical(oldWidget.input, widget.input) &&
        identical(oldWidget.pointer, widget.pointer)) {
      return;
    }
    _sources.removeListener(_update);
    _listen();
  }

  @override
  void dispose() {
    _sources.removeListener(_update);
    _stale.dispose();
    super.dispose();
  }

  void _listen() {
    final RemoteCursorLayer(:viewport, :input, :pointer) = widget;
    _sources = Listenable.merge([
      viewport,
      input.cursorChanges,
      ?pointer,
      ?pointer?.geometry,
    ])..addListener(_update);
    _update();
  }

  void _update() {
    if (_paintNow() != _painted) _stale.value++;
  }

  /// What the painter draws, read as it paints: a frame that lays the stage
  /// out anew moves the picture before the viewport tells anyone (it waits
  /// for the frame's end), and its paint still draws the cursor where the
  /// picture now shows it.
  RemoteCursorPaint? _draw() => _painted = _paintNow();

  /// Where to draw, in the stage's coordinates, and what.
  RemoteCursorPaint? _paintNow() {
    final RemoteCursorLayer(:viewport, :input, :pointer) = widget;
    final n = input.shownCursor;
    if (n == null) return null;
    final at = viewport.toLocal(n);
    if (pointer == null) return (at: at, shape: null, scale: 1);
    final shape = pointer.cursor.shape;
    final image = shape?.image;
    switch (pointer.value.paint) {
      case RemotePointerPaint.none:
        return null;
      case RemotePointerPaint.shape when shape != null && image != null:
        return (at: at, shape: shape, scale: _scale(pointer, shape));
      case RemotePointerPaint.arrow || RemotePointerPaint.shape:
        return (at: at, shape: null, scale: 1);
    }
  }

  /// The host's pixels as the picture shows them now, zoom included, but
  /// never so small that the shape is under [kMinPointerSide].
  double _scale(RemotePointer pointer, DesktopPointerShape shape) {
    final viewport = widget.viewport;
    final display = pointer.geometry.value?.displayWidth ?? 0;
    final shown =
        viewport.toLocal(const Offset(1, 0)).dx -
        viewport.toLocal(Offset.zero).dx;
    final picture = display > 0 && shown > 0 ? shown / display : 1.0;
    final d = shape.definition;
    return math.max(picture, kMinPointerSide / math.max(d.width, d.height));
  }

  @override
  Widget build(BuildContext context) => RepaintBoundary(
    child: IgnorePointer(
      child: ClipRect(
        child: CustomPaint(
          size: Size.infinite,
          painter: RemoteCursorPainter(cursor: _draw, repaint: _stale),
        ),
      ),
    ),
  );
}

/// The smallest a painted shape's longer side gets, in logical pixels: a
/// phone shows a whole desktop small, and its pointer must stay legible.
const kMinPointerSide = 32.0;

/// High-contrast fills for up to four concurrent controllers (`PeerColor`
/// `0..4`): coral, sky, amber, emerald.
const kPeerCursorColors = <Color>[
  Color(0xFFFF5A5F),
  Color(0xFF0EA5E9),
  Color(0xFFF59E0B),
  Color(0xFF10B981),
];

Color peerCursorColor(int index) => kPeerCursorColors[index & 3];

final Path _arrowPath = Path()
  ..moveTo(1, 1)
  ..lineTo(1, 20)
  ..lineTo(6, 16)
  ..lineTo(10, 25)
  ..lineTo(14, 23)
  ..lineTo(10, 15)
  ..lineTo(17, 15)
  ..close();

/// Other controlling viewers' pointers, each in its session colour, drawn in
/// an isolated layer beneath [RemoteCursorLayer]. While empty it renders a
/// zero-cost placeholder with no compositor layer or viewport listener.
class RemotePeerCursorLayer extends StatefulWidget {
  const RemotePeerCursorLayer({
    super.key,
    required this.viewport,
    required this.peerCursors,
  });

  final RemoteViewport viewport;
  final ValueListenable<List<PeerCursorPoint>> peerCursors;

  @override
  State<RemotePeerCursorLayer> createState() => _RemotePeerCursorLayerState();
}

class _RemotePeerCursorLayerState extends State<RemotePeerCursorLayer> {
  late bool _active = widget.peerCursors.value.isNotEmpty;

  @override
  void initState() {
    super.initState();
    widget.peerCursors.addListener(_onPeersChanged);
  }

  @override
  void didUpdateWidget(covariant RemotePeerCursorLayer oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.peerCursors, widget.peerCursors)) {
      oldWidget.peerCursors.removeListener(_onPeersChanged);
      widget.peerCursors.addListener(_onPeersChanged);
      _active = widget.peerCursors.value.isNotEmpty;
    }
  }

  @override
  void dispose() {
    widget.peerCursors.removeListener(_onPeersChanged);
    super.dispose();
  }

  void _onPeersChanged() {
    final active = widget.peerCursors.value.isNotEmpty;
    if (active != _active) {
      setState(() => _active = active);
    }
  }

  @override
  Widget build(BuildContext context) {
    if (!_active) return const SizedBox.shrink();
    return RepaintBoundary(
      child: IgnorePointer(
        child: ClipRect(
          child: CustomPaint(
            size: Size.infinite,
            painter: RemotePeerCursorPainter(
              viewport: widget.viewport,
              peerCursors: widget.peerCursors,
              repaint: Listenable.merge([widget.viewport, widget.peerCursors]),
            ),
          ),
        ),
      ),
    );
  }
}

class RemotePeerCursorPainter extends CustomPainter {
  RemotePeerCursorPainter({
    required this.viewport,
    required this.peerCursors,
    required Listenable repaint,
  }) : super(repaint: repaint);

  final RemoteViewport viewport;
  final ValueListenable<List<PeerCursorPoint>> peerCursors;

  static final _fills = <Paint>[
    for (final color in kPeerCursorColors) Paint()..color = color,
  ];
  static final _edge = Paint()
    ..color = const Color(0xFFFFFFFF)
    ..style = PaintingStyle.stroke
    ..strokeJoin = StrokeJoin.round
    ..strokeWidth = 1.5;

  @override
  void paint(Canvas canvas, Size _) {
    final peers = peerCursors.value;
    if (peers.isEmpty) return;
    for (final peer in peers) {
      final at = viewport.toLocal(peer.position);
      final path = _arrowPath.shift(at - const Offset(1, 1));
      canvas
        ..drawPath(path, _fills[peer.color & 3])
        ..drawPath(path, _edge);
    }
  }

  @override
  bool shouldRepaint(covariant RemotePeerCursorPainter oldDelegate) =>
      !identical(oldDelegate.viewport, viewport) ||
      !identical(oldDelegate.peerCursors, peerCursors);
}

/// Where the cursor is drawn, and as what: [shape]'s bitmap at [scale],
/// its hotspot at [at], or with no shape the generic arrow.
typedef RemoteCursorPaint = ({
  Offset at,
  DesktopPointerShape? shape,
  double scale,
});

/// The host's shape, or an arrow 18×27 whatever the zoom: black with a
/// white edge, its tip at the point.
class RemoteCursorPainter extends CustomPainter {
  RemoteCursorPainter({required this.cursor, required Listenable repaint})
    : super(repaint: repaint);

  final RemoteCursorPaint? Function() cursor;

  static const size = Size(18, 27);

  static final _fill = Paint()..color = const Color(0xFF000000);
  static final _edge = Paint()
    ..color = const Color(0xFFFFFFFF)
    ..style = PaintingStyle.stroke
    ..strokeWidth = 1.5;
  static final _bitmap = Paint()..filterQuality = FilterQuality.medium;

  @override
  void paint(Canvas canvas, Size _) {
    final what = cursor();
    if (what == null) return;
    final shape = what.shape, image = shape?.image;
    if (shape != null && image != null) {
      final d = shape.definition;
      final topLeft =
          what.at - Offset(d.hotX.toDouble(), d.hotY.toDouble()) * what.scale;
      canvas.drawImageRect(
        image,
        Rect.fromLTWH(0, 0, d.width.toDouble(), d.height.toDouble()),
        topLeft & Size(d.width * what.scale, d.height * what.scale),
        _bitmap,
      );
      return;
    }
    // The arrow's tip is at (1, 1).
    final path = _arrowPath.shift(what.at - const Offset(1, 1));
    canvas
      ..drawPath(path, _fill)
      ..drawPath(path, _edge);
  }

  @override
  bool shouldRepaint(covariant RemoteCursorPainter oldDelegate) =>
      oldDelegate.cursor != cursor;
}
