import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import '../common/l10n_extension.dart';
import '../ffi/desktop_api.dart';
import '../state/desktop_store.dart';
import 'desktop_connection_banner.dart';
import 'desktop_gesture_surface.dart';

class DesktopViewer extends StatefulWidget {
  const DesktopViewer({super.key, required this.store});
  final DesktopStore store;
  @override
  State<DesktopViewer> createState() => _DesktopViewerState();
}

class _DesktopViewerState extends State<DesktopViewer>
    with WidgetsBindingObserver {
  ui.Image? _image;
  DesktopFrame? _decoded;
  DesktopStatus? _rendered, _videoGeometry;
  int _viewEpoch = -1;
  int? _renderedEpoch;
  DateTime? _decodedAt;
  bool _decoding = false, _trackpad = false, _foreground = true;
  double _lastAspectRatio = 16 / 9;
  final _focus = FocusNode();
  final _keys = <LogicalKeyboardKey>{};
  final _surface = GlobalKey<DesktopGestureSurfaceState>();
  DesktopStore get store => widget.store;
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _foreground =
        WidgetsBinding.instance.lifecycleState == null ||
        WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed;
    store.setViewerForeground(_foreground);
    store.addListener(_update);
    store.cursorStatus.addListener(_updateCursor);
    _update();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final foreground = state == AppLifecycleState.resumed;
    if (_foreground && !foreground) {
      _releaseLocalInput();
    }
    _foreground = foreground;
    store.setViewerForeground(foreground);
  }

  void _update() {
    if (_viewEpoch != store.mediaEpoch) {
      _viewEpoch = store.mediaEpoch;
      final previous = _image;
      _image = null;
      _decoded = null;
      _rendered = null;
      _videoGeometry = null;
      _renderedEpoch = null;
      _decodedAt = null;
      _keys.clear();
      if (previous != null) {
        WidgetsBinding.instance.addPostFrameCallback((_) => previous.dispose());
      }
    }
    if (store.frame?.status.codec == 'h264') {
      final video = store.frame!.status;
      if (video.videoWidth >= 2 && video.videoHeight >= 2) {
        _videoGeometry = video;
        _lastAspectRatio = video.videoWidth / video.videoHeight;
      }
      _rendered = store.videoPresented;
      _renderedEpoch = _rendered == null ? null : store.mediaEpoch;
    }
    if (mounted) setState(() {});
    _updateCursor();
    final frame = store.frame;
    if (_decoding ||
        frame == null ||
        frame.status.codec == 'h264' ||
        identical(frame, _decoded) ||
        frame.bytes.isEmpty) {
      return;
    }
    _decoding = true;
    unawaited(_decode(frame, store.mediaEpoch));
  }

  void _updateCursor() {
    final status = store.cursorStatus.value;
    final display = _rendered?.display;
    final cursor = status?.generation == _rendered?.generation
        ? status?.cursor
        : null;
    _surface.currentState?.updateRemoteCursor(
      cursor == null ||
              !cursor.visible ||
              display == null ||
              display.width == 0 ||
              display.height == 0
          ? null
          : Offset(cursor.x / display.width, cursor.y / display.height),
    );
  }

  Future<void> _decode(DesktopFrame frame, int epoch) async {
    final received = DateTime.now();
    try {
      final buffer = await ui.ImmutableBuffer.fromUint8List(frame.bytes);
      final codec = await ui.instantiateImageCodecWithSize(
        buffer,
        getTargetSize: (width, height) {
          final scale = math.min(1.0, math.min(1600 / width, 1000 / height));
          return ui.TargetImageSize(
            width: math.max(1, (width * scale).round()),
            height: math.max(1, (height * scale).round()),
          );
        },
      );
      final info = await codec.getNextFrame();
      codec.dispose();
      if (!mounted ||
          epoch != store.mediaEpoch ||
          store.frame?.status.generation != frame.status.generation ||
          store.frame?.status.codec == 'h264' ||
          DateTime.now().difference(received) >= const Duration(seconds: 2)) {
        info.image.dispose();
        return;
      }
      final previous = _image;
      setState(() {
        _image = info.image;
        _lastAspectRatio = info.image.width / info.image.height;
        _decoded = frame;
        _rendered = null;
        _renderedEpoch = null;
        _decodedAt = received;
      });
      WidgetsBinding.instance.addPostFrameCallback((_) {
        previous?.dispose();
        if (mounted &&
            epoch == store.mediaEpoch &&
            identical(_decoded, frame)) {
          setState(() {
            _rendered = frame.status;
            _renderedEpoch = epoch;
          });
          store.acknowledgePresented(frame.status, epoch);
        }
      });
    } catch (e) {
      if (mounted && epoch == store.mediaEpoch) {
        store.error = e.toString();
      }
    } finally {
      _decoding = false;
      // Pick up a newer frame only. A decode failure is not retried in a loop.
      if (mounted && !identical(store.frame, frame)) _update();
    }
  }

  @override
  void didUpdateWidget(covariant DesktopViewer oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.store, store)) {
      oldWidget.store.removeListener(_update);
      oldWidget.store.cursorStatus.removeListener(_updateCursor);
      oldWidget.store.setViewerForeground(false);
      _viewEpoch = -1;
      store.setViewerForeground(_foreground);
      store.addListener(_update);
      store.cursorStatus.addListener(_updateCursor);
      _update();
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    store.removeListener(_update);
    store.cursorStatus.removeListener(_updateCursor);
    store.setViewerForeground(false);
    _focus.dispose();
    _image?.dispose();
    super.dispose();
  }

  bool get _interactive =>
      store.controlling &&
      _rendered != null &&
      _renderedEpoch == store.mediaEpoch &&
      (store.frame?.status.codec == 'h264' ||
          (_decodedAt != null &&
              DateTime.now().difference(_decodedAt!) <
                  const Duration(seconds: 2))) &&
      _rendered!.generation == store.frame?.status.generation;
  Future<void> _send(Map<String, Object?> event) async {
    final rendered = _rendered;
    if (rendered != null && _interactive) {
      await store.send(rendered, event, mediaEpoch: _renderedEpoch);
    }
  }

  Future<void> _move(Offset normalized) => _send({
    'kind': 'move',
    'x': (normalized.dx * (_rendered?.display?.width ?? 0)).floor().clamp(
      0,
      math.max(0, (_rendered?.display?.width ?? 1) - 1),
    ),
    'y': (normalized.dy * (_rendered?.display?.height ?? 0)).floor().clamp(
      0,
      math.max(0, (_rendered?.display?.height ?? 1) - 1),
    ),
  });
  Future<void> _click(String button) async {
    await _send({'kind': 'button', 'button': button, 'down': true});
    await _send({'kind': 'button', 'button': button, 'down': false});
  }

  Future<void> _key(String key) async {
    await _send({'kind': 'key', 'key': key, 'down': true});
    await _send({'kind': 'key', 'key': key, 'down': false});
  }

  KeyEventResult _keyboard(FocusNode node, KeyEvent event) {
    if (!_interactive) return KeyEventResult.ignored;
    final key = _keyNames[event.logicalKey];
    if (key != null) {
      if (event is KeyUpEvent) {
        _keys.remove(event.logicalKey);
      } else {
        _keys.add(event.logicalKey);
      }
      unawaited(
        _send({'kind': 'key', 'key': key, 'down': event is! KeyUpEvent}),
      );
      return KeyEventResult.handled;
    }
    if (event is! KeyUpEvent &&
        event.character != null &&
        !HardwareKeyboard.instance.isControlPressed &&
        !HardwareKeyboard.instance.isAltPressed &&
        !HardwareKeyboard.instance.isMetaPressed) {
      unawaited(_send({'kind': 'text', 'text': event.character}));
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  @override
  Widget build(BuildContext context) {
    final l = context.l10n;
    final status = store.snapshot.viewer;
    final hasGrant = status?.controlling == true;
    return Scaffold(
      appBar: AppBar(
        title: Text(l.desktopTitle),
        actions: [
          TextButton(
            onPressed: () async {
              await store.disconnect();
              if (context.mounted) Navigator.pop(context);
            },
            child: Text(l.desktopDisconnect),
          ),
        ],
      ),
      body: SafeArea(
        child: Column(
          children: [
            DesktopConnectionBanner(store: store),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
              child: Wrap(
                spacing: 8,
                runSpacing: 4,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  FilledButton.tonal(
                    onPressed: hasGrant
                        ? () => store.control(false)
                        : store.busy ||
                              !store.fresh ||
                              status?.controlAllowed != true
                        ? null
                        : () => store.control(true),
                    child: Text(
                      hasGrant ? l.desktopEndControl : l.desktopRequestControl,
                    ),
                  ),
                  SegmentedButton<bool>(
                    key: const Key('desktop-touch-mode'),
                    showSelectedIcon: false,
                    segments: [
                      ButtonSegment(
                        value: false,
                        label: Text(l.desktopTouchMode),
                      ),
                      ButtonSegment(
                        value: true,
                        label: Text(l.desktopTrackpadMode),
                      ),
                    ],
                    selected: {_trackpad},
                    onSelectionChanged: (values) {
                      _releaseLocalInput();
                      setState(() => _trackpad = values.single);
                    },
                  ),
                ],
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
              child: Text(
                l.desktopGestureHelp,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ),
            Expanded(
              child: Focus(
                focusNode: _focus,
                autofocus: true,
                onKeyEvent: _keyboard,
                onFocusChange: (focused) {
                  if (!focused) _releaseLocalInput();
                },
                child: ColoredBox(
                  color: Colors.black,
                  child: _screen(context, status),
                ),
              ),
            ),
            SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.all(8),
              child: Row(
                children: [
                  IconButton(
                    tooltip: l.desktopKeyboard,
                    onPressed: _interactive ? () => _text(context) : null,
                    icon: const Icon(Icons.keyboard_outlined),
                  ),
                  IconButton(
                    tooltip: l.desktopLeftClick,
                    onPressed: _interactive ? () => _click('left') : null,
                    icon: const Icon(Icons.ads_click),
                  ),
                  IconButton(
                    tooltip: l.desktopRightClick,
                    onPressed: _interactive ? () => _click('right') : null,
                    icon: const Icon(Icons.mouse_outlined),
                  ),
                  IconButton(
                    tooltip: l.desktopScrollUp,
                    onPressed: _interactive
                        ? () => _send({
                            'kind': 'wheel',
                            'horizontal': 0,
                            'vertical': -3,
                          })
                        : null,
                    icon: const Icon(Icons.arrow_upward),
                  ),
                  IconButton(
                    tooltip: l.desktopScrollDown,
                    onPressed: _interactive
                        ? () => _send({
                            'kind': 'wheel',
                            'horizontal': 0,
                            'vertical': 3,
                          })
                        : null,
                    icon: const Icon(Icons.arrow_downward),
                  ),
                  for (final key in ['escape', 'tab', 'enter', 'backspace'])
                    TextButton(
                      onPressed: _interactive ? () => _key(key) : null,
                      child: Text(key),
                    ),
                  TextButton(
                    onPressed: _interactive
                        ? () async {
                            await _send({
                              'kind': 'key',
                              'key': 'control',
                              'down': true,
                            });
                            await _key('a');
                            await _send({
                              'kind': 'key',
                              'key': 'control',
                              'down': false,
                            });
                          }
                        : null,
                    child: const Text('Ctrl+A'),
                  ),
                  IconButton(
                    tooltip: l.desktopRelease,
                    onPressed: store.release,
                    icon: const Icon(Icons.pan_tool_outlined),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  void _releaseLocalInput() {
    _surface.currentState?.cancelInput();
    if (_keys.isNotEmpty) {
      _keys.clear();
      unawaited(store.release());
    }
  }

  Widget _screen(BuildContext context, DesktopStatus? status) {
    final texture = store.videoTexture;
    final video = texture == null ? null : _videoGeometry;
    final image = _image;
    final hasPicture = video != null || image != null;
    final display = _rendered?.display;
    final cursor = status?.generation == _rendered?.generation
        ? status?.cursor
        : _rendered?.cursor;
    return Stack(
      fit: StackFit.expand,
      children: [
        DesktopGestureSurface(
          key: _surface,
          aspectRatio: _lastAspectRatio,
          enabled: _interactive,
          trackpad: _trackpad,
          inputEpoch: (store, store.mediaEpoch, store.frame?.status.generation),
          cursor:
              cursor == null ||
                  !cursor.visible ||
                  display == null ||
                  display.width == 0 ||
                  display.height == 0
              ? null
              : Offset(cursor.x / display.width, cursor.y / display.height),
          onFocus: _focus.requestFocus,
          onMove: _move,
          onButton: (button, down) =>
              _send({'kind': 'button', 'button': button, 'down': down}),
          onWheel: (delta) => _send({
            'kind': 'wheel',
            'horizontal': (delta.dx / 30).round().clamp(-100, 100),
            'vertical': (delta.dy / 30).round().clamp(-100, 100),
          }),
          onRelease: store.release,
          child: Stack(
            fit: StackFit.expand,
            children: [
              if (video != null)
                Texture(
                  key: ValueKey(texture!.textureId),
                  textureId: texture.textureId,
                  filterQuality: FilterQuality.low,
                )
              else if (image != null)
                RawImage(
                  image: image,
                  fit: BoxFit.fill,
                  filterQuality: FilterQuality.low,
                ),
            ],
          ),
        ),
        if (!hasPicture)
          IgnorePointer(
            child: Center(
              child: Text(
                status?.state == 'stopped'
                    ? context.l10n.desktopStopped
                    : context.l10n.desktopConnecting,
                style: const TextStyle(color: Colors.white),
              ),
            ),
          )
        else if (!store.fresh)
          // Never cover the entire native texture with an opaque layer. Flutter
          // still needs to consume it to acknowledge the next recovered frame.
          Align(
            alignment: Alignment.bottomCenter,
            child: IgnorePointer(
              child: Container(
                margin: const EdgeInsets.all(12),
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 8,
                ),
                decoration: BoxDecoration(
                  color: Colors.black.withValues(alpha: 0.65),
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Text(
                  context.l10n.desktopStale,
                  style: const TextStyle(color: Colors.white),
                  textAlign: TextAlign.center,
                ),
              ),
            ),
          ),
      ],
    );
  }

  Future<void> _text(BuildContext context) async {
    final field = TextEditingController();
    final text = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(context.l10n.desktopKeyboard),
        content: TextField(controller: field, autofocus: true, maxLength: 256),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, field.text),
            child: Text(context.l10n.desktopSendText),
          ),
        ],
      ),
    );
    field.dispose();
    if (text != null && text.isNotEmpty) {
      await _send({'kind': 'text', 'text': text});
    }
  }
}

final _keyNames = <LogicalKeyboardKey, String>{
  LogicalKeyboardKey.shiftLeft: 'shift',
  LogicalKeyboardKey.shiftRight: 'shift',
  LogicalKeyboardKey.controlLeft: 'control',
  LogicalKeyboardKey.controlRight: 'control',
  LogicalKeyboardKey.altLeft: 'alt',
  LogicalKeyboardKey.altRight: 'alt',
  LogicalKeyboardKey.metaLeft: 'meta',
  LogicalKeyboardKey.metaRight: 'meta',
  LogicalKeyboardKey.enter: 'enter',
  LogicalKeyboardKey.escape: 'escape',
  LogicalKeyboardKey.tab: 'tab',
  LogicalKeyboardKey.backspace: 'backspace',
  LogicalKeyboardKey.delete: 'delete',
  LogicalKeyboardKey.arrowLeft: 'left',
  LogicalKeyboardKey.arrowRight: 'right',
  LogicalKeyboardKey.arrowUp: 'up',
  LogicalKeyboardKey.arrowDown: 'down',
  LogicalKeyboardKey.home: 'home',
  LogicalKeyboardKey.end: 'end',
  LogicalKeyboardKey.pageUp: 'page_up',
  LogicalKeyboardKey.pageDown: 'page_down',
  LogicalKeyboardKey.keyA: 'a',
  LogicalKeyboardKey.keyC: 'c',
  LogicalKeyboardKey.keyV: 'v',
  LogicalKeyboardKey.keyX: 'x',
  LogicalKeyboardKey.keyZ: 'z',
};
