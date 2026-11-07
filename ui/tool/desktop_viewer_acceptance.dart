// Explicit native UI acceptance entry, never the packaged application entry.
// Build with --target tool/desktop_viewer_acceptance.dart and the defines below.
// The bootstrap contains only a dedicated expiring test scope from `deployed`,
// and takes the report back with PUT /report (scripts/test/desktop-bootstrap.cjs),
// so a release build, whose private storage cannot be read, works too.
//
// Report schema 2 (perf 11.1): one sample a second of `viewer_status` exactly
// as Rust reports it, with that second's Flutter frame timings; the run's
// events (phases, app lifecycle, the first picture after each resume); the
// build, device, network and battery temperature; the disconnect snapshot;
// and a summary of the measures the pass criteria name. It holds counters and
// status only, never images, invitations or credentials.
//
// Defines: setupUrl (a loopback controller.json), seconds (15..1800),
// relayOnly, quality (quality | balanced | smooth), case (a label for the
// run) and host (the host's GPU and refresh rate, which a viewer cannot see).
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' show FrameTiming, PlatformDispatcher;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';

/// Samples before this are startup, not steady state.
const _warmup = Duration(seconds: 5);

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final setup = Uri.parse(const String.fromEnvironment('setupUrl'));
  const defaultSeconds = int.fromEnvironment('seconds', defaultValue: 30);
  const defaultRelayOnly = bool.fromEnvironment('relayOnly');
  final quality =
      DesktopQuality.values.asNameMap()[const String.fromEnvironment(
        'quality',
        defaultValue: 'balanced',
      )];
  if (setup.scheme != 'http' ||
      !['127.0.0.1', 'localhost', '::1'].contains(setup.host) ||
      defaultSeconds < 15 ||
      defaultSeconds > 1800 ||
      quality == null) {
    throw ArgumentError(
      'Set a loopback setupUrl, seconds 15..1800 and quality '
      'quality|balanced|smooth',
    );
  }
  final config = await _fetchSetup(setup);
  final seconds = (config['seconds'] as int? ?? defaultSeconds).clamp(15, 1800);
  final relayOnly = config['relay_only'] as bool? ?? defaultRelayOnly;

  final service = PbMapperService();
  service.ensureInitialized();
  final support = await getApplicationSupportDirectory();
  final directory = '${support.path}/desktop-acceptance';
  await Directory(directory).create(recursive: true);
  await service.setAppDirectoryPath(directory);
  final api = DesktopApi();
  // The store keeps its texture to itself; the samples read the native
  // decoder's own counters through the same one.
  DesktopVideoTexture? texture;
  final store = DesktopStore(
    api,
    textures: ({required instanceToken}) async =>
        texture = await createDesktopVideoTexture(instanceToken: instanceToken),
  );
  final run = _Run(api, store);
  // What the app's viewer host holds while a viewer is on screen.
  final demand = store.demand(DesktopDemand.viewer);
  var outcome = 'DESKTOP_ACCEPTANCE failed';
  try {
    final batteryStart = await _batteryCelsius();
    final environment = await _environment();
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
        home: Builder(
          builder: (context) => Theme(
            data: viewerTheme(context),
            child: DesktopViewer(
              desktop: store,
              prefs: ViewerPrefs(persist: false),
              title: 'acceptance',
            ),
          ),
        ),
      ),
    );
    run.start();
    await store.connect(
      DesktopTarget(
        invitation: config['invitation'] as String,
        name: 'acceptance',
        relayOnly: relayOnly,
        quality: quality,
      ),
    );
    for (var i = 0; i < seconds; i++) {
      await Future<void>.delayed(const Duration(seconds: 1));
      run.samples.add(await run.sample(texture));
    }
    final ending = await run.disconnect('$directory/acceptance.thumb');
    final report = {
      'schema': 2,
      'case': const String.fromEnvironment('case'),
      'quality': quality.name,
      'relay_only': relayOnly,
      'transport': config['transport'],
      'seconds': seconds,
      'warmup_seconds': _warmup.inSeconds,
      ...environment,
      'host': {
        'description': const String.fromEnvironment('host'),
        // The viewer sees the rate model; the host's feedback log has `gop`.
        'rate_model': run.lastStats?['rate_model'],
        'gop': run.lastStats?['gop'],
      },
      'decoder': run.lastStatus?['decoder'],
      'battery_celsius': {
        'start': batteryStart,
        'end': await _batteryCelsius(),
      },
      'elapsed_us': run.clock.elapsedMicroseconds,
      'connect_ms': run.connectUs == null ? null : run.connectUs! / 1000,
      'disconnect': ending,
      'summary': run.summary(),
      'events': run.events,
      'samples': run.samples,
    };
    await _upload(setup.resolve('/report'), report);
    outcome =
        'DESKTOP_ACCEPTANCE report uploaded; credentials and images '
        'excluded';
  } finally {
    debugPrint(outcome);
    run.dispose();
    // Unmount the viewer before its store goes.
    runApp(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Center(child: Text(outcome)),
      ),
    );
    await WidgetsBinding.instance.endOfFrame;
    demand.dispose();
    await store.disconnect();
    store.dispose();
    service.dispose();
  }
}

