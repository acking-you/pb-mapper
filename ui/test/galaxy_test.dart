import 'dart:ffi' show DynamicLibrary;
import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_backdrop.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_frame.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

Tunnel _share(String key, {String status = 'running'}) =>
    Tunnel.fromService(fakeService(serviceKey: key, status: status));

Tunnel _connect(String key, {String status = 'running'}) =>
    Tunnel.fromClient(fakeClient(serviceKey: key, status: status));

GalaxyScene _scene({int shares = 3, int remote = 4}) => buildScene(
  shares: [for (var i = 0; i < shares; i++) _share('share-$i')],
  connections: [
    _connect('conn-a'),
    _connect('conn-b', status: 'stopped'),
  ],
  relayAddress: 'relay.example:7666',
  relayOnline: true,
  remote: [for (var i = 0; i < remote; i++) 'remote-$i'],
  homeLabel: 'This computer',
);

Float64List _encode(GalaxyScene scene, {Map<String, double>? appeared}) =>
    encodeScene(
      GalaxyBodies(scene),
      stars: Starfield().data,
      systemLabel: (s) => Size(s.label.length * 7.0, 15),
      planetLabel: (p, {required strong}) =>
          Size(p.key.length * (strong ? 7.5 : 6.6), 14),
      appeared: appeared ?? const {},
      rippled: const {},
    );

Float64List _frame({
  double yaw = 0.6,
  double tilt = 0.52,
  double distance = 6,
  double time = 3,
  bool still = false,
  int selected = -1,
}) {
  final frame = Float64List(galaxyFrameFields);
  encodeFrame(
    frame,
    size: const Size(1100, 760),
    yaw: yaw,
    tilt: tilt,
    distance: distance,
    target: Vec3.zero,
    time: time,
    still: still,
    selected: selected,
    hovered: -1,
    focused: -1,
  );
  return frame;
}

/// The debug or release FFI library cargo built, if there is one.
String? _builtLibrary() {
  final name = Platform.isWindows
      ? 'pb_mapper_ffi.dll'
      : Platform.isMacOS
      ? 'libpb_mapper_ffi.dylib'
      : 'libpb_mapper_ffi.so';
  for (final profile in ['release', 'debug']) {
    final file = File('../target/$profile/$name');
    if (file.existsSync()) return file.absolute.path;
  }
  return null;
}

