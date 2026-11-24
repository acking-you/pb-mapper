import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/native_video_hole.dart';

/// The hole 10 logical pixels across and 20 down, in a 200 x 100 clip, its
/// 160 x 90 picture moved by [dx], [dy] and scaled by [scale] inside it, as
/// the viewer's surface zooms and pans the picture, under [opacity], and
/// painted unless [offstage]. The tree keeps its shape, so the hole stays
/// one render object.
Widget _stage(
  List<VideoPlacement> placed, {
  double dx = -50,
  double dy = -10,
  double scale = 1.5,
  double opacity = 1,
  bool offstage = false,
  Matrix4? transform,
}) {
  final picture = Opacity(
    opacity: opacity,
    child: Transform(
      transform:
          transform ??
          (Matrix4.diagonal3Values(scale, scale, 1)
            ..setTranslationRaw(dx, dy, 0)),
      child: SizedBox(
        width: 160,
        height: 90,
        child: NativeVideoHole(onPlace: placed.add),
      ),
    ),
  );
  return Directionality(
    textDirection: TextDirection.ltr,
    child: Align(
      alignment: Alignment.topLeft,
      child: Padding(
        padding: const EdgeInsets.only(left: 10, top: 20),
        child: SizedBox(
          width: 200,
          height: 100,
          child: ClipRect(
            child: Offstage(
              offstage: offstage,
              child: OverflowBox(
                alignment: Alignment.topLeft,
                minWidth: 0,
                minHeight: 0,
                maxWidth: 400,
                maxHeight: 400,
                child: Align(alignment: Alignment.topLeft, child: picture),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

void main() {
  testWidgets('the hole clears its rectangle in a layer of its own that is '
      'never raster-cached', (tester) async {
    final placed = <VideoPlacement>[];
    await tester.pumpWidget(_stage(placed));
    final hole = tester.renderObject<RenderNativeVideoHole>(
      find.byType(NativeVideoHole),
    );
    expect(hole.isRepaintBoundary, isTrue);
    expect(
      hole,
      paints..something((method, arguments) {
        if (method != #drawRect) return false;
        final paint = arguments[1] as Paint;
        return arguments[0] == const Rect.fromLTWH(0, 0, 160, 90) &&
            paint.blendMode == BlendMode.clear;
      }),
    );
    final picture = hole.debugLayer!.firstChild! as PictureLayer;
    expect(picture.willChangeHint, isTrue);
  });

  testWidgets('the plane is placed where the hole lands, in physical pixels '
      'and cut to the clip, once a frame moves it', (tester) async {
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.resetDevicePixelRatio);
    final placed = <VideoPlacement>[];
    await tester.pumpWidget(_stage(placed));
    // The picture: (10 - 50, 20 - 10) and 1.5 x 160 by 1.5 x 90 logical
    // pixels; the clip: 200 x 100 from (10, 20); twice each in pixels.
    expect(placed, [
      const VideoPlacement(
        visible: true,
        picture: Rect.fromLTRB(-80, 20, 400, 290),
        clip: Rect.fromLTRB(20, 40, 400, 240),
      ),
    ]);

    // A frame that moves nothing places nothing again.
    await tester.pumpWidget(_stage(placed));
    await tester.pump();
    expect(placed, hasLength(1));

    // A pan moves only the transform above the hole's own layer, which it
    // does not repaint for: the composite still places it anew.
    await tester.pumpWidget(_stage(placed, dx: -20.2, dy: 0));
    expect(placed.last.picture, const Rect.fromLTRB(-20, 40, 460, 310));
    expect(placed.last.clip, const Rect.fromLTRB(20, 40, 420, 240));
    expect(placed, hasLength(2));
    expect(placed.last.toChannel(7), {
      'textureId': 7,
      'visible': true,
      'x': -20,
      'y': 40,
      'w': 480,
      'h': 270,
      'clipX': 20,
      'clipY': 40,
      'clipW': 400,
      'clipH': 200,
    });
  });

  testWidgets('the plane hides when the hole is not composited, or only '
      'through an opacity or a rotation, and shows again', (tester) async {
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetDevicePixelRatio);
    final placed = <VideoPlacement>[];
    await tester.pumpWidget(_stage(placed));
    expect(placed.single.visible, isTrue);

    // A route transition's fade: the hole would clear only the opacity's
    // own layer.
    await tester.pumpWidget(_stage(placed, opacity: 0.5));
    expect(placed.last, VideoPlacement.hidden);
    // Fully opaque, an opacity composites its children as they are.
    await tester.pumpWidget(_stage(placed, opacity: 1));
    expect(placed.last.visible, isTrue);
    expect(placed, hasLength(3));

    // Painted nowhere: its layer leaves the scene.
    await tester.pumpWidget(_stage(placed, offstage: true));
    expect(placed.last, VideoPlacement.hidden);
    await tester.pumpWidget(_stage(placed));
    expect(placed.last.visible, isTrue);

    await tester.pumpWidget(_stage(placed, transform: Matrix4.rotationZ(0.1)));
    expect(placed.last, VideoPlacement.hidden);
    await tester.pumpWidget(_stage(placed));
    expect(placed.last.visible, isTrue);

    // Gone from the tree.
    final count = placed.length;
    await tester.pumpWidget(const SizedBox.shrink());
    expect(placed.last, VideoPlacement.hidden);
    expect(placed, hasLength(count + 1));
  });

  test('a create reply names the texture, and whether the plane shows it', () {
    expect(createdTexture(5), (textureId: 5, plane: false));
    expect(createdTexture({'textureId': 5, 'plane': true}), (
      textureId: 5,
      plane: true,
    ));
    expect(createdTexture({'textureId': 5}), (textureId: 5, plane: false));
    expect(createdTexture(-1), isNull);
    expect(createdTexture({'plane': true}), isNull);
    expect(createdTexture(null), isNull);
    expect(presenterFor('plane', macOS: false, linux: true), 'plane');
    expect(presenterFor('plane', macOS: true, linux: false), 'texture');
    expect(presenterFor('layer', macOS: true, linux: false), 'layer');
    expect(presenterFor('layer', macOS: false, linux: true), 'texture');
    expect(presenterFor('texture', macOS: false, linux: true), 'texture');
  });

  testWidgets('a texture on the plane places and greys it over the channel; '
      'one without places nothing', (tester) async {
    const channel = MethodChannel('pb_mapper/desktop_video');
    final calls = <MethodCall>[];
    var plane = true;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      calls.add(call);
      return call.method == 'create' ? {'textureId': 5, 'plane': plane} : null;
    });
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        channel,
        null,
      ),
    );
    final texture = await createDesktopVideoTexture(instanceToken: 1);
    expect(texture.textureId, 5);
    expect(texture.plane, isTrue);
    await texture.place(
      const VideoPlacement(
        visible: true,
        picture: Rect.fromLTRB(0, 10, 1280, 730),
        clip: Rect.fromLTRB(0, 10, 1000, 700),
      ),
    );
    await texture.place(VideoPlacement.hidden);
    await texture.grey(true);
    expect(calls.map((call) => call.method), [
      'create',
      'place',
      'place',
      'grey',
    ]);
    expect(calls[1].arguments, {
      'textureId': 5,
      'visible': true,
      'x': 0,
      'y': 10,
      'w': 1280,
      'h': 720,
      'clipX': 0,
      'clipY': 10,
      'clipW': 1000,
      'clipH': 690,
    });
    expect(calls[2].arguments, {'textureId': 5, 'visible': false});
    expect(calls[3].arguments, {'textureId': 5, 'on': true});

    plane = false;
    calls.clear();
    final flutter = await createDesktopVideoTexture(instanceToken: 1);
    expect(flutter.plane, isFalse);
    await flutter.place(VideoPlacement.hidden);
    expect(calls.map((call) => call.method), ['create']);
  });
}
