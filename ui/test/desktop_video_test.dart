import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';

void main() {
  testWidgets('only the layer presenter takes an enhancement level, as '
      'enhance {textureId, level}; a texture or a plane sends nothing', (
    tester,
  ) async {
    const channel = MethodChannel('pb_mapper/desktop_video');
    final calls = <MethodCall>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      calls.add(call);
      return null;
    });
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        channel,
        null,
      ),
    );

    final layer = channelVideoTexture(
      7,
      platformViewType: desktopVideoViewType,
    );
    expect(layer.enhances, isTrue);
    await layer.enhance(PictureEnhancement.medium);
    await layer.enhance(PictureEnhancement.off);
    expect(calls.map((call) => call.method), ['enhance', 'enhance']);
    expect(calls[0].arguments, {'textureId': 7, 'level': 'medium'});
    expect(calls[1].arguments, {'textureId': 7, 'level': 'off'});

    calls.clear();
    for (final other in [
      channelVideoTexture(8),
      channelVideoTexture(9, plane: true),
    ]) {
      expect(other.enhances, isFalse);
      await other.enhance(PictureEnhancement.high);
    }
    // Disposed: the runner has nothing left to enhance.
    await layer.dispose();
    await layer.enhance(PictureEnhancement.high);
    expect(calls.map((call) => call.method), ['dispose']);
  });
}
