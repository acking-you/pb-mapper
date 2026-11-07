import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_text_input.dart';
import 'package:pb_mapper_ui/src/state/desktop_input.dart';

import 'remote_viewer_harness.dart';

const _none = TextRange.empty;

TextSelection _caret(int offset) => TextSelection.collapsed(offset: offset);

void main() {
  testWidgets(
    'IME deltas: backspace becomes keys, composing waits for its commit, newline is Enter, and drift resets the sentinel',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      final ime = RemoteTextInput(input: h.input);
      final sentinel = RemoteTextInput.sentinel.text;
      ime.show();
      expect(ime.attached.value, isTrue);
      expect(tester.testTextInput.editingState?['text'], sentinel);
      h.sent();

      // Backspace is a key.
      ime.updateEditingValueWithDeltas([
        TextEditingDeltaDeletion(
          oldText: sentinel,
          deletedRange: const TextRange(start: 63, end: 64),
          selection: _caret(63),
          composing: _none,
        ),
      ]);
      expect(h.sent(), [
        ['backspace↓', 'backspace↑'],
      ]);

      // Text being composed waits for its commit.
      var text = sentinel.substring(0, 63);
      ime.updateEditingValueWithDeltas([
        TextEditingDeltaInsertion(
          oldText: text,
          textInserted: 'ni',
          insertionOffset: 63,
          selection: _caret(65),
          composing: const TextRange(start: 63, end: 65),
        ),
      ]);
      expect(h.sent(), isEmpty);
      expect(ime.composing.value, 'ni');
      ime.updateEditingValueWithDeltas([
        TextEditingDeltaReplacement(
          oldText: '${text}ni',
          replacementText: '你',
          replacedRange: const TextRange(start: 63, end: 65),
          selection: _caret(64),
          composing: _none,
        ),
      ]);
      expect(h.sent(), [
        ['text "你"'],
      ]);
      expect(ime.composing.value, isEmpty);

      // A newline is Enter.
      text = '$text你';
      ime.updateEditingValueWithDeltas([
        TextEditingDeltaInsertion(
          oldText: text,
          textInserted: '\n',
          insertionOffset: 64,
          selection: _caret(65),
          composing: _none,
        ),
      ]);
      expect(h.sent(), [
        ['enter↓', 'enter↑'],
      ]);

      // A 64-character deletion: 8 sequences within the host's batch, and
      // the drifted buffer goes back to the sentinel.
      text = '$text\n';
      ime.updateEditingValueWithDeltas([
        TextEditingDeltaDeletion(
          oldText: text,
          deletedRange: TextRange(start: 1, end: text.length),
          selection: _caret(1),
          composing: _none,
        ),
      ]);
      final deletion = h.sent();
      expect(deletion, hasLength(8));
      expect(deletion.every((batch) => batch.length <= 16), isTrue);
      expect(
        deletion.expand((batch) => batch).where((e) => e == 'backspace↓'),
        hasLength(64),
      );
      expect(tester.testTextInput.editingState?['text'], sentinel);

      // With Shift locked, a 40-character commit keys 16 characters and
      // sends the rest as text: the burst the host's queue is sized on.
      h.latch
        ..tap(RemoteKey.shift)
        ..tap(RemoteKey.shift);
      const commit = 'abcdefghijklmnopqrstuvwxyzabcdefghijklmn';
      ime.updateEditingValueWithDeltas([
        TextEditingDeltaInsertion(
          oldText: sentinel,
          textInserted: commit,
          insertionOffset: 64,
          selection: _caret(104),
          composing: _none,
        ),
      ]);
      final burst = h.sent();
      expect(burst, hasLength(17));
      expect(burst.first, ['shift↓', 'a↓', 'a↑', 'shift↑']);
      expect(burst[15], ['shift↓', 'p↓', 'p↑', 'shift↑']);
      expect(burst.last, ['text "${commit.substring(16)}"']);
      expect(h.latch.stateOf(RemoteKey.shift), LatchState.locked);

      ime.dispose();
      h.dispose();
    },
  );
}
