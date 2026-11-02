/// Isolated galaxy renderer probe. It never starts a tunnel or reads settings.
///
/// Build/run with Flutter in profile mode; it prints `GALAXY_PROBE` JSON for
/// five seconds at rest and five seconds following a selection. Set the
/// `GALAXY_PROBE_OUTPUT` Dart define to a file path on GUI-only desktops.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_canvas.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_frame.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/state/remote_service.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';

void main() => runApp(const _Probe());

Tunnel _tunnel(TunnelKind kind, String key) => Tunnel(
  kind: kind,
  key: key,
  localAddress: '127.0.0.1:1234',
  protocol: 'TCP',
  encrypted: true,
  keepAlive: true,
  health: TunnelHealth.running,
  path: kind == TunnelKind.connect
      ? const TunnelPath(activeDirect: 1, directSessions: 1)
      : null,
);

final _scene = buildScene(
  shares: [for (var i = 0; i < 30; i++) _tunnel(TunnelKind.share, 'share-$i')],
  connections: [
    for (var i = 0; i < 10; i++) _tunnel(TunnelKind.connect, 'remote-$i'),
  ],
  relayAddress: 'relay.example:7666',
  relayOnline: true,
  remote: [for (var i = 0; i < 30; i++) RemoteService(name: 'remote-$i')],
  homeLabel: 'This computer',
);

class _Probe extends StatefulWidget {
  const _Probe();

  @override
  State<_Probe> createState() => _ProbeState();
}

class _ProbeState extends State<_Probe> {
  final _view = GalaxyView();
  final _canvas = GlobalKey<GalaxyCanvasState>();
  final _timings = <FrameTiming>[];
  bool _collect = false;

  @override
  void initState() {
    super.initState();
    SchedulerBinding.instance.addTimingsCallback(_record);
    WidgetsBinding.instance.addPostFrameCallback((_) => _run());
  }

  void _record(List<FrameTiming> timings) {
    if (_collect) _timings.addAll(timings);
  }

  int _cpuTicks() {
    if (!Platform.isLinux) return -1;
    final stat = File('/proc/self/stat').readAsStringSync();
    final fields = stat.substring(stat.lastIndexOf(')') + 2).split(' ');
    return int.parse(fields[11]) + int.parse(fields[12]);
  }

  Future<Map<String, Object>> _sample(String name) async {
    _timings.clear();
    final cpu = _cpuTicks();
    final watch = Stopwatch()..start();
    _collect = true;
    await Future<void>.delayed(const Duration(seconds: 5));
    _collect = false;
    watch.stop();
    final endTicks = _cpuTicks();
    final ticks = cpu < 0 || endTicks < 0 ? -1 : endTicks - cpu;
    final build = [for (final t in _timings) t.buildDuration.inMicroseconds]
      ..sort();
    final raster = [for (final t in _timings) t.rasterDuration.inMicroseconds]
      ..sort();
    int p95(List<int> values) =>
        values.isEmpty ? 0 : values[((values.length - 1) * .95).round()];
    return {
      'phase': name,
      'elapsed_ms': watch.elapsedMilliseconds,
      'frames': _timings.length,
      'build_p95_us': p95(build),
      'raster_p95_us': p95(raster),
      'cpu_ticks': ticks,
      'rss_bytes': ProcessInfo.currentRss,
      'native_geometry': GalaxyNative.available,
    };
  }

  Future<void> _run() async {
    await Future<void>.delayed(const Duration(seconds: 8));
    final idle = await _sample('idle');
    _canvas.currentState!.select('share:share-0');
    final active = await _sample('selection');
    final result = 'GALAXY_PROBE ${jsonEncode([idle, active])}';
    const output = String.fromEnvironment('GALAXY_PROBE_OUTPUT');
    if (output.isEmpty) {
      stdout.writeln(result);
    } else {
      File(output).writeAsStringSync('$result\n');
    }
    exit(0);
  }

  @override
  void dispose() {
    SchedulerBinding.instance.removeTimingsCallback(_record);
    _view.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    theme: ThemeData.dark(),
    home: Scaffold(
      body: GalaxyCanvas(
        key: _canvas,
        view: _view,
        scene: _scene,
        semanticLabel: 'Performance fixture',
      ),
    ),
  );
}
