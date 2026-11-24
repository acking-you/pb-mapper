import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/rendering.dart' show PlatformViewHitTestBehavior;
import 'package:flutter/services.dart' show StandardMessageCodec;
import 'package:flutter/widgets.dart';

import '../../../common/l10n_extension.dart';
import '../../../ffi/desktop_video.dart' show VideoPlacement;
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import 'jpeg_picture.dart';
import 'native_video_hole.dart';
import 'remote_cursor_layer.dart';
import 'remote_input.dart';
import 'remote_surface.dart';
import 'remote_viewport.dart';
import 'touch_gestures.dart';
import 'viewer_menus.dart';
import 'viewer_overlays.dart';

/// The picture, the remote cursor and the overlays: what both viewers show
/// (ui §7.2).
///
/// It takes listenables, not the store, so a test drives its phases with
/// `ValueNotifier`s. The picture element stays the same across zoom, pan,
/// overlays, reconnects and the soft keyboard; nothing that changes at
/// media or state rate rebuilds above the `Texture`.
///
/// It is its own layer, as are the cursor and the touch ring inside it (or
/// above it, see [touchIndicator]): a stroke, a pan or a zoom repaints no
/// gutter, bar or title.
class ViewerStage extends StatefulWidget {
  const ViewerStage({
    super.key,
    required this.session,
    required this.geometry,
    required this.picture,
    required this.jpegImage,
    required this.stats,
    required this.viewport,
    required this.input,
    required this.machine,
    required this.touchMode,
    required this.title,
    required this.actions,
    this.compact = false,
    this.toolbarInset,
    this.touchIndicator = true,
    this.cursor,
    this.peerCursors,
    this.localPointer = false,
    this.greyPicture,
    this.placePicture,
    this.enhancePicture,
  });

  /// The stage over [desktop]'s own slices.
  factory ViewerStage.of(
    DesktopStore desktop, {
    Key? key,
    required RemoteViewport viewport,
    required RemoteInput input,
    required TouchGestureMachine machine,
    required ValueListenable<TouchMode> touchMode,
    required String title,
    required ViewerActions actions,
    bool compact = false,
    ValueListenable<double>? toolbarInset,
    bool touchIndicator = true,
    bool localPointer = false,
  }) => ViewerStage(
    key: key,
    session: desktop.session,
    geometry: desktop.geometry,
    picture: desktop.picture,
    jpegImage: desktop.jpegImage,
    stats: desktop.stats,
    viewport: viewport,
    input: input,
    machine: machine,
    touchMode: touchMode,
    title: title,
    actions: actions,
    compact: compact,
    toolbarInset: toolbarInset,
    touchIndicator: touchIndicator,
    cursor: desktop.cursor,
    peerCursors: desktop.peerCursors,
    localPointer: localPointer,
    greyPicture: desktop.greyPicture,
    placePicture: desktop.placePicture,
    enhancePicture: desktop.enhancePicture,
  );

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopGeometry?> geometry;
  final ValueListenable<DesktopPicture?> picture;
  final ValueListenable<ui.Image?> jpegImage;
  final ValueListenable<DesktopStats> stats;
  final RemoteViewport viewport;
  final RemoteInput input;
  final TouchGestureMachine machine;
  final ValueListenable<TouchMode> touchMode;
  final String title;
  final ViewerActions actions;

  /// A phone: cards stay 16 inside the picture, pills sit 12 below its top.
  final bool compact;

  /// Desktop: the top of the pills under the toolbar.
  final ValueListenable<double>? toolbarInset;

  /// False: the [TouchIndicator] is drawn by the viewer instead, above
  /// what it lays over the picture (a phone's gutter strips), where a
  /// stroke or a long press may start.
  final bool touchIndicator;

  /// The session's remote cursor, whose shape the host's pointer takes;
  /// without it, the generic arrow.
  final DesktopCursor? cursor;

  /// Other controlling viewers' pointers on this display.
  final ValueListenable<List<PeerCursorPoint>>? peerCursors;

  /// A desktop viewer: the local pointer hovers the picture, and takes the
  /// host's shape there while it controls (see [RemotePointerLook]).
  final bool localPointer;

  /// Greys a picture the runner presents itself (see [GreyStill]).
  final Future<void> Function(bool on)? greyPicture;

  /// Places the plane a runner draws a texture's picture on, by the
  /// texture's id (see [NativeVideoHole]).
  final Future<void> Function(int textureId, VideoPlacement placement)?
  placePicture;

  /// Sets how much the runner sharpens a texture's enlarged picture, by the
  /// texture's id (see [DesktopPicture.enhances]).
  final Future<void> Function(int textureId, PictureEnhancement level)?
  enhancePicture;

  @override
  State<ViewerStage> createState() => _ViewerStageState();
}

class _ViewerStageState extends State<ViewerStage> {
  RemotePointer? _pointer;

