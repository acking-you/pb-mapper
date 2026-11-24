import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_scroll.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_surface.dart';

import 'fake_desktop_api.dart';
import 'remote_viewer_harness.dart';

const _hires = ['wheel_hires'];
const _at = Offset(200, 150);

/// A live, controlling viewer's surface, 400×300 at the view's top left,
/// over a host that listed [capabilities].
Future<ViewerHarness> _surface(
  WidgetTester tester, {
  List<String> capabilities = const [],
}) async {
  final h = await ViewerHarness.live(tester, capabilities: capabilities);
  await tester.pumpWidget(
    Directionality(
      textDirection: TextDirection.ltr,
      child: Align(
        alignment: Alignment.topLeft,
        child: SizedBox(
          width: 400,
          height: 300,
          child: RemoteSurface(
            viewport: h.viewport,
            input: h.input,
            machine: h.machine,
            touchMode: h.touchMode,
            child: const Texture(textureId: 17),
          ),
        ),
      ),
    ),
  );
  await tester.pump();
  h.sent();
  return h;
}

Future<void> _close(WidgetTester tester, ViewerHarness h) async {
  await tester.pumpWidget(const SizedBox());
  h.dispose();
}

/// The wheels and scrolls the host was sent since the last call, in order.
List<String> _scrolls(ViewerHarness h) => [
  for (final batch in h.sent())
    for (final e in batch)
      if (e.startsWith('wheel') || e.startsWith('scroll')) e,
];

/// What [_scrolls] moved vertically in all.
int _vertical(List<String> scrolls) => scrolls.fold(
  0,
  (sum, s) => sum + int.parse(s.split(',').last.split(' ').first),
);

/// A touchpad's pan of [steps] logical pixels each, fingers up, one every
/// [every] from [from]; the time of the last.
Future<Duration> _pan(
  WidgetTester tester,
  TestPointer pad,
  Iterable<double> steps, {
  Duration from = Duration.zero,
  Duration every = const Duration(milliseconds: 8),
}) async {
  await tester.sendEventToBinding(pad.panZoomStart(_at, timeStamp: from));
  var at = from, pan = Offset.zero;
  for (final step in steps) {
    at += every;
    pan += Offset(0, -step);
    await tester.sendEventToBinding(
      pad.panZoomUpdate(_at, pan: pan, timeStamp: at),
    );
  }
  return at;
}

/// A fast two-finger swipe, 300 px up in 80 ms (3 750 px/s), and its
/// lift; what it scrolled is taken.
Future<void> _swipe(
  WidgetTester tester,
  ViewerHarness h,
  TestPointer pad,
) async {
  final end = await _pan(tester, pad, List.filled(10, 30.0));
  await tester.sendEventToBinding(pad.panZoomEnd(timeStamp: end));
  expect(_vertical(_scrolls(h)), 360);
}

