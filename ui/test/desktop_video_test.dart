import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';

void main() {
  testWidgets('the layer presenter and the Linux plane take an enhancement '
      'level, as enhance {textureId, level}; a texture sends nothing', (
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

    // The plane scales the picture itself, so it sharpens it too.
    calls.clear();
    final plane = channelVideoTexture(9, plane: true);
    expect(plane.enhances, isTrue);
    await plane.enhance(PictureEnhancement.low);
    expect(calls.single.method, 'enhance');
    expect(calls.single.arguments, {'textureId': 9, 'level': 'low'});

    calls.clear();
    final texture = channelVideoTexture(8);
    expect(texture.enhances, isFalse);
    await texture.enhance(PictureEnhancement.high);
    // Disposed: the runner has nothing left to enhance.
    await layer.dispose();
    await layer.enhance(PictureEnhancement.high);
    await plane.dispose();
    await plane.enhance(PictureEnhancement.high);
    expect(calls.map((call) => call.method), ['dispose', 'dispose']);
  });
}
