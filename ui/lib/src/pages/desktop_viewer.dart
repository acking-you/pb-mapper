import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import '../common/l10n_extension.dart';
import '../ffi/desktop_api.dart';
import '../state/desktop_store.dart';

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
  DesktopStatus? _rendered;
  int _viewEpoch = -1;
  int? _renderedEpoch;
  DateTime? _decodedAt;
  bool _decoding = false, _navigate = false;
  final _focus = FocusNode();
  final _buttons = <int, String>{};
  final _transform = TransformationController();
  DesktopStore get store => widget.store;
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    store.addListener(_update);
    _update();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) {
      _buttons.clear();
      unawaited(store.control(false));
    }
  }

  void _update() {
    if (_viewEpoch != store.mediaEpoch) {
      _viewEpoch = store.mediaEpoch;
      final previous = _image;
      _image = null;
      _decoded = null;
      _rendered = null;
      _renderedEpoch = null;
      _decodedAt = null;
      _buttons.clear();
      _transform.value = Matrix4.identity();
      if (previous != null) {
        WidgetsBinding.instance.addPostFrameCallback((_) => previous.dispose());
      }
    }
    if (store.frame?.status.codec == 'h264') {
      _rendered = store.videoPresented;
      _renderedEpoch = _rendered == null ? null : store.mediaEpoch;
    }
    if (mounted) setState(() {});
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
        unawaited(store.control(false));
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
      unawaited(oldWidget.store.control(false));
      _viewEpoch = -1;
      store.addListener(_update);
      _update();
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    store.removeListener(_update);
    scheduleMicrotask(() => unawaited(store.control(false)));
    _focus.dispose();
    _transform.dispose();
    _image?.dispose();
    super.dispose();
  }

  bool get _interactive =>
      store.controlling &&
      !_navigate &&
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

  Future<void> _move(Offset point, Size size) => _send({
    'kind': 'move',
    'x': (point.dx / size.width * (_rendered?.display?.width ?? 0))
        .floor()
        .clamp(0, math.max(0, (_rendered?.display?.width ?? 1) - 1)),
    'y': (point.dy / size.height * (_rendered?.display?.height ?? 0))
        .floor()
        .clamp(0, math.max(0, (_rendered?.display?.height ?? 1) - 1)),
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
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
              child: Wrap(
                spacing: 12,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  Text(
                    store.controlling ? l.desktopInteract : l.desktopViewOnly,
                  ),
                  if (status?.path.isNotEmpty == true)
                    Text('${status!.path} · TLS 1.3'),
                  if (status?.desktop == 'secure') Text(l.desktopSecureActive),
                  if (status?.codec == 'h264')
                    Text(
                      '${status!.captureFps.toStringAsFixed(0)} / ${status.receiveFps.toStringAsFixed(0)} / ${store.presentFps.toStringAsFixed(0)} fps · H.264',
                    ),
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
                  FilterChip(
                    label: Text(l.desktopNavigate),
                    selected: _navigate,
                    onSelected: (value) {
                      unawaited(store.release());
                      _buttons.clear();
                      setState(() => _navigate = value);
                    },
                  ),
                ],
              ),
            ),
            if (status?.message.isNotEmpty == true || store.error != null)
              Padding(
                padding: const EdgeInsets.all(8),
                child: Text(
                  store.error ?? status!.message,
                  maxLines: 3,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
            Expanded(
              child: Focus(
                focusNode: _focus,
                autofocus: true,
                onKeyEvent: _keyboard,
                onFocusChange: (focused) {
                  if (!focused) unawaited(store.release());
                },
                child: ColoredBox(
                  color: Colors.black,
                  child: LayoutBuilder(
                    builder: (context, constraints) {
                      final image = _image;
                      final texture = store.videoTexture;
                      final video = store.frame?.status.codec == 'h264'
                          ? store.frame!.status
                          : null;
                      if (video == null && image == null ||
                          video != null &&
                              (texture == null ||
                                  video.videoWidth < 2 ||
                                  video.videoHeight < 2)) {
                        return Center(
                          child: Text(
                            status?.state == 'stopped'
                                ? l.desktopStopped
                                : l.desktopConnecting,
                            style: const TextStyle(color: Colors.white),
                          ),
                        );
                      }
                      final ratio = video != null
                          ? video.videoWidth / video.videoHeight
                          : image!.width / image.height;
                      final width = math.min(
                        constraints.maxWidth,
                        constraints.maxHeight * ratio,
                      );
                      final size = Size(width, width / ratio);
                      return Stack(
                        fit: StackFit.expand,
                        children: [
                          Center(
                            child: SizedBox.fromSize(
                              size: size,
                              child: InteractiveViewer(
                                transformationController: _transform,
                                minScale: 1,
                                maxScale: 5,
                                panEnabled: !_interactive,
                                scaleEnabled: !_interactive,
                                child: Listener(
                                  onPointerHover: (event) {
                                    unawaited(_move(event.localPosition, size));
                                  },
                                  onPointerDown: (event) {
                                    if (!_interactive) return;
                                    _focus.requestFocus();
                                    final button =
                                        event.buttons & kSecondaryMouseButton !=
                                            0
                                        ? 'right'
                                        : event.buttons & kMiddleMouseButton !=
                                              0
                                        ? 'middle'
                                        : 'left';
                                    _buttons[event.pointer] = button;
                                    unawaited(_move(event.localPosition, size));
                                    unawaited(
                                      _send({
                                        'kind': 'button',
                                        'button': button,
                                        'down': true,
                                      }),
                                    );
                                  },
                                  onPointerMove: (event) {
                                    if (_buttons.containsKey(event.pointer)) {
                                      unawaited(
                                        _move(event.localPosition, size),
                                      );
                                    }
                                  },
                                  onPointerUp: (event) {
                                    final button = _buttons.remove(
                                      event.pointer,
                                    );
                                    if (button != null) {
                                      unawaited(
                                        _send({
                                          'kind': 'button',
                                          'button': button,
                                          'down': false,
                                        }),
                                      );
                                    }
                                  },
                                  onPointerCancel: (_) {
                                    _buttons.clear();
                                    unawaited(store.release());
                                  },
                                  onPointerSignal: (event) {
                                    if (event is PointerScrollEvent &&
                                        _interactive) {
                                      unawaited(
                                        _send({
                                          'kind': 'wheel',
                                          'horizontal':
                                              (event.scrollDelta.dx / 30)
                                                  .round()
                                                  .clamp(-100, 100),
                                          'vertical':
                                              (event.scrollDelta.dy / 30)
                                                  .round()
                                                  .clamp(-100, 100),
                                        }),
                                      );
                                    }
                                  },
                                  child: Stack(
                                    fit: StackFit.expand,
                                    clipBehavior: Clip.hardEdge,
                                    children: [
                                      if (video != null)
                                        Texture(
                                          textureId: texture!.textureId,
                                          filterQuality: FilterQuality.low,
                                        )
                                      else
                                        RawImage(
                                          image: image,
                                          fit: BoxFit.fill,
                                          filterQuality: FilterQuality.low,
                                        ),
                                      if (video != null)
                                        ValueListenableBuilder<DesktopStatus?>(
                                          valueListenable:
                                              store.videoPresentation,
                                          builder: (_, rendered, _) =>
                                              _cursor(rendered, size),
                                        )
                                      else
                                        _cursor(_rendered, size),
                                    ],
                                  ),
                                ),
                              ),
                            ),
                          ),
                          if (!store.fresh)
                            ColoredBox(
                              color: Colors.black,
                              child: Center(
                                child: Padding(
                                  padding: const EdgeInsets.all(24),
                                  child: Text(
                                    l.desktopStale,
                                    style: const TextStyle(color: Colors.white),
                                    textAlign: TextAlign.center,
                                  ),
                                ),
                              ),
                            ),
                        ],
                      );
                    },
                  ),
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

  Widget _cursor(DesktopStatus? rendered, Size size) {
    final cursor = rendered?.cursor;
    final display = rendered?.display;
    if (!store.fresh ||
        rendered?.generation != store.frame?.status.generation ||
        cursor == null ||
        !cursor.visible ||
        display == null ||
        display.width <= 0 ||
        display.height <= 0 ||
        cursor.x < 0 ||
        cursor.y < 0 ||
        cursor.x >= display.width ||
        cursor.y >= display.height) {
      return const SizedBox.shrink();
    }
    return Positioned(
      left: cursor.x / display.width * size.width,
      top: cursor.y / display.height * size.height,
      child: const IgnorePointer(
        child: CustomPaint(
          size: Size(18, 27),
          painter: _DesktopCursorPainter(),
        ),
      ),
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

/// A generic pointer marker; operating-system cursor bitmaps are not transported.
class _DesktopCursorPainter extends CustomPainter {
  const _DesktopCursorPainter();
  @override
  void paint(Canvas canvas, Size size) {
    final pointer = Path()
      ..moveTo(1, 1)
      ..lineTo(1, 20)
      ..lineTo(6, 16)
      ..lineTo(10, 25)
      ..lineTo(14, 23)
      ..lineTo(10, 15)
      ..lineTo(17, 15)
      ..close();
    canvas.drawPath(pointer, Paint()..color = Colors.black);
    canvas.drawPath(
      pointer,
      Paint()
        ..color = Colors.white
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
  }

  @override
  bool shouldRepaint(covariant _DesktopCursorPainter oldDelegate) => false;
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