  @override
  void initState() {
    super.initState();
    _pointer = _newPointer();
  }

  RemotePointer? _newPointer() {
    final cursor = widget.cursor;
    return cursor == null
        ? null
        : RemotePointer(
            mouse: widget.localPointer,
            cursor: cursor,
            controlling: widget.input.ready,
            geometry: widget.geometry,
            viewport: widget.viewport,
          );
  }

  @override
  void didUpdateWidget(covariant ViewerStage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (identical(oldWidget.cursor, widget.cursor) &&
        identical(oldWidget.input, widget.input) &&
        identical(oldWidget.geometry, widget.geometry) &&
        identical(oldWidget.viewport, widget.viewport) &&
        oldWidget.localPointer == widget.localPointer) {
      return;
    }
    _pointer?.dispose();
    _pointer = _newPointer();
  }

  @override
  void dispose() {
    _pointer?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => RepaintBoundary(child: _stack(context));

  Widget _stack(BuildContext context) {
    final l10n = context.l10n;
    final ViewerStage(
      :session,
      :geometry,
      :picture,
      :jpegImage,
      :viewport,
      :input,
      :machine,
      :touchMode,
      :title,
      :actions,
      :compact,
      :touchIndicator,
      :peerCursors,
    ) = widget;
    final pointer = _pointer;
    final scene = ValueListenableBuilder(
      valueListenable: picture,
      builder: (context, picture, _) {
        final id = picture?.textureId;
        if (id == null) return JpegPicture(image: jpegImage);
        // A runner that draws pictures on its own plane under the Flutter
        // view (the Linux video plane) gets a hole where the texture would
        // be, which tells it where to draw.
        if (picture?.plane == true) {
          final place = widget.placePicture;
          return NativeVideoHole(
            key: ValueKey(id),
            onPlace: (placement) {
              if (place != null) unawaited(place(id, placement));
            },
          );
        }
        final viewType = picture?.platformViewType;
        // A runner that presents pictures itself (the macOS layer) shows
        // them in its own view, which Flutter composites and transforms as
        // it would the texture. It takes hits as the texture does; the
        // pointer's events still reach Flutter through the native view. It
        // never takes the keyboard focus, which would hand the keys to a
        // view that wants none.
        return viewType != null
            ? ExcludeFocus(
                child: AppKitView(
                  key: ValueKey(id),
                  viewType: viewType,
                  creationParams: <String, Object?>{'textureId': id},
                  creationParamsCodec: const StandardMessageCodec(),
                  hitTestBehavior: PlatformViewHitTestBehavior.opaque,
                ),
              )
            : Texture(
                key: ValueKey(id),
                textureId: id,
                filterQuality: FilterQuality.low,
              );
      },
    );
    return Stack(
      fit: StackFit.expand,
      children: [
        _ViewportSync(
          picture: picture,
          geometry: geometry,
          prefs: actions.prefs,
          viewport: viewport,
        ),
        _EnhanceSync(
          picture: picture,
          prefs: actions.prefs,
          enhancePicture: widget.enhancePicture,
        ),
        ValueListenableBuilder(
          valueListenable: touchMode,
          builder: (context, mode, child) => Semantics(
            container: true,
            label: l10n.viewerPictureLabel(title),
            hint: mode == TouchMode.trackpad
                ? l10n.touchTrackpadHint
                : l10n.touchDirectHint,
            child: child,
          ),
          child: RemoteSurface(
            viewport: viewport,
            input: input,
            machine: machine,
            touchMode: touchMode,
            child: pointer != null && pointer.mouse
                ? RemotePointerRegion(pointer: pointer, child: scene)
                : scene,
          ),
        ),
        GreyStill(
          session: session,
          viewport: viewport,
          picture: picture,
          greyPicture: widget.greyPicture,
        ),
        if (peerCursors != null)
          RemotePeerCursorLayer(viewport: viewport, peerCursors: peerCursors),
        RemoteCursorLayer(viewport: viewport, input: input, pointer: pointer),
        if (touchIndicator) TouchIndicator(machine: machine),
        if (compact)
          _PictureFrame(viewport: viewport, child: _overlays)
        else
          _overlays,
      ],
    );
  }

  Widget get _overlays => ViewerOverlayLayer(
    session: widget.session,
    stats: widget.stats,
    title: widget.title,
    actions: widget.actions,
    compact: widget.compact,
    toolbarInset: widget.toolbarInset,
  );
}

/// A phone: the overlays keep to the picture as fitted between the gutters,
/// less what the soft keyboard covers, so they stay clear of both. Zoom and
/// pan do not move them.
class _PictureFrame extends StatelessWidget {
  const _PictureFrame({required this.viewport, required this.child});

