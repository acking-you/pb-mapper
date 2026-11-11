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
/// the picture sees keys before any input method, and keeps them.
final class DesktopKeyboard {
  DesktopKeyboard({
    required RemoteInput input,
    required FocusNode picture,
    LocalInputMethod? ime,
  }) : _input = input,
       _picture = picture,
       _ime = ime ?? LocalInputMethod() {
    text = RemoteTextInput(input: input);
    input.textInput = text;
    picture.addListener(sync);
    input.ready.addListener(sync);
  }

  final RemoteInput _input;
  final FocusNode _picture;
  final LocalInputMethod _ime;
  late final RemoteTextInput text;

  /// After a change of focus, control or [RemoteInput.keyboard].
  void sync() {
    final focused = _picture.hasFocus;
    final local = _input.keyboard.mode == KeyboardMode.local;
    if (focused && local && _input.ready.value) {
      text.show();
    } else {
      text.hide();
    }
    _ime.enabled = !(focused && !local);
  }

  /// Before the input it was made for.
  void dispose() {
    _picture.removeListener(sync);
    _input.ready.removeListener(sync);
    _input.textInput = null;
    text.dispose();
    _ime.enabled = true;
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
