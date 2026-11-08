import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import 'jpeg_picture.dart';
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
/// It is its own layer, as are the cursor and the touch ring inside it: a
/// stroke, a pan or a zoom repaints no gutter, bar or title.
class ViewerStage extends StatelessWidget {
  const ViewerStage({
    super.key,
    required this.session,
    required this.geometry,
    required this.picture,
    required this.jpegImage,
    required this.stats,
    required this.hostCursor,
    required this.viewport,
    required this.input,
    required this.machine,
    required this.touchMode,
    required this.title,
    required this.actions,
    this.compact = false,
    this.toolbarInset,
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
  }) => ViewerStage(
    key: key,
    session: desktop.session,
    geometry: desktop.geometry,
    picture: desktop.picture,
    jpegImage: desktop.jpegImage,
    stats: desktop.stats,
    hostCursor: desktop.hostCursor,
    viewport: viewport,
    input: input,
    machine: machine,
    touchMode: touchMode,
    title: title,
    actions: actions,
    compact: compact,
    toolbarInset: toolbarInset,
  );

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopGeometry?> geometry;
  final ValueListenable<DesktopPicture?> picture;
  final ValueListenable<ui.Image?> jpegImage;
  final ValueListenable<DesktopStats> stats;
  final ValueListenable<Offset?> hostCursor;
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

  @override
  Widget build(BuildContext context) => RepaintBoundary(child: _stack(context));

  Widget _stack(BuildContext context) {
    final l10n = context.l10n;
    return Stack(
      fit: StackFit.expand,
      children: [
        _ViewportSync(
          picture: picture,
          geometry: geometry,
          prefs: actions.prefs,
          viewport: viewport,
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
            child: ValueListenableBuilder(
              valueListenable: picture,
              builder: (context, picture, _) {
                final id = picture?.textureId;
                return id != null
                    ? Texture(
                        key: ValueKey(id),
                        textureId: id,
                        filterQuality: FilterQuality.low,
                      )
                    : JpegPicture(image: jpegImage);
              },
            ),
          ),
        ),
        GreyStill(session: session, viewport: viewport),
        RemoteCursorLayer(
          hostCursor: hostCursor,
          viewport: viewport,
          input: input,
        ),
        TouchIndicator(machine: machine),
        if (compact)
          _PictureFrame(viewport: viewport, child: _overlays)
        else
          _overlays,
      ],
    );
  }

  Widget get _overlays => ViewerOverlayLayer(
    session: session,
    stats: stats,
    title: title,
    actions: actions,
    compact: compact,
    toolbarInset: toolbarInset,
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

/// Keeps the viewport's aspect and scale mode in step with the picture,
/// the remote display and the scale preference. A reconnect that keeps the
/// display keeps the user's zoom and pan.
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
  Size? _remote;

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
    _remote = null;
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
    _remote = null;
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
    final remote = g == null
        ? Size.zero
        : Size(g.displayWidth.toDouble(), g.displayHeight.toDouble());
    final mode = widget.prefs.scaleMode;
    final modeChanged = mode != viewport.mode;
    final sizeChanged = mode == ViewerScaleMode.original && remote != _remote;
    if (modeChanged || sizeChanged) {
      _remote = remote;
      viewport.setMode(mode, remotePixels: remote, dpr: _dpr);
    }
  }

  @override
  Widget build(BuildContext context) => const SizedBox.shrink();
}
