import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../../../state/desktop_input.dart';
import 'remote_input.dart';

/// The soft keyboard's client (ui §10.5.2): turns what the IME commits into
/// keys and text for the host.
///
/// It is not a widget and never takes focus: the picture keeps it, so a
/// hardware keyboard goes through [RemoteInput.handleHardwareKey] and never
/// edits this buffer. The buffer is a sentinel of 64 digits with the caret
/// at the end, so Backspace always yields a deletion, and digits draw no
/// autocorrection. Only committed text reaches the host; text being
/// composed is shown in [composing] and sent when it is committed.
final class RemoteTextInput with TextInputClient, DeltaTextInputClient {
  RemoteTextInput({required RemoteInput input}) : _input = input;

  final RemoteInput _input;

  static final sentinel = TextEditingValue(
    text: '1' * 64,
    selection: const TextSelection.collapsed(offset: 64),
  );

  /// The buffer drifted this far from the sentinel and is reset.
  static const _driftMin = 32, _driftMax = 512;

  static const _configuration = TextInputConfiguration(
    inputType: TextInputType.multiline,
    inputAction: TextInputAction.newline,
    enableDeltaModel: true,
    autocorrect: false,
    enableSuggestions: false,
    enableIMEPersonalizedLearning: false,
    keyboardAppearance: Brightness.dark,
    smartDashesType: SmartDashesType.disabled,
    smartQuotesType: SmartQuotesType.disabled,
  );

  TextInputConnection? _connection;
  TextEditingValue _value = sentinel;
  final _attached = ValueNotifier(false);
  final _composing = ValueNotifier('');

  /// A connection exists and was not closed: the keyboard may be up.
  ValueListenable<bool> get attached => _attached;

  /// Text the IME is composing, not sent yet.
  ValueListenable<String> get composing => _composing;

  /// Attaches if needed and always shows the keyboard, so it comes back
  /// after Android's Back hid it.
  void show() {
    var connection = _connection;
    if (connection == null || !connection.attached) {
      connection = TextInput.attach(this, _configuration);
      _connection = connection;
      _value = sentinel;
      connection.setEditingState(_value);
    }
    _attached.value = true;
    connection.show();
  }

  void hide() {
    _connection?.close();
    _detached();
  }

  void dispose() {
    hide();
    _attached.dispose();
    _composing.dispose();
  }

  void _detached() {
    _connection = null;
    _attached.value = false;
    _composing.value = '';
  }

  // ------------------------------------------------------------ the IME

  @override
  TextEditingValue? get currentTextEditingValue => _value;

  @override
  AutofillScope? get currentAutofillScope => null;

  @override
  void updateEditingValue(TextEditingValue value) => _apply(value);

  @override
  void updateEditingValueWithDeltas(List<TextEditingDelta> textEditingDeltas) {
    for (final delta in textEditingDeltas) {
      TextEditingValue? next;
      if (delta.oldText == _value.text) {
        try {
          next = delta.apply(_value);
        } catch (_) {
          next = null;
        }
      }
      if (next == null) {
        // A delta against a buffer already reset: what it changed cannot be
        // known, so nothing is sent and the IME starts over.
        _resync();
        return;
      }
      _apply(next);
    }
  }

  /// What the committed text gained or lost goes to the host: deletions as
  /// backspaces, insertions as text, a lone newline as Enter.
  void _apply(TextEditingValue next) {
    final before = _committed(_value), after = _committed(next);
    _value = next;
    _composing.value = _composingText(next);
    if (before != after) {
      final (deleted, inserted) = _edit(before, after);
      if (deleted.isNotEmpty) _input.backspace(deleted.runes.length);
      if (inserted == '\n') {
        _input.tapKey(RemoteKey.enter);
      } else if (inserted.isNotEmpty) {
        _input.typeText(inserted);
      }
    }
    final length = next.text.length;
    if (_composing.value.isEmpty &&
        (length < _driftMin || length > _driftMax)) {
      _resync();
    }
  }

  void _resync() {
    _value = sentinel;
    _composing.value = '';
    _connection?.setEditingState(sentinel);
  }

  /// What turned [before] into [after]: the text between their common
  /// prefix and suffix, never splitting a surrogate pair.
  static (String deleted, String inserted) _edit(String before, String after) {
    final shortest = before.length < after.length
        ? before.length
        : after.length;
    var prefix = 0;
    while (prefix < shortest &&
        before.codeUnitAt(prefix) == after.codeUnitAt(prefix)) {
      prefix++;
    }
    if (prefix > 0 && _isHighSurrogate(before.codeUnitAt(prefix - 1))) {
      prefix--;
    }
    var suffix = 0;
    while (suffix < shortest - prefix &&
        before.codeUnitAt(before.length - 1 - suffix) ==
            after.codeUnitAt(after.length - 1 - suffix)) {
      suffix++;
    }
    if (suffix > 0 &&
        _isLowSurrogate(before.codeUnitAt(before.length - suffix))) {
      suffix--;
    }
    return (
      before.substring(prefix, before.length - suffix),
      after.substring(prefix, after.length - suffix),
    );
  }

  static bool _isHighSurrogate(int unit) => unit >= 0xD800 && unit <= 0xDBFF;
  static bool _isLowSurrogate(int unit) => unit >= 0xDC00 && unit <= 0xDFFF;

  /// The buffer without the part being composed.
  static String _committed(TextEditingValue v) {
    final c = v.composing;
    if (!c.isValid || c.isCollapsed || c.end > v.text.length) return v.text;
    return v.text.replaceRange(c.start, c.end, '');
  }

  static String _composingText(TextEditingValue v) {
    final c = v.composing;
    if (!c.isValid || c.isCollapsed || c.end > v.text.length) return '';
    return c.textInside(v.text);
  }

  @override
  void performAction(TextInputAction action) {
    if (action == TextInputAction.newline) _input.tapKey(RemoteKey.enter);
  }

  @override
  void connectionClosed() => _detached();

  @override
  void performPrivateCommand(String action, Map<String, dynamic> data) {}

  @override
  void updateFloatingCursor(RawFloatingCursorPoint point) {}

  @override
  void showAutocorrectionPromptRect(int start, int end) {}
}
