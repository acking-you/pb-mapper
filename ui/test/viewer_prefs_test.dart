import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:shared_preferences/shared_preferences.dart';

void main() {
  test(
    'viewer prefs start pinned in trackpad mode and survive a relaunch',
    () async {
      SharedPreferences.setMockInitialValues({
        // A value from a build that knew another mode falls back.
        'viewer_touch_mode': 'stylus',
      });
      final first = ViewerPrefs();
      await first.load();
      expect(first.toolbarPinned, isTrue);
      expect(first.touchMode, TouchMode.trackpad);
      expect(first.scaleMode, ViewerScaleMode.fit);
      expect(first.codec, DesktopCodecPreference.auto);
      expect(first.qualityMonitor, isFalse);

      await first.setToolbarPinned(false);
      await first.setTouchMode(TouchMode.direct);
      await first.setScaleMode(ViewerScaleMode.original);
      await first.setCodec(DesktopCodecPreference.h264);
      await first.setQualityMonitor(true);

      final relaunched = ViewerPrefs();
      await relaunched.load();
      expect(relaunched.toolbarPinned, isFalse);
      expect(relaunched.touchMode, TouchMode.direct);
      expect(relaunched.scaleMode, ViewerScaleMode.original);
      expect(relaunched.codec, DesktopCodecPreference.h264);
      expect(relaunched.qualityMonitor, isTrue);
    },
  );

  test('the keyboard is chosen per computer, and one not remembered keeps it '
      'only until the app closes', () async {
    SharedPreferences.setMockInitialValues({});
    final first = ViewerPrefs();
    await first.load();
    expect(first.keyboardFor('a' * 32), const KeyboardSettings());
    const remote = KeyboardSettings(mode: KeyboardMode.remote, swap: false);
    await first.setKeyboard('a' * 32, remote);
    await first.setKeyboard(null, remote);
    expect(first.keyboardFor('b' * 32), const KeyboardSettings());
    expect(first.keyboardFor(null), remote);

    final relaunched = ViewerPrefs();
    await relaunched.load();
    expect(relaunched.keyboardFor('a' * 32), remote);
    expect(relaunched.keyboardFor(null), const KeyboardSettings());

    // Bounded: the oldest choice goes first.
    for (var i = 0; i < ViewerPrefs.keyboardsKept; i++) {
      await relaunched.setKeyboard('$i', remote);
    }
    expect(relaunched.keyboardFor('a' * 32), const KeyboardSettings());
    expect(relaunched.keyboardFor('0'), remote);
  });
}
