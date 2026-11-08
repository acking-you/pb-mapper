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
}
