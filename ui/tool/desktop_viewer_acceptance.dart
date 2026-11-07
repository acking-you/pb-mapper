// Explicit native UI acceptance entry, never the packaged application entry.
// Build with --target tool/desktop_viewer_acceptance.dart and the defines below.
// The bootstrap contains only a dedicated expiring test scope from `deployed`,
// and takes the report back with PUT /report (scripts/test/desktop-bootstrap.cjs),
// so a release build, whose private storage cannot be read, works too.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' show FrameTiming;

import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/pages/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final setup = Uri.parse(const String.fromEnvironment('setupUrl'));
  const defaultSeconds = int.fromEnvironment('seconds', defaultValue: 30);
  const defaultRelayOnly = bool.fromEnvironment('relayOnly');
  if (setup.scheme != 'http' ||
      !['127.0.0.1', 'localhost', '::1'].contains(setup.host) ||
      defaultSeconds < 15 ||
      defaultSeconds > 1800) {
    throw ArgumentError('Set a loopback setupUrl and seconds 15..1800');
  }
  final http = HttpClient()..connectionTimeout = const Duration(seconds: 5);
  late final Map<String, dynamic> config;
  try {
    final response = await (await http.getUrl(setup)).close();
    if (response.statusCode != HttpStatus.ok) {
      throw StateError('Acceptance bootstrap unavailable');
    }
    final buffer = <int>[];
    await for (final chunk in response) {
      buffer.addAll(chunk);
      if (buffer.length > 16 * 1024) {
        throw StateError('Acceptance bootstrap exceeds bounds');
      }
    }
    config = jsonDecode(utf8.decode(buffer)) as Map<String, dynamic>;
  } finally {
    http.close(force: true);
  }
  final seconds = (config['seconds'] as int? ?? defaultSeconds).clamp(15, 1800);
  final relayOnly = config['relay_only'] as bool? ?? defaultRelayOnly;

  final service = PbMapperService();
  service.ensureInitialized();
  final support = await getApplicationSupportDirectory();
  await service.setAppDirectoryPath('${support.path}/desktop-acceptance');
  // The store keeps its texture to itself; the report reads the native
  // presentation counters through the same one.
  DesktopVideoTexture? texture;
  final store = DesktopStore(
    DesktopApi(),
    textures: ({required instanceToken}) async =>
        texture = await createDesktopVideoTexture(instanceToken: instanceToken),
  );
  // Statistics are sampled only while something listens.
  void sampled() {}
  store.stats.addListener(sampled);
  final rendering = _RenderTimings();
  WidgetsBinding.instance.addTimingsCallback(rendering.add);
  try {
    final update = await service.updateConfig(
      serverAddress: config['server'] as String,
      keepAlive: true,
      msgHeaderKey: config['credential'] as String,
      relayTransport: config['transport'] as String,
    );
    if (update['success'] != true) {
      throw StateError('Acceptance configuration failed');
    }
    runApp(
      MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: DesktopViewer(store: store),
      ),
    );
    await store.connect(
      DesktopTarget(
        invitation: config['invitation'] as String,
        name: 'acceptance',
        relayOnly: relayOnly,
      ),
    );
    final clock = Stopwatch()..start();
    final samples = <Map<String, Object?>>[];
    for (var i = 0; i < seconds; i++) {
      await Future<void>.delayed(const Duration(seconds: 1));
      final session = store.session.value;
      final stats = store.stats.value;
      final geometry = store.geometry.value;
      final native = await texture?.stats();
      samples.add({
        'elapsed_us': clock.elapsedMicroseconds,
        'phase': session.phase.name,
        'codec': stats.codec,
        'path': session.path,
        'receive_fps': stats.receivedFps,
        'present_fps': stats.presentedFps,
        'bitrate': stats.bitrateBps,
        'width': geometry?.videoWidth,
        'height': geometry?.videoHeight,
        'picture_latency_ms': stats.pictureLatencyMs,
        'has_error': session.failure != null,
        'error': session.failure?.code,
        'reconnects': session.reconnectAttempt,
        'controlling': session.controlling,
        'texture_id': store.picture.value?.textureId,
        'native': native,
        'flutter': rendering.snapshot(),
      });
    }
    // Native counters acknowledge texture consumption, not decoder submission.
    // Supply moving content separately: static liveness repeats are not proof
    // of the requested number of unique desktop updates.
    final steady = samples
        .where((s) => (s['elapsed_us'] as int) >= 5000000)
        .toList();
    final firstNative = steady.first['native'] as Map<Object?, Object?>?;
    final lastNative = steady.last['native'] as Map<Object?, Object?>?;
    final firstPresented = firstNative?['presented'] as num?;
    final lastPresented = lastNative?['presented'] as num?;
    final durationUs =
        (steady.last['elapsed_us'] as int) -
        (steady.first['elapsed_us'] as int);
    final report = {
      'schema': 1,
      'platform': Platform.operatingSystem,
      'relay_only': relayOnly,
      'warmup_seconds': 5,
      'elapsed_us': clock.elapsedMicroseconds,
      'native_presented_fps': firstPresented == null || lastPresented == null
          ? null
          : (lastPresented - firstPresented) * 1000000 / durationUs,
      'samples': samples,
    };
    await _upload(setup.resolve('/report'), report);
    debugPrint(
      'DESKTOP_ACCEPTANCE report uploaded; credentials and images excluded',
    );
  } finally {
    WidgetsBinding.instance.removeTimingsCallback(rendering.add);
    store.stats.removeListener(sampled);
    await store.disconnect();
    store.dispose();
    service.dispose();
  }
}

// The bootstrap names and keeps the file. It refuses more than 8 MiB, so the
// report goes compact.
Future<void> _upload(Uri target, Map<String, Object?> report) async {
  final http = HttpClient()..connectionTimeout = const Duration(seconds: 5);
  try {
    final request = await http.putUrl(target);
    request.headers.contentType = ContentType.json;
    request.add(utf8.encode(jsonEncode(report)));
    final response = await request.close();
    await response.drain<void>();
    if (response.statusCode != HttpStatus.created) {
      throw StateError('Report upload refused: HTTP ${response.statusCode}');
    }
  } finally {
    http.close(force: true);
  }
}

// Explicit acceptance diagnostics only; no callback is installed by the app.
class _RenderTimings {
  int frames = 0, buildUs = 0, rasterUs = 0, totalUs = 0;
  int maxBuildUs = 0, maxRasterUs = 0, maxTotalUs = 0;
  void add(List<FrameTiming> timings) {
    for (final timing in timings) {
      frames++;
      final build = timing.buildDuration.inMicroseconds;
      final raster = timing.rasterDuration.inMicroseconds;
      final total = timing.totalSpan.inMicroseconds;
      buildUs += build;
      rasterUs += raster;
      totalUs += total;
      if (build > maxBuildUs) maxBuildUs = build;
      if (raster > maxRasterUs) maxRasterUs = raster;
      if (total > maxTotalUs) maxTotalUs = total;
    }
  }

  Map<String, Object> snapshot() => {
    'frames': frames,
    'build_mean_ms': frames == 0 ? 0 : buildUs / frames / 1000,
    'raster_mean_ms': frames == 0 ? 0 : rasterUs / frames / 1000,
    'total_mean_ms': frames == 0 ? 0 : totalUs / frames / 1000,
    'build_max_ms': maxBuildUs / 1000,
    'raster_max_ms': maxRasterUs / 1000,
    'total_max_ms': maxTotalUs / 1000,
  };
}
