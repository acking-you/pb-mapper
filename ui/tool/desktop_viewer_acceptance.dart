// Explicit native UI acceptance entry, never the packaged application entry.
// Build with --target tool/desktop_viewer_acceptance.dart and the defines below.
// The bootstrap contains only a dedicated expiring test scope from `deployed`.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' show FrameTiming;

import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/pages/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final setup = Uri.parse(const String.fromEnvironment('setupUrl'));
  const reportPath = String.fromEnvironment('reportPath');
  const defaultSeconds = int.fromEnvironment('seconds', defaultValue: 30);
  const defaultRelayOnly = bool.fromEnvironment('relayOnly');
  if (setup.scheme != 'http' ||
      !['127.0.0.1', 'localhost', '::1'].contains(setup.host) ||
      reportPath.isEmpty ||
      defaultSeconds < 15 ||
      defaultSeconds > 1800) {
    throw ArgumentError(
      'Set a loopback setupUrl, reportPath and seconds 15..1800',
    );
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
  final api = _AcceptanceApi();
  final store = DesktopStore(api);
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
    await store.load();
    await store.connect(config['invitation'] as String, relayOnly);
    final clock = Stopwatch()..start();
    final samples = <Map<String, Object?>>[];
    for (var i = 0; i < seconds; i++) {
      await Future<void>.delayed(const Duration(seconds: 1));
      final state = store.snapshot.viewer;
      final native = await store.videoTexture?.stats();
      samples.add({
        'elapsed_us': clock.elapsedMicroseconds,
        'state': state?.state,
        'codec': state?.codec,
        'path': state?.path,
        'direct_stage': state?.directStage,
        'direct_failure': state?.directFailure,
        'capture_fps': state?.captureFps,
        'receive_fps': state?.receiveFps,
        'present_fps': store.presentFps,
        'dropped': state?.droppedFrames,
        'frame_id': state?.frameId,
        'bytes': state?.bytes,
        'bitrate': state?.bitrate,
        'width': state?.videoWidth,
        'height': state?.videoHeight,
        'frame_ack_ms': state?.frameAckMs,
        'has_error': store.error != null,
        'error': store.error,
        'recovery': store.recoveryReason,
        'phase': store.connectionPhase.name,
        'reconnects': store.reconnectAttempt,
        'controlling': store.controlling,
        'picture_age_ms': store.pictureAge?.inMilliseconds,
        'texture_id': store.videoTexture?.textureId,
        'presented_generation': store.videoPresented?.generation,
        'presented_frame_id': store.videoPresented?.frameId,
        'feedback': api.snapshot(),
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
    final file = File(reportPath);
    await file.parent.create(recursive: true);
    await file.writeAsString(
      const JsonEncoder.withIndent('  ').convert(report),
    );
    debugPrint(
      'DESKTOP_ACCEPTANCE report saved; credentials and images excluded',
    );
  } finally {
    WidgetsBinding.instance.removeTimingsCallback(rendering.add);
    await store.disconnect();
    store.dispose();
    service.dispose();
  }
}

// Observe the ordinary API without changing its feedback timing or behavior.
class _AcceptanceApi extends DesktopApi {
  int acknowledgments = 0, acknowledgmentErrors = 0, keyframes = 0;
  int keyframeErrors = 0;
  int? generation, frameId;

  @override
  Future<void> presented(int generation, int frameId) async {
    try {
      await super.presented(generation, frameId);
      acknowledgments++;
      this.generation = generation;
      this.frameId = frameId;
    } catch (_) {
      acknowledgmentErrors++;
      rethrow;
    }
  }

  @override
  Future<void> keyframe() async {
    keyframes++;
    try {
      await super.keyframe();
    } catch (_) {
      keyframeErrors++;
      rethrow;
    }
  }

  Map<String, Object?> snapshot() => {
    'acknowledgments': acknowledgments,
    'ack_errors': acknowledgmentErrors,
    'last_generation': generation,
    'last_frame_id': frameId,
    'keyframes': keyframes,
    'keyframe_errors': keyframeErrors,
  };
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
