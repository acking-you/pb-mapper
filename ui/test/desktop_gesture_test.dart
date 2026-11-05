import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/desktop_gesture_surface.dart';

class _Input {
  final events = <(String, Object)>[];
  Widget surface({bool trackpad = false, bool enabled = true, int epoch = 1}) =>
      MaterialApp(
        home: Scaffold(
          body: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              width: 400,
              height: 300,
              child: DesktopGestureSurface(
                aspectRatio: 2,
                enabled: enabled,
                trackpad: trackpad,
                inputEpoch: epoch,
                cursor: const Offset(0.5, 0.5),
                onMove: (point) async => events.add(('move', point)),
                onButton: (button, down) async => events.add((button, down)),
                onWheel: (delta) async => events.add(('wheel', delta)),
                onRelease: () async => events.add(('release', true)),
                onFocus: () {},
                child: const Texture(textureId: 17),
              ),
            ),
          ),
        ),
      );
}

void main() {
  testWidgets(
    'touch taps wait for the gesture and two fingers click only right',
    (tester) async {
      final input = _Input();
      await tester.pumpWidget(input.surface());
      final finger = await tester.startGesture(const Offset(100, 150));
      expect(input.events, isEmpty);
      await finger.up();
      await tester.pump();
      expect(input.events, [
        ('move', const Offset(0.25, 0.5)),
        ('left', true),
        ('left', false),
      ]);
      input.events.clear();
      final left = await tester.startGesture(
        const Offset(160, 150),
        pointer: 2,
      );
      final right = await tester.startGesture(
        const Offset(240, 150),
        pointer: 3,
      );
      expect(input.events, isEmpty);
      await left.up();
      expect(input.events, isEmpty);
      await right.up();
      await tester.pump();
      expect(input.events, [
        ('move', const Offset(0.5, 0.5)),
        ('right', true),
        ('right', false),
      ]);
      input.events.clear();
      await tester.pumpWidget(input.surface(enabled: false));
      await tester.tapAt(const Offset(100, 150));
      expect(input.events, isEmpty);
    },
  );

  testWidgets(
    'pinch keeps the texture mounted and maps later taps through zoom',
    (tester) async {
      final input = _Input();
      await tester.pumpWidget(input.surface());
      final texture = tester.element(find.byType(Texture));
      final left = await tester.startGesture(
        const Offset(140, 150),
        pointer: 1,
      );
      final right = await tester.startGesture(
        const Offset(260, 150),
        pointer: 2,
      );
      for (var step = 1; step <= 4; step++) {
        await left.moveTo(Offset(140 - step * 15, 150));
        await right.moveTo(Offset(260 + step * 15, 150));
        await tester.pump();
      }
      await left.up();
      await right.up();
      await tester.pump();
      expect(input.events, isEmpty);
      expect(identical(tester.element(find.byType(Texture)), texture), isTrue);
      final before = tester
          .widget<Transform>(find.byKey(const Key('desktop-scene-transform')))
          .transform
          .clone();
      expect(before.getMaxScaleOnAxis(), greaterThan(1.2));
      const tap = Offset(300, 170);
      final scene = MatrixUtils.transformPoint(Matrix4.inverted(before), tap);
      await tester.tapAt(tap);
      await tester.pump();
      final sent = input.events.first.$2 as Offset;
      expect(sent.dx, closeTo(scene.dx / 400, 0.001));
      expect(sent.dy, closeTo(scene.dy / 200, 0.001));
      expect(input.events.skip(1), [('left', true), ('left', false)]);

      // Reconnect/input generation changes cannot throw away the chosen zoom.
      await tester.pumpWidget(input.surface(epoch: 2));
      final after = tester
          .widget<Transform>(find.byKey(const Key('desktop-scene-transform')))
          .transform;
      expect(after, before);
      expect(identical(tester.element(find.byType(Texture)), texture), isTrue);
    },
  );

  testWidgets(
    'trackpad moves without dragging and releases a held drag on pinch',
    (tester) async {
      final input = _Input();
      await tester.pumpWidget(input.surface(trackpad: true));
      final finger = await tester.startGesture(
        const Offset(100, 150),
        pointer: 1,
      );
      await finger.moveTo(const Offset(140, 150));
      await finger.moveTo(const Offset(180, 150));
      await finger.up();
      await tester.pump();
      expect(input.events.every((event) => event.$1 == 'move'), isTrue);
      expect((input.events.last.$2 as Offset).dx, greaterThan(0.5));
      final movedCursor = input.events.last.$2;
      input.events.clear();
      // A lagging video cursor must not undo the previous trackpad stroke.
      await tester.tapAt(const Offset(100, 150));
      expect(input.events.first, ('move', movedCursor));
      input.events.clear();
      final held = await tester.startGesture(
        const Offset(150, 150),
        pointer: 2,
      );
      await tester.pump(const Duration(milliseconds: 550));
      expect(input.events.where((event) => event.$1 == 'left'), [
        ('left', true),
      ]);
      final pinch = await tester.startGesture(
        const Offset(250, 150),
        pointer: 3,
      );
      expect(input.events.where((event) => event.$1 == 'release'), [
        ('release', true),
      ]);
      await held.cancel();
      await pinch.up();
      await tester.pump();
      expect(input.events.where((event) => event.$1 == 'right'), isEmpty);
      expect(input.events.where((event) => event.$1 == 'release').length, 1);

      input.events.clear();
      final mouse = await tester.createGesture(
        kind: ui.PointerDeviceKind.mouse,
      );
      await mouse.addPointer(location: const Offset(200, 150));
      await mouse.down(const Offset(200, 150));
      await mouse.moveTo(const Offset(220, 150));
      await mouse.up();
      await mouse.removePointer();
      expect(input.events.where((event) => event.$1 == 'left'), [
        ('left', true),
        ('left', false),
      ]);
    },
  );
}