  final RemoteViewport viewport;
  final Widget child;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: viewport,
    child: child,
    // Read at layout, once the surface has laid the viewport out.
    builder: (context, child) => LayoutBuilder(
      builder: (context, _) {
        final fitted = viewport.fittedRect;
        final frame = fitted.intersect(viewport.visibleRect);
        return Stack(
          children: [
            Positioned.fromRect(
              rect: frame.width > 0 && frame.height > 0 ? frame : fitted,
              child: child!,
            ),
          ],
        );
      },
    ),
  );
}

/// Keeps the viewport's aspect, pixels and scale mode in step with the
/// picture, the remote display, the screen and the scale preference. A
/// reconnect that keeps the display keeps the user's zoom and pan.
class _ViewportSync extends StatefulWidget {
  const _ViewportSync({
    required this.picture,
    required this.geometry,
    required this.prefs,
    required this.viewport,
  });

  final ValueListenable<DesktopPicture?> picture;
  final ValueListenable<DesktopGeometry?> geometry;
  final ViewerPrefs prefs;
  final RemoteViewport viewport;

  @override
  State<_ViewportSync> createState() => _ViewportSyncState();
}

class _ViewportSyncState extends State<_ViewportSync> {
  double _dpr = 1;

  Listenable get _sources =>
      Listenable.merge([widget.picture, widget.geometry, widget.prefs]);

  late Listenable _listening;

  @override
  void initState() {
    super.initState();
    _listening = _sources..addListener(_apply);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _dpr = MediaQuery.devicePixelRatioOf(context);
    _apply();
  }

  /// A rebuild of the viewer around it changes nothing: a fresh
  /// [RemoteViewport.setMode] would undo the user's zoom.
  @override
  void didUpdateWidget(covariant _ViewportSync oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (identical(oldWidget.picture, widget.picture) &&
        identical(oldWidget.geometry, widget.geometry) &&
        identical(oldWidget.prefs, widget.prefs) &&
        identical(oldWidget.viewport, widget.viewport)) {
      return;
    }
    _listening.removeListener(_apply);
    _listening = _sources..addListener(_apply);
    _apply();
  }

  @override
  void dispose() {
    _listening.removeListener(_apply);
    super.dispose();
  }

  void _apply() {
    final viewport = widget.viewport;
    final picture = widget.picture.value;
    if (picture != null) viewport.aspectRatio = picture.aspectRatio;
    final g = widget.geometry.value;
    // Unchanged pixels change nothing: a reconnect keeps the zoom.
    viewport.setPixels(
      display: g == null
          ? Size.zero
          : Size(g.displayWidth.toDouble(), g.displayHeight.toDouble()),
      video: g == null
          ? Size.zero
          : Size(g.videoWidth.toDouble(), g.videoHeight.toDouble()),
      dpr: _dpr,
    );
    final mode = widget.prefs.scaleMode;
    if (mode != viewport.mode) viewport.setMode(mode);
  }

  @override
  Widget build(BuildContext context) => const SizedBox.shrink();
}

/// Keeps how much the runner sharpens an enlarged picture in step with the
/// device's preference ([ViewerPrefs.enhance]), for a picture whose runner
/// can ([DesktopPicture.enhances]). A texture starts at
/// [PictureEnhancement.off], so nothing is sent until the preference
/// differs from what the texture has.
class _EnhanceSync extends StatefulWidget {
  const _EnhanceSync({
    required this.picture,
    required this.prefs,
    required this.enhancePicture,
  });

  final ValueListenable<DesktopPicture?> picture;
  final ViewerPrefs prefs;
  final Future<void> Function(int textureId, PictureEnhancement level)?
  enhancePicture;

  @override
  State<_EnhanceSync> createState() => _EnhanceSyncState();
}

class _EnhanceSyncState extends State<_EnhanceSync> {
  /// The texture last told, and what it was told.
  int? _texture;
  PictureEnhancement _level = PictureEnhancement.off;

  late Listenable _listening;

  Listenable get _sources => Listenable.merge([widget.picture, widget.prefs]);

  @override
  void initState() {
    super.initState();
    _listening = _sources..addListener(_apply);
    _apply();
  }

  @override
  void didUpdateWidget(covariant _EnhanceSync oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.picture, widget.picture) ||
        !identical(oldWidget.prefs, widget.prefs)) {
      _listening.removeListener(_apply);
      _listening = _sources..addListener(_apply);
    }
    _apply();
  }

  @override
  void dispose() {
    _listening.removeListener(_apply);
    super.dispose();
  }

  void _apply() {
    final enhance = widget.enhancePicture;
    final picture = widget.picture.value;
    final id = picture != null && picture.enhances ? picture.textureId : null;
    if (enhance == null || id == null) return;
    if (id != _texture) {
      _texture = id;
      _level = PictureEnhancement.off;
    }
    final level = widget.prefs.enhance;
    if (level == _level) return;
    _level = level;
    unawaited(enhance(id, level));
  }

  @override
  Widget build(BuildContext context) => const SizedBox.shrink();
}
