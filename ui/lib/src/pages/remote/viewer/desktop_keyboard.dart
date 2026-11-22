import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../../../state/viewer_prefs.dart';
import 'remote_input.dart';
import 'remote_text_input.dart';
import 'remote_viewport.dart';
import 'viewer_overlays.dart';

/// The desktop viewer's keyboard (ui §10.5). In local mode the text-input
/// client is attached while the picture has the keyboard and control, so
/// this computer's input method composes and its text reaches the host. In
/// remote mode, on Windows, the window's input method is off for as long as
/// the picture has the keyboard, so keys reach the host whole: elsewhere
/// the picture sees keys before any input method, and keeps them. While the
/// picture also fills the screen, this computer's own shortcuts go to the
/// host alone ([SystemKeyCapture]).
final class DesktopKeyboard {
  DesktopKeyboard({
    required RemoteInput input,
    required FocusNode picture,
    LocalInputMethod? ime,
    SystemKeyCapture? capture,
  }) : _input = input,
       _picture = picture,
       _ime = ime ?? LocalInputMethod(),
       _capture = capture ?? SystemKeyCapture() {
    text = RemoteTextInput(input: input);
    input.textInput = text;
    _capture.onKey = input.handleCapturedKey;
    text.composing.addListener(_composingChanged);
    picture.addListener(sync);
    input.ready.addListener(sync);
  }

  final RemoteInput _input;
  final FocusNode _picture;
  final LocalInputMethod _ime;
  final SystemKeyCapture _capture;
  late final RemoteTextInput text;

  /// The viewer fills the screen.
  bool get fullscreen => _fullscreen;
  bool _fullscreen = false;
  set fullscreen(bool value) {
    if (value == _fullscreen) return;
    _fullscreen = value;
    sync();
  }

  /// This app's window has the keyboard. Lost, it may never see the keys
  /// the capture holds go up (this computer locks, say), so the capture
  /// stops, and their ups go with it.
  bool get active => _active;
  bool _active = true;
  set active(bool value) {
    if (value == _active) return;
    _active = value;
    sync();
  }

  /// After a change of focus, control, [fullscreen], [active] or
  /// [RemoteInput.keyboard].
  void sync() {
    final focused = _picture.hasFocus;
    final local = _input.keyboard.mode == KeyboardMode.local;
    if (focused && local && _input.ready.value) {
      final context = _picture.context;
      text.viewId = context == null ? null : View.maybeOf(context)?.viewId;
      text.show();
    } else {
      text.hide();
    }
    _ime.enabled = !(focused && !local);
    _capture.captured = focused && _fullscreen && _active && _input.ready.value;
    if (!_capture.captured) _input.captureStopped();
  }

  void _composingChanged() =>
      _capture.composing = text.composing.value.isNotEmpty;

  /// Before the input it was made for.
  void dispose() {
    _picture.removeListener(sync);
    _input.ready.removeListener(sync);
    text.composing.removeListener(_composingChanged);
    _input.textInput = null;
    text.dispose();
    _ime.enabled = true;
    _capture.dispose();
    _input.captureStopped();
  }
}

/// This computer's own shortcuts, kept from its system while the picture
/// fills the screen with the keyboard and control, so they act on the host
/// alone (ui §10.5): the Windows key and Alt, each with whatever is pressed
/// while it is held, Ctrl+Esc, F10 (unless this computer's input method
/// composes) and Print Screen. The Windows runner's keyboard hook takes them
/// (`pb_mapper/key_capture`) and hands each here as the picture would have
/// had it. Every other key reaches the picture as before, so this computer's
/// input method still composes; Ctrl+Alt+Del and Win+L, which Windows
/// handles before any hook, act on this computer too. Off elsewhere than
/// Windows.
class SystemKeyCapture {
  SystemKeyCapture() {
    if (_supported) channel.setMethodCallHandler(_handle);
  }

  static const channel = MethodChannel('pb_mapper/key_capture');

  static bool get _supported =>
      !kIsWeb && defaultTargetPlatform == TargetPlatform.windows;

  /// Each key taken, down, repeated and up; ups keep coming for the keys
  /// still held once capture stops.
  ValueChanged<KeyEvent>? onKey;

  bool _captured = false;
  bool _disposed = false;

  bool get captured => _captured;
  set captured(bool value) {
    if (value == _captured || _disposed) return;
    _captured = value;
    if (!_supported) return;
    unawaited(
      channel.invokeMethod<void>('setCaptured', value).catchError((
        Object error,
      ) {
        debugPrint("This computer's shortcuts could not be captured: $error");
      }),
    );
  }

  /// This computer's input method composes: F10, one of its own keys (a
  /// Japanese one's half-width letters), stays with it.
  bool get composing => _composing;
  bool _composing = false;
  set composing(bool value) {
    if (value == _composing || _disposed) return;
    _composing = value;
    if (!_supported) return;
    unawaited(
      channel.invokeMethod<void>('setComposing', value).catchError((
        Object error,
      ) {
        debugPrint('The input method state could not be passed on: $error');
      }),
    );
  }

  /// When the viewer closes: capture stops, and nothing more comes.
  void dispose() {
    captured = false;
    _disposed = true;
    onKey = null;
    if (_supported) channel.setMethodCallHandler(null);
  }

  Future<void> _handle(MethodCall call) async {
    if (call.method != 'key') {
      throw MissingPluginException('pb_mapper/key_capture: ${call.method}');
    }
    final event = eventOf(call.arguments);
    if (event != null) onKey?.call(event);
  }