/// The scrolls of each 16 ms frame of a fling, until [frames] passed.
Future<List<int>> _frames(
  WidgetTester tester,
  ViewerHarness h,
  int frames,
) async {
  final each = <int>[];
  for (var i = 0; i < frames; i++) {
    await tester.pump(const Duration(milliseconds: 16));
    each.add(_vertical(_scrolls(h)));
  }
  return each;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  testWidgets(
    'a Windows wheel notch is one notch on the host at 100, 150 and 200 % '
    'and whatever lines a notch scrolls, quarter notches included',
    (tester) async {
      addTearDown(tester.view.reset);
      addTearDown(() => WheelLines.value = kWindowsWheelLines);
      for (final (dpr, lines) in [
        (1.0, 3),
        (1.5, 3),
        (2.0, 3),
        (1.25, 1),
        (1.5, 7),
      ]) {
        tester.view.devicePixelRatio = dpr;
        WheelLines.value = lines;
        // What Flutter's Windows embedder reports for a notch: lines × 100
        // / 3 physical pixels, cut to a whole number, in logical ones.
        final notch = (lines * 100 / 3).truncateToDouble() / dpr;
        for (final hires in [false, true]) {
          final reason = 'at $dpr with $lines lines, hires $hires';
          final h = await _surface(
            tester,
            capabilities: hires ? _hires : const [],
          );
          final mouse = TestPointer(1, PointerDeviceKind.mouse)..hover(_at);
          for (var i = 0; i < 5; i++) {
            await tester.sendEventToBinding(mouse.scroll(Offset(0, notch)));
            expect(_scrolls(h), [
              hires ? 'scroll 0,120' : 'wheel 0,1',
            ], reason: reason);
          }
          await tester.sendEventToBinding(mouse.scroll(Offset(-notch, 0)));
          expect(_scrolls(h), [
            hires ? 'scroll -120,0' : 'wheel -1,0',
          ], reason: reason);
          // A high-resolution wheel's quarter notches: to a host that takes
          // them each as it came, else one notch for the four.
          for (var i = 0; i < 4; i++) {
            await tester.sendEventToBinding(
              mouse.scroll(Offset(0, -notch / 4)),
            );
          }
          expect(
            _scrolls(h),
            hires ? List.filled(4, 'scroll 0,-30') : ['wheel 0,-1'],
            reason: reason,
          );
          await _close(tester, h);
        }
      }
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  testWidgets(
    'a Mac or Linux wheel notch is one notch on the host',
    (tester) async {
      // What their embedders report for a notch, in logical pixels.
      final notch = defaultTargetPlatform == TargetPlatform.macOS ? 40.0 : 53.0;
      for (final hires in [false, true]) {
        final h = await _surface(tester, capabilities: hires ? _hires : []);
        final mouse = TestPointer(1, PointerDeviceKind.mouse)..hover(_at);
        for (var i = 0; i < 5; i++) {
          await tester.sendEventToBinding(mouse.scroll(Offset(0, notch)));
          expect(_scrolls(h), [hires ? 'scroll 0,120' : 'wheel 0,1']);
        }
        // macOS's accelerated wheel: two and a half lines.
        await tester.sendEventToBinding(mouse.scroll(Offset(0, -notch * 2.5)));
        expect(_scrolls(h), [hires ? 'scroll 0,-300' : 'wheel 0,-2']);
        // The half kept toward an older host takes nothing from a notch the
        // other way.
        await tester.sendEventToBinding(mouse.scroll(Offset(0, notch)));
        expect(_scrolls(h), [hires ? 'scroll 0,120' : 'wheel 0,1']);
        await _close(tester, h);
      }
    },
    variant: TargetPlatformVariant({
      TargetPlatform.macOS,
      TargetPlatform.linux,
    }),
  );

  testWidgets(
    "a touchpad's travel goes to a host that takes it as it moves, 1.2 "
    'units a pixel, and to any other in notches of 30 px',
    (tester) async {
      for (final hires in [true, false]) {
        final h = await _surface(tester, capabilities: hires ? _hires : []);
        final pad = TestPointer(2, PointerDeviceKind.trackpad);
        // 300 px in uneven steps, slowly, and still before the lift: no
        // fling follows.
        final steps = [
          7.0,
          13.5,
          2.5,
          30.0,
          17.0,
          5.0,
          25.0,
        ].followedBy(List.filled(20, 10.0));
        final end = await _pan(
          tester,
          pad,
          steps,
          every: const Duration(milliseconds: 30),
        );
        await tester.pump(const Duration(milliseconds: 100));
        await tester.sendEventToBinding(
          pad.panZoomEnd(timeStamp: end + const Duration(milliseconds: 100)),
        );
        final sent = _scrolls(h);
        if (hires) {
          // One continuous scroll for each event of the pan.
          expect(sent, hasLength(steps.length));
          expect(sent.every((s) => s.endsWith(' ~')), isTrue, reason: '$sent');
          expect(_vertical(sent), 360);
        } else {
          expect(sent.every((s) => s.startsWith('wheel 0,')), isTrue);
          expect(_vertical(sent), 10);
        }
        await tester.pump(const Duration(seconds: 1));
        expect(_scrolls(h), isEmpty, reason: 'a slow pan flings nothing');
        await _close(tester, h);
      }
    },
    variant: TargetPlatformVariant.only(TargetPlatform.macOS),
  );

  testWidgets(
    "what a touchpad's travel left short of a step is no part of a wheel's "
    'notch',
    (tester) async {
      for (final hires in [true, false]) {
        final h = await _surface(tester, capabilities: hires ? _hires : []);
        final pad = TestPointer(2, PointerDeviceKind.trackpad);
        final mouse = TestPointer(1, PointerDeviceKind.mouse)..hover(_at);
        // 25.75 px down: 30 units with 0.9 kept, or 0.86 of a notch kept.
        await _pan(tester, pad, [25.75]);
        await tester.sendEventToBinding(pad.panZoomEnd());
        expect(_scrolls(h), hires ? ['scroll 0,30 ~'] : isEmpty);
        // Quarter notches down: each as it came, or one notch with the
        // fourth, not with the first.
        for (var i = 1; i <= 4; i++) {
          await tester.sendEventToBinding(mouse.scroll(const Offset(0, 10)));
          expect(
            _scrolls(h),
            hires
                ? ['scroll 0,30']
                : i == 4
                ? ['wheel 0,1']
                : isEmpty,
            reason: 'quarter $i, hires $hires',
          );
        }
        // A notch the other way is a whole notch, never one short.
        await tester.sendEventToBinding(mouse.scroll(const Offset(0, -40)));
        expect(_scrolls(h), [hires ? 'scroll 0,-120' : 'wheel 0,-1']);
        await _close(tester, h);
      }
    },
    variant: TargetPlatformVariant.only(TargetPlatform.macOS),
  );

  testWidgets(
    'a fast swipe flings on after the lift, decaying as this platform '
    'scrolls, for about as far as its scroll views go',
    (tester) async {
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      final mac = defaultTargetPlatform == TargetPlatform.macOS;
      final h = await _surface(tester, capabilities: _hires);
      final pad = TestPointer(2, PointerDeviceKind.trackpad);
      await _swipe(tester, h, pad);
      final frames = await _frames(tester, h, 250);
      final moving = frames.lastIndexWhere((units) => units != 0) + 1;
      final travel = frames.fold(0, (sum, units) => sum + units) / 1.2;
      // 3 750 px/s: UIKit's deceleration takes it 3 750 / ln(1 / 0.135),
      // about 1 870 px, until it is slower than 10 px/s, after about
      // 3 s; Android's, about 1 930 px, in about 1.2 s.
      if (mac) {
        expect(travel, inInclusiveRange(1800, 1873));
        expect(moving * 16, inInclusiveRange(2500, 3200));
      } else {
        expect(travel, inInclusiveRange(1850, 1950));
        expect(moving * 16, inInclusiveRange(1100, 1300));
      }
      // Each tenth of a second scrolls less than the one before, but for a
      // unit that whole units may move from one to the next.
      final tenths = [
        // From the first tick that moves: the ticker's first is at 0.
        for (var at = 1; at + 6 <= moving; at += 6)
          frames.sublist(at, at + 6).fold(0, (sum, units) => sum + units),
      ];
      for (var i = 1; i < tenths.length; i++) {
        expect(
          tenths[i],
          lessThanOrEqualTo(tenths[i - 1] + 1),
          reason: '$tenths',
        );
      }
      expect(tenths.last, lessThan(tenths.first ~/ 10));
      expect(tenths.first, greaterThan(200));
      await _close(tester, h);
    },
    variant: TargetPlatformVariant({
      TargetPlatform.macOS,
      TargetPlatform.windows,
    }),
  );

  testWidgets('a fling toward a host in notches goes in notches', (
    tester,
  ) async {
    final h = await _surface(tester);
    final pad = TestPointer(2, PointerDeviceKind.trackpad);
    final end = await _pan(tester, pad, List.filled(10, 30.0));
    await tester.sendEventToBinding(pad.panZoomEnd(timeStamp: end));
    expect(_vertical(_scrolls(h)), 10);
    final frames = await _frames(tester, h, 100);
    expect(frames.sublist(1, 4).every((notches) => notches > 0), isTrue);
    // About 1 930 px of fling, 30 px a notch.
    expect(
      frames.fold(0, (sum, units) => sum + units),
      inInclusiveRange(60, 66),
    );
    await _close(tester, h);
  }, variant: TargetPlatformVariant.only(TargetPlatform.linux));

  testWidgets(
    'a fling stops at a touch, a new pan, a click, a wheel, focus or control '
    'lost and a new picture generation, and a pinch flings nothing',
    (tester) async {
      final h = await _surface(tester, capabilities: _hires);
      final pad = TestPointer(2, PointerDeviceKind.trackpad);
      final mouse = TestPointer(1, PointerDeviceKind.mouse)..hover(_at);
      final stops = <String, Future<void> Function()>{
        'touch': () => tester.sendEventToBinding(pad.scrollInertiaCancel()),
        'pan': () async {
          await tester.sendEventToBinding(pad.panZoomStart(_at));
          await tester.sendEventToBinding(pad.panZoomEnd());
        },
        'click': () async {
          await tester.sendEventToBinding(mouse.down(_at));
          await tester.sendEventToBinding(mouse.up());
        },
        'wheel': () =>
            tester.sendEventToBinding(mouse.scroll(const Offset(0, 40))),
        'focus': () async => h.input.releaseAll(),
      };
      for (final MapEntry(key: stop, value: act) in stops.entries) {
        await _swipe(tester, h, pad);
        // The ticker's first frame starts the fling's clock.
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 100));
        expect(_scrolls(h), isNotEmpty, reason: 'flinging before $stop');
        await act();
        // Only the wheel's own notch is sent once it stopped.
        expect(
          _scrolls(h),
          stop == 'wheel' ? ['scroll 0,120'] : isEmpty,
          reason: stop,
        );
        await tester.pump(const Duration(seconds: 1));
        expect(_scrolls(h), isEmpty, reason: 'after $stop');
      }

      // A pinch ends in no fling, however fast.
      await tester.sendEventToBinding(pad.panZoomStart(_at));
      var at = Duration.zero;
      for (var i = 1; i <= 10; i++) {
        at += const Duration(milliseconds: 8);
        await tester.sendEventToBinding(
          pad.panZoomUpdate(
            _at,
            pan: Offset(0, -30.0 * i),
            scale: 1 + i / 10,
            timeStamp: at,
          ),
        );
      }
      await tester.sendEventToBinding(pad.panZoomEnd(timeStamp: at));
      await tester.pump(const Duration(milliseconds: 500));
      expect(_scrolls(h), isEmpty);

      // A new generation: the host's picture is another's.
      await _swipe(tester, h, pad);
      h.api.viewerStatusValue = liveStatus(
        controlling: true,
        generation: 2,
        capabilities: _hires,
      );
      await tester.pump(const Duration(milliseconds: 500));
      expect(h.input.ready.value, isTrue);
      h.sent();
      await tester.pump(const Duration(seconds: 1));
      expect(_scrolls(h), isEmpty, reason: 'after a new generation');

      // Control lost.
      await _swipe(tester, h, pad);
      h.api.viewerStatusValue = liveStatus(
        controlling: false,
        generation: 2,
        capabilities: _hires,
      );
      await tester.pump(const Duration(milliseconds: 500));
      expect(h.input.ready.value, isFalse);
      h.sent();
      h.api.viewerStatusValue = liveStatus(
        controlling: true,
        generation: 2,
        capabilities: _hires,
      );
      await tester.pump(const Duration(milliseconds: 500));
      expect(h.input.ready.value, isTrue);
      await tester.pump(const Duration(seconds: 1));
      expect(_scrolls(h), isEmpty, reason: 'after control was lost');
      await _close(tester, h);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.macOS),
  );

  testWidgets(
    "what fell short of a step toward one host is not counted toward the "
    'next, which may scroll in other steps',
    (tester) async {
      final h = await _surface(tester, capabilities: _hires);
      final pad = TestPointer(2, PointerDeviceKind.trackpad);
      // 0.75 px: 0.9 units, short of one.
      await _pan(tester, pad, [0.75]);
      await tester.sendEventToBinding(pad.panZoomEnd());
      expect(_scrolls(h), isEmpty);
      // A reconnect to an older host, in notches of 30 px.
      h.api.viewerStatusValue = liveStatus(controlling: true, generation: 2);
      await tester.pump(const Duration(milliseconds: 500));
      expect(h.input.ready.value && !h.input.hiresWheel, isTrue);
      // 3 px: a tenth of a notch, which the 0.9 units must not complete.
      await _pan(tester, pad, [3]);
      await tester.sendEventToBinding(pad.panZoomEnd());
      expect(_scrolls(h), isEmpty);
      await _close(tester, h);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.macOS),
  );

  test('a fling is too slow to start below the fling velocity, and keeps '
      'its direction', () {
    expect(
      ScrollFling.start(
        const Velocity(pixelsPerSecond: Offset(30, 30)),
        platform: TargetPlatform.windows,
      ),
      isNull,
    );
    final fling = ScrollFling.start(
      const Velocity(pixelsPerSecond: Offset(-600, 800)),
      platform: TargetPlatform.linux,
    )!;
    final at = fling.offsetAt(const Duration(milliseconds: 200));
    expect(at.dx, lessThan(0));
    expect(at.dy / at.dx, moreOrLessEquals(-4 / 3));
  });
}
