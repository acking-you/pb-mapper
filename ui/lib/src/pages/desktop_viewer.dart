import 'dart:async';
import 'dart:io';
import 'dart:math' as math;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import '../common/l10n_extension.dart';
import '../state/desktop_store.dart';
import 'desktop_connection_banner.dart';
import 'desktop_gesture_surface.dart';

class DesktopViewer extends StatefulWidget {
  const DesktopViewer({super.key, required this.store});
  final DesktopStore store;
  @override
  State<DesktopViewer> createState() => _DesktopViewerState();
}

class _DesktopViewerState extends State<DesktopViewer> {
  late DesktopDemandHandle _demand;
  Timer? _cursorTimer;
  bool _trackpad = false;
  final _focus = FocusNode();
  final _keys = <LogicalKeyboardKey>{};
  final _surface = GlobalKey<DesktopGestureSurfaceState>();
  DesktopStore get store => widget.store;

  @override
  void initState() {
    super.initState();
    _attach();
  }

  /// The viewer on screen is what keeps the session resumed (D13).
  void _attach() {
    _demand = store.demand(DesktopDemand.viewer);
    store.session.addListener(_sessionChanged);
    _cursorTimer = Timer.periodic(
      const Duration(milliseconds: 33),
      (_) => _updateCursor(),
    );
  }

  void _detach(DesktopStore store) {
    _cursorTimer?.cancel();
    store.session.removeListener(_sessionChanged);
    _demand.dispose();
  }