void main() {
  group('scene', () {
    test('without a relay there is only this computer', () {
      final scene = buildScene(
        shares: [_share('web')],
        connections: const [],
        relayAddress: null,
        relayOnline: false,
        remote: const [],
        homeLabel: 'This computer',
      );
      expect(scene.systems.map((s) => s.id), [StarSystem.homeId]);
      expect(scene.links, isEmpty);
      expect(scene.planetOf('share:web')?.planet.kind, BodyKind.share);
    });

    test('live tunnels are linked to the relay, stopped ones are not', () {
      final scene = _scene();
      expect(scene.systems.map((s) => s.id), [
        StarSystem.homeId,
        StarSystem.relayId,
      ]);
      expect(scene.links.map((l) => l.planetId).toSet(), {
        'share:share-0',
        'share:share-1',
        'share:share-2',
        'connect:conn-a',
      });
      expect(
        scene.links.every((l) => l.systemId == StarSystem.relayId),
        isTrue,
      );
    });

    test('the two stars never overlap, however many planets they hold', () {
      for (final count in [0, 1, 6, 13, 40]) {
        final scene = _scene(shares: count, remote: count);
        final home = scene.systemOf(StarSystem.homeId)!;
        final relay = scene.systemOf(StarSystem.relayId)!;
        final gap = (home.position - relay.position).length;
        expect(gap, greaterThan(home.extent + relay.extent), reason: '$count');
      }
    });

    test('a planet keeps its place between builds', () {
      final a = _scene().planetOf('remote:remote-2');
      final b = _scene(shares: 5).planetOf('remote:remote-2');
      expect(a?.planet.phase, b?.planet.phase);
      expect(a?.planet.radius, b?.planet.radius);
    });

    test('rings hold six bodies, the outer ones turning slower', () {
      final home = _scene(shares: 13).systemOf(StarSystem.homeId)!;
      final rings = home.planets.map((p) => p.ring).toSet();
      expect(rings.length, 4); // 6 + 6 + 1 shares, then the connections.
      final inner = home.planets.firstWhere((p) => p.ring == 0);
      final outer = home.planets.firstWhere((p) => p.ring == 3);
      expect(outer.period, greaterThan(inner.period));
    });
  });

  group('frame in Dart', () {
    test('a whole frame is drawn, and a short buffer learns what it needs', () {
      final scene = _encode(_scene());
      final small = Float32List(16);
      final needed = computeFrameInDart(scene, _frame(), small);
      expect(needed, lessThan(-16));
      final out = Float32List(-needed);
      expect(computeFrameInDart(scene, _frame(), out), -needed);
    });

    test('a malformed scene or frame draws nothing', () {
      final scene = _encode(_scene());
      final out = Float32List(1 << 16);
      final wrongVersion = Float64List.fromList(scene)..[0] = 2;
      expect(computeFrameInDart(wrongVersion, _frame(), out), -1);
      expect(computeFrameInDart(scene, _frame(distance: 0), out), -1);
      final truncated = Float64List.sublistView(scene, 0, 20);
      expect(computeFrameInDart(truncated, _frame(), out), -1);
    });

    test('the frame buffer computes in Dart without the library', () {
      GalaxyNative.reset();
      final buffer = GalaxyFrameBuffer();
      addTearDown(buffer.dispose);
      buffer.frame.setAll(0, _frame());
      final values = buffer.compute(_encode(_scene()));
      expect(values, isNotNull);
      expect(values!.length, greaterThan(100));
    });
  });

  // The Rust geometry must draw what the Dart mirror draws. Runs against the
  // library cargo built (`cargo build -p pb-mapper-ffi`); skipped without one.
  final library = _builtLibrary();
  test(
    'Rust and Dart compute the same frames',
    () {
      GalaxyNative.use(DynamicLibrary.open(library!));
      addTearDown(GalaxyNative.reset);
      final random = math.Random(7);
      for (var i = 0; i < 16; i++) {
        final scene = _encode(
          _scene(shares: random.nextInt(14), remote: random.nextInt(20)),
          appeared: {'share:share-0': 2.5},
        );
        final frame = _frame(
          yaw: random.nextDouble() * math.pi * 2,
          tilt: 0.2 + random.nextDouble(),
          distance: 3 + random.nextDouble() * 8,
          time: random.nextDouble() * 60,
          still: i % 5 == 0,
          selected: i % 3 == 0 ? random.nextInt(4) : -1,
        );
        final native = GalaxyFrameBuffer();
        native.frame.setAll(0, frame);
        final fromRust = Float32List.fromList(native.compute(scene)!);
        native.dispose();
        final dart = Float32List(1 << 18);
        final written = computeFrameInDart(scene, frame, dart);
        expect(fromRust.length, written, reason: 'frame $i length');
        for (var v = 0; v < written; v++) {
          final a = fromRust[v], b = dart[v];
          final tolerance = 1e-3 * math.max(1, b.abs());
          expect(
            (a - b).abs(),
            lessThanOrEqualTo(tolerance),
            reason: 'frame $i value $v',
          );
        }
      }
    },
    skip: library == null ? 'no built pb_mapper_ffi library' : false,
  );

  testWidgets('a planet picked by keyboard opens its panel', (tester) async {
    final api = FakePbMapperApi(
      services: [fakeService(serviceKey: 'web')],
      serverServices: ['web'],
    );
    await pumpApp(tester, api);
    expect(find.byKey(const Key('galaxy-canvas')), findsOneWidget);

    // Focus the sky, then walk: this computer, then its first planet.
    await tester.tap(
      find.byKey(const Key('galaxy-canvas')),
      warnIfMissed: false,
    );
    await tester.pump();
    // The tap may have picked something already; start from a clean slate.
    if (find.byKey(const Key('galaxy-panel-close')).evaluate().isNotEmpty) {
      await tester.tap(find.byKey(const Key('galaxy-panel-close')));
      await settle(tester);
    }
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowRight);
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowRight);
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await settle(tester);

    expect(find.byKey(const Key('galaxy-toggle')), findsOneWidget);
    await tester.tap(find.byKey(const Key('galaxy-toggle')));
    await settle(tester);
    expect(api.calls, contains('unregisterService(web)'));
    await finish(tester);
  });
}