Future<Map<String, dynamic>> _fetchSetup(Uri setup) async {
  final http = HttpClient()..connectionTimeout = const Duration(seconds: 5);
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
    return jsonDecode(utf8.decode(buffer)) as Map<String, dynamic>;
  } finally {
    http.close(force: true);
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

double get _refreshHz {
  final views = PlatformDispatcher.instance.views;
  final hz = views.isEmpty ? 60.0 : views.first.display.refreshRate;
  return hz.isFinite && hz > 0 ? hz : 60;
}

/// The build, the device and its network, as far as an app can see them.
Future<Map<String, Object?>> _environment() async {
  final package = await PackageInfo.fromPlatform();
  Future<String?> property(String name) async {
    try {
      final result = await Process.run('getprop', [
        name,
      ]).timeout(const Duration(seconds: 2));
      final value = '${result.stdout}'.trim();
      return value.isEmpty ? null : value;
    } catch (_) {
      return null;
    }
  }

  return {
    'build': {
      'version': package.version,
      'number': package.buildNumber,
      'mode': kReleaseMode
          ? 'release'
          : kProfileMode
          ? 'profile'
          : 'debug',
    },
    'device': {
      'os': Platform.operatingSystem,
      'os_version': Platform.operatingSystemVersion,
      'processors': Platform.numberOfProcessors,
      'refresh_hz': _refreshHz,
      if (Platform.isAndroid) ...{
        'manufacturer': await property('ro.product.manufacturer'),
        'model': await property('ro.product.model'),
        'sdk': await property('ro.build.version.sdk'),
      },
    },
    'network': await _network(),
  };
}

/// Interface names only, never addresses. The type is the first of Wi-Fi,
/// Ethernet and cellular among the interfaces that are up.
Future<Map<String, Object?>> _network() async {
  try {
    final names = [for (final i in await NetworkInterface.list()) i.name];
    String kind(String name) {
      final n = name.toLowerCase();
      if (n.startsWith('wlan') || n.contains('wi-fi') || n.contains('wifi')) {
        return 'wifi';
      }
      if (n.startsWith('eth') || n.contains('ethernet')) return 'ethernet';
      if (['rmnet', 'ccmni', 'wwan', 'pdp'].any(n.contains)) return 'cellular';
      return 'other';
    }

    final kinds = {for (final n in names) kind(n)};
    return {
      'type':
          ['wifi', 'ethernet', 'cellular'].where(kinds.contains).firstOrNull ??
          'unknown',
      'interfaces': names,
    };
  } catch (_) {
    return {'type': 'unknown'};
  }
}

/// Android's battery sensor in °C; null where an app cannot read it.
Future<double?> _batteryCelsius() async {
  if (!Platform.isAndroid) return null;
  try {
    final tenths = await File(
      '/sys/class/power_supply/battery/temp',
    ).readAsString();
    return int.parse(tenths.trim()) / 10;
  } catch (_) {
    return null;
  }
}

class _Run {
  _Run(this.api, this.store);
  final DesktopApi api;
  final DesktopStore store;
  final clock = Stopwatch();
  final events = <Map<String, Object?>>[];
  final samples = <Map<String, Object?>>[];
  final _frames = _FrameWindow();
  AppLifecycleListener? _lifecycle;
  AppLifecycleState? _state;
  bool _wasHidden = false;
  DesktopPhase? _phase;
  Timer? _probe;

  /// From connect to the first live picture.
  int? connectUs;
  Map<String, dynamic>? lastStatus;
  Map<String, dynamic>? get lastStats => lastStatus?['stats'] is Map
      ? Map<String, dynamic>.from(lastStatus!['stats'] as Map)
      : null;

  int get _now => clock.elapsedMicroseconds;

  void start() {
    clock.start();
    WidgetsBinding.instance.addTimingsCallback(_frames.add);
    store.session.addListener(_onSession);
    _lifecycle = AppLifecycleListener(onStateChange: _onLifecycle);
    _onSession();
  }

  void dispose() {
    _probe?.cancel();
    _lifecycle?.dispose();
    store.session.removeListener(_onSession);
    WidgetsBinding.instance.removeTimingsCallback(_frames.add);
  }

  void _onSession() {
    final session = store.session.value;
    if (session.phase == _phase) return;
    _phase = session.phase;
    if (session.phase == DesktopPhase.live) connectUs ??= _now;
    events.add({
      'elapsed_us': _now,
      'phase': session.phase.name,
      if (session.failure != null) 'failure': session.failure!.code,
    });
  }

  void _onLifecycle(AppLifecycleState state) {
    _state = state;
    events.add({'elapsed_us': _now, 'lifecycle': state.name});
    if (state == AppLifecycleState.hidden ||
        state == AppLifecycleState.paused) {
      _wasHidden = true;
    } else if (state == AppLifecycleState.resumed && _wasHidden) {
      _wasHidden = false;
      _probeResume();
    }
  }

  DesktopView? _leaf() {
    try {
      return api.view();
    } catch (_) {
      return null;
    }
  }

  /// Case E: the first picture presented after a resume, read from the view
  /// leaf, or null when none came within 10 s.
  void _probeResume() {
    _probe?.cancel();
    final before = _leaf();
    final from = _now;
    _probe = Timer.periodic(const Duration(milliseconds: 8), (timer) {
      final view = _leaf();
      final shown =
          view != null &&
          view.presentedFrameId != 0 &&
          (before == null ||
              view.presentedFrameId != before.presentedFrameId ||
              view.generation != before.generation);
      if (!shown && _now - from < 10000000) return;
      timer.cancel();
      events.add({
        'elapsed_us': _now,
        'resume_picture_ms': shown ? (_now - from) / 1000 : null,
      });
    });
  }

  Future<Map<String, Object?>> sample(DesktopVideoTexture? texture) async {
    Object? status;
    try {
      final result = PbMapperService().desktopViewerCallSync({
        'op': 'viewer_status',
      });
      status = result['success'] == true
          ? result['data']
          : {'error': result['code'] ?? 'INTERNAL'};
    } catch (e) {
      status = {'error': '$e'};
    }
    if (status is Map && status['stats'] is Map) {
      lastStatus = Map<String, dynamic>.from(status);
    }
    Map<String, Object?>? native;
    try {
      native = await texture?.stats();
    } catch (_) {
      // A disposed texture has no counters.
    }
    final session = store.session.value;
    return {
      'elapsed_us': _now,
      'phase': session.phase.name,
      'reconnect_attempt': session.reconnectAttempt,
      if (session.failure != null) 'failure': session.failure!.code,
      'lifecycle': _state?.name,
      'status': status,
      'native': native,
      'flutter': _frames.take(1e6 / _refreshHz),
    };
  }

  /// D23: the thumbnail is written only from a fresh picture, within 300 ms,
  /// and never holds up the disconnect.
  Future<Map<String, Object?>> disconnect(String path) async {
    final thumbnail = File(path);
    if (await thumbnail.exists()) await thumbnail.delete();
    final fresh = store.session.value.fresh;
    final watch = Stopwatch()..start();
    await store.disconnect(snapshotPath: path);
    final written = await thumbnail.exists();
    return {
      'fresh': fresh,
      'disconnect_ms': watch.elapsedMicroseconds / 1000,
      'snapshot_written': written,
      'snapshot_bytes': written ? await thumbnail.length() : null,
    };
  }

  /// The measures perf 11.4 names, over the samples after the warm-up. No
  /// verdict: the owner decides.
  Map<String, Object?> summary() {
    final steady = [
      for (final s in samples)
        if ((s['elapsed_us'] as int) >= _warmup.inMicroseconds) s,
    ];
    final live = [
      for (final s in steady)
        if (s['phase'] == DesktopPhase.live.name) s,
    ];
    Map<String, dynamic> stats(Map<String, Object?> s) => switch (s['status']) {
      {'stats': final Map<String, dynamic> stats} => stats,
      _ => const {},
    };
    List<num> values(List<Map<String, Object?>> from, String key) => [
      for (final s in from)
        if (stats(s)[key] case final num value) value,
    ];
    List<num> intervals(List<Map<String, Object?>> from, String key) => [
      for (final s in from)
        if ((stats(s)['present_interval'] as Map?)?[key] case final num value)
          value,
    ];
    // Session counters restart with each reconnect's new viewer.
    num rise(List<num> counter) {
      num total = 0;
      for (var i = 1; i < counter.length; i++) {
        final step = counter[i] - counter[i - 1];
        total += step >= 0 ? step : counter[i];
      }
      return total;
    }

    Map<String, int> tally(String key) {
      final counts = <String, int>{};
      for (final s in steady) {
        final value = stats(s)[key];
        if (value != null) counts['$value'] = (counts['$value'] ?? 0) + 1;
      }
      return counts;
    }

    final minutes = steady.length / 60;
    // Gaps count over every steady second: a stall that leaves `live` is
    // exactly what they measure.
    final gaps100 = rise(intervals(steady, 'gaps_100'));
    final frames = [
      for (final s in steady)
        if (s['flutter'] case final Map<String, Object?> f) f,
    ];
    return {
      'steady_seconds': steady.length,
      'live_seconds': live.length,
      'presented_fps': _spread(values(live, 'presented_fps')),
      'received_fps': _spread(values(live, 'received_fps')),
      'present_interval_ms': {
        for (final key in ['p50_ms', 'p95_ms', 'p99_ms', 'max_ms'])
          key: _spread(intervals(live, key))?['max'],
      },
      'gaps_100': gaps100,
      'gaps_100_per_minute': minutes == 0 ? null : gaps100 / minutes,
      'gaps_250': rise(intervals(steady, 'gaps_250')),
      'picture_latency_ms': _spread(values(live, 'picture_latency_ms')),
      'rtt_ms': _spread(values(steady, 'rtt_ms')),
      'queue_delay_ms': _spread(values(steady, 'queue_delay_ms')),
      'keyframes': rise(values(steady, 'keyframes')),
      'keyframes_requested': rise(values(steady, 'keyframes_requested')),
      'backlog_drops': rise(values(steady, 'backlog_drops')),
      'dropped_frames': rise(values(steady, 'dropped_frames')),
      'adaptation_seconds': tally('adaptation'),
      'degraded_seconds': steady
          .where((s) => stats(s)['degraded'] == true)
          .length,
      'fps_cap': tally('fps_cap'),
      'resolutions': {
        for (final s in steady)
          if (stats(s) case {
            'width': final int w,
            'height': final int h,
          } when w > 0)
            '${w}x$h',
      }.toList(),
      'reconnects': events
          .where((e) => e['phase'] == DesktopPhase.reconnecting.name)
          .length,
      'resume_picture_ms': [
        for (final e in events)
          if (e.containsKey('resume_picture_ms')) e['resume_picture_ms'],
      ],
      'flutter': {
        'frames': frames.fold<int>(0, (n, f) => n + (f['frames'] as int)),
        'over_budget': frames.fold<int>(
          0,
          (n, f) => n + (f['over_budget'] as int),
        ),
        'build_max_ms': _spread([
          for (final f in frames) f['build_max_ms'] as num,
        ])?['max'],
        'raster_max_ms': _spread([
          for (final f in frames) f['raster_max_ms'] as num,
        ])?['max'],
      },
    };
  }
}

/// p50, min and max, or null for no values.
Map<String, num>? _spread(List<num> values) {
  if (values.isEmpty) return null;
  final sorted = [...values]..sort();
  return {
    'p50': sorted[(sorted.length - 1) ~/ 2],
    'min': sorted.first,
    'max': sorted.last,
  };
}

/// Flutter's frame timings, summarised a second at a time.
class _FrameWindow {
  final _timings = <FrameTiming>[];

  void add(List<FrameTiming> timings) => _timings.addAll(timings);

  /// The window since the last take; a frame is over budget when its build
  /// or its raster took longer than one refresh interval.
  Map<String, Object> take(double budgetUs) {
    final timings = [..._timings];
    _timings.clear();
    num ms(Duration d) => d.inMicroseconds / 1000;
    final totals = [for (final t in timings) ms(t.totalSpan)]..sort();
    return {
      'frames': timings.length,
      'over_budget': timings
          .where(
            (t) =>
                t.buildDuration.inMicroseconds > budgetUs ||
                t.rasterDuration.inMicroseconds > budgetUs,
          )
          .length,
      'build_max_ms': timings.fold<num>(0, (m, t) {
        final v = ms(t.buildDuration);
        return v > m ? v : m;
      }),
      'raster_max_ms': timings.fold<num>(0, (m, t) {
        final v = ms(t.rasterDuration);
        return v > m ? v : m;
      }),
      'total_p50_ms': totals.isEmpty ? 0 : totals[(totals.length - 1) ~/ 2],
      'total_p95_ms': totals.isEmpty
          ? 0
          : totals[((totals.length - 1) * 0.95).round()],
      'total_max_ms': totals.isEmpty ? 0 : totals.last,
    };
  }
}