  @override
  void didUpdateWidget(covariant DesktopViewer oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.store, store)) {
      _detach(oldWidget.store);
      _keys.clear();
      _attach();
    }
  }

  @override
  void dispose() {
    _detach(store);
    _focus.dispose();
    super.dispose();
  }

  void _sessionChanged() {
    // Control or freshness lost: the host has released everything already.
    if (!store.session.value.inputLive) _keys.clear();
  }

  void _updateCursor() {
    final view = store.readView();
    final geometry = store.geometry.value;
    _surface.currentState?.updateRemoteCursor(
      view == null ||
              !view.cursorVisible ||
              geometry == null ||
              view.generation != geometry.generation ||
              geometry.displayWidth == 0 ||
              geometry.displayHeight == 0
          ? null
          : Offset(
              view.cursorX / geometry.displayWidth,
              view.cursorY / geometry.displayHeight,
            ),
    );
  }

  bool get _interactive =>
      store.session.value.inputLive && store.geometry.value != null;

  bool _send(DesktopInput input) {
    final geometry = store.geometry.value;
    return geometry != null && store.send(input, geometryEpoch: geometry.epoch);
  }

  bool _sequence(List<DesktopInput> inputs) {
    final geometry = store.geometry.value;
    return geometry != null &&
        store.sendSequence(inputs, geometryEpoch: geometry.epoch);
  }

  Future<void> _move(Offset normalized) async {
    final geometry = store.geometry.value;
    if (geometry == null) return;
    final width = geometry.displayWidth, height = geometry.displayHeight;
    _send(
      DesktopInput.move(
        (normalized.dx * width).floor().clamp(0, math.max(0, width - 1)),
        (normalized.dy * height).floor().clamp(0, math.max(0, height - 1)),
      ),
    );
  }

  static RemoteButton _button(String name) => switch (name) {
    'right' => RemoteButton.right,
    'middle' => RemoteButton.middle,
    _ => RemoteButton.left,
  };

  void _click(RemoteButton button) => _sequence([
    DesktopInput.button(button, true),
    DesktopInput.button(button, false),
  ]);

  void _tap(RemoteKey key, {List<RemoteKey> held = const []}) => _sequence([
    for (final modifier in held) DesktopInput.key(modifier, true),
    DesktopInput.key(key, true),
    DesktopInput.key(key, false),
    for (final modifier in held.reversed) DesktopInput.key(modifier, false),
  ]);

  KeyEventResult _keyboard(FocusNode node, KeyEvent event) {
    if (!_interactive) return KeyEventResult.ignored;
    final key = _keyNames[event.logicalKey];
    if (key != null) {
      if (event is KeyUpEvent) {
        _keys.remove(event.logicalKey);
      } else {
        _keys.add(event.logicalKey);
      }
      _send(DesktopInput.key(key, event is! KeyUpEvent));
      return KeyEventResult.handled;
    }
    if (event is! KeyUpEvent &&
        event.character != null &&
        !HardwareKeyboard.instance.isControlPressed &&
        !HardwareKeyboard.instance.isAltPressed &&
        !HardwareKeyboard.instance.isMetaPressed) {
      _send(DesktopInput.text(event.character!));
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  /// Best effort: the G2 smoke checks that the native snapshot is written.
  Future<void> _disconnect(BuildContext context) async {
    String? snapshot;
    try {
      final directory = Directory(
        '${(await getApplicationSupportDirectory()).path}/desktop-thumbnails',
      );
      await directory.create(recursive: true);
      snapshot = '${directory.path}/smoke.thumb';
    } catch (_) {
      // No application directory: disconnect without a thumbnail.
    }
    await store.disconnect(snapshotPath: snapshot);
    if (context.mounted) Navigator.pop(context);
  }

  @override
  Widget build(BuildContext context) {
    final l = context.l10n;
    return ListenableBuilder(
      listenable: Listenable.merge([store.session, store.geometry]),
      builder: (context, _) {
        final session = store.session.value;
        final interactive = _interactive;
        return Scaffold(
          appBar: AppBar(
            title: Text(l.desktopTitle),
            actions: [
              TextButton(
                onPressed: () => _disconnect(context),
                child: Text(l.desktopDisconnect),
              ),
            ],
          ),
          body: SafeArea(
            child: Column(
              children: [
                DesktopConnectionBanner(store: store),
                Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 4,
                  ),
                  child: Wrap(
                    spacing: 8,
                    runSpacing: 4,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    children: [
                      FilledButton.tonal(
                        onPressed: session.controlling
                            ? () => store.control(false)
                            : !session.fresh || !session.controlAllowed
                            ? null
                            : () => store.control(true),
                        child: Text(
                          session.controlling
                              ? l.desktopEndControl
                              : l.desktopRequestControl,
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
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 4,
                  ),
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
                      child: _screen(context, session, interactive),
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
                        onPressed: interactive ? () => _text(context) : null,
                        icon: const Icon(Icons.keyboard_outlined),
                      ),
                      IconButton(
                        tooltip: l.desktopLeftClick,
                        onPressed: interactive
                            ? () => _click(RemoteButton.left)
                            : null,
                        icon: const Icon(Icons.ads_click),
                      ),
                      IconButton(
                        tooltip: l.desktopRightClick,
                        onPressed: interactive
                            ? () => _click(RemoteButton.right)
                            : null,
                        icon: const Icon(Icons.mouse_outlined),
                      ),
                      IconButton(
                        tooltip: l.desktopScrollUp,
                        onPressed: interactive
                            ? () => _send(DesktopInput.wheel(0, -3))
                            : null,
                        icon: const Icon(Icons.arrow_upward),
                      ),
                      IconButton(
                        tooltip: l.desktopScrollDown,
                        onPressed: interactive
                            ? () => _send(DesktopInput.wheel(0, 3))
                            : null,
                        icon: const Icon(Icons.arrow_downward),
                      ),
                      for (final key in const [
                        RemoteKey.escape,
                        RemoteKey.tab,
                        RemoteKey.enter,
                        RemoteKey.backspace,
                      ])
                        TextButton(
                          onPressed: interactive ? () => _tap(key) : null,
                          child: Text(key.wire),
                        ),
                      TextButton(
                        onPressed: interactive
                            ? () => _tap(
                                RemoteKey.a,
                                held: const [RemoteKey.control],
                              )
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
      },
    );
  }

  void _releaseLocalInput() {
    _surface.currentState?.cancelInput();
    if (_keys.isNotEmpty) {
      _keys.clear();
      store.release();
    }
  }

  Widget _screen(
    BuildContext context,
    DesktopSessionView session,
    bool interactive,
  ) => ValueListenableBuilder(
    valueListenable: store.picture,
    builder: (context, picture, _) {
      final geometry = store.geometry.value;
      return Stack(
        fit: StackFit.expand,
        children: [
          DesktopGestureSurface(
            key: _surface,
            aspectRatio: picture?.aspectRatio ?? 16 / 9,
            enabled: interactive,
            trackpad: _trackpad,
            inputEpoch: (store, geometry?.epoch),
            onFocus: _focus.requestFocus,
            onMove: _move,
            onButton: (button, down) async =>
                _send(DesktopInput.button(_button(button), down)),
            onWheel: (delta) async => _send(
              DesktopInput.wheel(
                (delta.dx / 30).round(),
                (delta.dy / 30).round(),
              ),
            ),
            onRelease: () async => store.release(),
            child: picture?.textureId != null
                ? Texture(
                    key: ValueKey(picture!.textureId),
                    textureId: picture.textureId!,
                    filterQuality: FilterQuality.low,
                  )
                : ValueListenableBuilder(
                    valueListenable: store.jpegImage,
                    builder: (context, image, _) => image == null
                        ? const SizedBox.expand()
                        : RawImage(
                            image: image,
                            fit: BoxFit.fill,
                            filterQuality: FilterQuality.low,
                          ),
                  ),
          ),
          if (picture == null)
            IgnorePointer(
              child: Center(
                child: Text(
                  session.phase == DesktopPhase.ended
                      ? context.l10n.desktopStopped
                      : context.l10n.desktopConnecting,
                  style: const TextStyle(color: Colors.white),
                ),
              ),
            )
          else if (!session.fresh)
            // Never cover the native texture with an opaque layer: Flutter
            // must keep consuming it to present the recovered picture.
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
    },
  );

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
    if (text != null && text.isNotEmpty) _send(DesktopInput.text(text));
  }
}

final _keyNames = <LogicalKeyboardKey, RemoteKey>{
  LogicalKeyboardKey.shiftLeft: RemoteKey.shift,
  LogicalKeyboardKey.shiftRight: RemoteKey.shift,
  LogicalKeyboardKey.controlLeft: RemoteKey.control,
  LogicalKeyboardKey.controlRight: RemoteKey.control,
  LogicalKeyboardKey.altLeft: RemoteKey.alt,
  LogicalKeyboardKey.altRight: RemoteKey.alt,
  LogicalKeyboardKey.metaLeft: RemoteKey.meta,
  LogicalKeyboardKey.metaRight: RemoteKey.meta,
  LogicalKeyboardKey.enter: RemoteKey.enter,
  LogicalKeyboardKey.escape: RemoteKey.escape,
  LogicalKeyboardKey.tab: RemoteKey.tab,
  LogicalKeyboardKey.backspace: RemoteKey.backspace,
  LogicalKeyboardKey.delete: RemoteKey.delete,
  LogicalKeyboardKey.arrowLeft: RemoteKey.left,
  LogicalKeyboardKey.arrowRight: RemoteKey.right,
  LogicalKeyboardKey.arrowUp: RemoteKey.up,
  LogicalKeyboardKey.arrowDown: RemoteKey.down,
  LogicalKeyboardKey.home: RemoteKey.home,
  LogicalKeyboardKey.end: RemoteKey.end,
  LogicalKeyboardKey.pageUp: RemoteKey.pageUp,
  LogicalKeyboardKey.pageDown: RemoteKey.pageDown,
  LogicalKeyboardKey.keyA: RemoteKey.a,
  LogicalKeyboardKey.keyC: RemoteKey.c,
  LogicalKeyboardKey.keyV: RemoteKey.v,
  LogicalKeyboardKey.keyX: RemoteKey.x,
  LogicalKeyboardKey.keyZ: RemoteKey.z,
};