  /// The key event the picture would have had for a key the runner took
  /// (`{vk, scan, down, repeat}`), named by Flutter's own Windows tables as
  /// its embedder names the keys it gets, and as it does by Windows' own
  /// codes where they name none (the ISO `<>` key's code, say): the runner
  /// has kept the key from everything else.
  static KeyEvent? eventOf(Object? arguments) {
    if (arguments is! Map) return null;
    final scan = arguments['scan'], vk = arguments['vk'];
    if (scan is! int || vk is! int) return null;
    final physical =
        kWindowsToPhysicalKey[scan] ??
        PhysicalKeyboardKey(scan | LogicalKeyboardKey.windowsPlane);
    final logical =
        kWindowsToLogicalKey[vk] ??
        LogicalKeyboardKey(vk | LogicalKeyboardKey.windowsPlane);
    if (arguments['down'] != true) {
      return KeyUpEvent(
        physicalKey: physical,
        logicalKey: logical,
        timeStamp: Duration.zero,
      );
    }
    if (arguments['repeat'] == true) {
      return KeyRepeatEvent(
        physicalKey: physical,
        logicalKey: logical,
        timeStamp: Duration.zero,
      );
    }
    return KeyDownEvent(
      physicalKey: physical,
      logicalKey: logical,
      timeStamp: Duration.zero,
    );
  }
}

/// This window's own input method, which the Windows runner turns off and
/// on (`pb_mapper/ime`, `ImmAssociateContextEx`). It is always turned back
/// on: when the picture loses the keyboard, the mode changes or the viewer
/// closes, and by the runner when the window goes.
class LocalInputMethod {
  static const _channel = MethodChannel('pb_mapper/ime');

  bool _enabled = true;

  bool get enabled => _enabled;
  set enabled(bool value) {
    if (value == _enabled) return;
    _enabled = value;
    if (kIsWeb || defaultTargetPlatform != TargetPlatform.windows) return;
    unawaited(
      _channel.invokeMethod<void>('setEnabled', value).catchError((
        Object error,
      ) {
        debugPrint('The input method could not be switched: $error');
      }),
    );
  }
}

/// What this computer's input method composes, shown at the remote cursor
/// while it composes, with the input method's candidates placed there too.
/// It fills the picture and takes no pointer.
class RemoteComposition extends StatefulWidget {
  const RemoteComposition({
    super.key,
    required this.text,
    required this.input,
    required this.viewport,
  });

  final RemoteTextInput text;
  final RemoteInput input;
  final RemoteViewport viewport;

  @override
  State<RemoteComposition> createState() => _RemoteCompositionState();
}

class _RemoteCompositionState extends State<RemoteComposition> {
  /// Once the cursor or the picture stops moving, the candidates move.
  static const _settle = Duration(milliseconds: 100);

  late Listenable _moves;
  Timer? _settling;

  @override
  void initState() {
    super.initState();
    _listen();
  }

  @override
  void didUpdateWidget(covariant RemoteComposition oldWidget) {
    super.didUpdateWidget(oldWidget);
    _unlisten(oldWidget);
    _listen();
  }

  @override
  void dispose() {
    _settling?.cancel();
    _unlisten(widget);
    super.dispose();
  }

  void _listen() {
    _moves = Listenable.merge([widget.viewport, widget.input.cursorChanges]);
    _moves.addListener(_moved);
    widget.text.attached.addListener(_place);
    widget.text.composing.addListener(_place);
  }

  void _unlisten(RemoteComposition old) {
    _moves.removeListener(_moved);
    old.text.attached.removeListener(_place);
    old.text.composing.removeListener(_place);
  }

  void _moved() {
    _settling?.cancel();
    if (widget.text.attached.value) _settling = Timer(_settle, _place);
  }

  Offset get _at =>
      widget.viewport.toLocal(widget.input.shownCursor ?? widget.input.cursor);

  /// After this frame's layout, where the box is.
  void _place() {
    if (!widget.text.attached.value) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final box = mounted ? context.findRenderObject() : null;
      if (box is RenderBox) {
        widget.text.placeAt(box, Rect.fromLTWH(_at.dx, _at.dy, 1, 20));
      }
    });
    WidgetsBinding.instance.scheduleFrame();
  }

  @override
  Widget build(BuildContext context) => IgnorePointer(
    child: ValueListenableBuilder(
      valueListenable: widget.text.composing,
      builder: (context, composing, _) => composing.isEmpty
          ? const SizedBox.expand()
          : ListenableBuilder(
              listenable: _moves,
              builder: (context, _) => CustomSingleChildLayout(
                delegate: _BesideCursor(_at),
                child: ViewerPill(height: 32, text: composing),
              ),
            ),
    ),
  );
}

/// Below and right of the cursor, kept inside the picture.
class _BesideCursor extends SingleChildLayoutDelegate {
  const _BesideCursor(this.cursor);

  final Offset cursor;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) =>
      constraints.loosen();

  @override
  Offset getPositionForChild(Size size, Size child) => Offset(
    (cursor.dx + 12).clamp(0, (size.width - child.width).clamp(0, size.width)),
    (cursor.dy + 22).clamp(
      0,
      (size.height - child.height).clamp(0, size.height),
    ),
  );

  @override
  bool shouldRelayout(_BesideCursor old) => old.cursor != cursor;
}
