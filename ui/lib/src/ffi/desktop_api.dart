import 'dart:typed_data';
import 'dart:io';
import 'dart:ui' as ui;
import 'pb_mapper_service.dart';

class DesktopDisplay {
  const DesktopDisplay(this.id, this.name, this.width, this.height);
  factory DesktopDisplay.fromJson(Map<String, dynamic> json) => DesktopDisplay(
    json['id'] as int,
    json['name'] as String,
    json['width'] as int,
    json['height'] as int,
  );
  final int id;
  final String name;
  final int width;
  final int height;
}

class DesktopCapabilities {
  const DesktopCapabilities({
    this.host = false,
    this.displays = const [],
    this.message = '',
    this.helper,
  });
  final bool host;
  final List<DesktopDisplay> displays;
  final String message;
  final DesktopHelper? helper;
}

class DesktopHelper {
  const DesktopHelper({
    this.installed = false,
    this.running = false,
    this.authorized = false,
    this.message = '',
  });
  final bool installed, running, authorized;
  final String message;
  factory DesktopHelper.fromJson(Map<String, dynamic> data) => DesktopHelper(
    installed: data['installed'] == true,
    running: data['running'] == true,
    authorized: data['authorized_application'] == true,
    message: data['message'] as String? ?? '',
  );
}

class DesktopStatus {
  const DesktopStatus({
    this.state = 'stopped',
    this.message = '',
    this.controlling = false,
    this.controlAllowed = false,
    this.generation = 0,
    this.frameId = 0,
    this.display,
    this.path = '',
    this.frames = 0,
    this.bytes = 0,
    this.desktop = 'ordinary',
    this.codec = 'jpeg',
    this.videoWidth = 0,
    this.videoHeight = 0,
    this.keyframe = true,
    this.ptsUs = 0,
    this.captureFps = 0,
    this.receiveFps = 0,
    this.droppedFrames = 0,
    this.bitrate = 0,
    this.cursor,
    this.frameAckMs = 0,
  });
  factory DesktopStatus.fromJson(Map<String, dynamic> json) => DesktopStatus(
    state: json['state'] as String? ?? 'stopped',
    message: json['message'] as String? ?? '',
    controlling: json['controlling'] == true,
    controlAllowed: json['control_allowed'] == true,
    generation: json['generation'] as int? ?? 0,
    frameId: json['frame_id'] as int? ?? 0,
    display: json['display'] == null
        ? null
        : DesktopDisplay.fromJson(
            Map<String, dynamic>.from(json['display'] as Map),
          ),
    path: json['path'] as String? ?? '',
    frames: json['frames'] as int? ?? 0,
    bytes: json['bytes'] as int? ?? 0,
    desktop: json['desktop'] as String? ?? 'ordinary',
    codec: (json['video'] as Map?)?['codec'] as String? ?? 'jpeg',
    videoWidth: (json['video'] as Map?)?['width'] as int? ?? 0,
    videoHeight: (json['video'] as Map?)?['height'] as int? ?? 0,
    keyframe: (json['video'] as Map?)?['keyframe'] != false,
    ptsUs: (json['video'] as Map?)?['pts_us'] as int? ?? 0,
    captureFps: (json['capture_fps'] as num?)?.toDouble() ?? 0,
    receiveFps: (json['receive_fps'] as num?)?.toDouble() ?? 0,
    droppedFrames: json['dropped_frames'] as int? ?? 0,
    bitrate: json['bitrate'] as int? ?? 0,
    cursor: json['cursor'] is Map
        ? DesktopCursor.fromJson(
            Map<String, dynamic>.from(json['cursor'] as Map),
          )
        : null,
    frameAckMs: (json['frame_ack_ms'] as num?)?.toDouble() ?? 0,
  );
  final String state, message, path, desktop;
  final bool controlling, controlAllowed;
  final int generation, frameId, frames, bytes;
  final DesktopDisplay? display;
  final String codec;
  final int videoWidth, videoHeight, ptsUs, droppedFrames, bitrate;
  final bool keyframe;
  final double captureFps, receiveFps;
  final DesktopCursor? cursor;
  final double frameAckMs;
  bool get streaming => state == 'streaming';
}

class DesktopCursor {
  const DesktopCursor({
    required this.x,
    required this.y,
    required this.visible,
  });
  final int x, y;
  final bool visible;
  factory DesktopCursor.fromJson(Map<String, dynamic> json) => DesktopCursor(
    x: json['x'] as int,
    y: json['y'] as int,
    visible: json['visible'] == true,
  );
}

class DesktopSnapshot {
  const DesktopSnapshot({this.host, this.viewer});
  final DesktopStatus? host, viewer;
}

class DesktopTrustedHost {
  const DesktopTrustedHost({
    required this.relay,
    required this.service,
    required this.displayId,
    this.allowControl = false,
    this.relayOnly = false,
    this.privileged = false,
    this.autoStart = false,
  });
  final String relay, service;
  final int displayId;
  final bool allowControl, relayOnly, privileged, autoStart;
  factory DesktopTrustedHost.fromJson(Map<String, dynamic> data) =>
      DesktopTrustedHost(
        relay: data['relay'] as String,
        service: data['service'] as String,
        displayId: data['display_id'] as int,
        allowControl: data['allow_control'] == true,
        relayOnly: data['relay_only'] == true,
        privileged: data['privileged'] == true,
        autoStart: data['auto_start'] == true,
      );
}

class DesktopTrust {
  const DesktopTrust({
    this.supported = false,
    this.saved,
    this.matchesRelay = false,
  });
  final bool supported, matchesRelay;
  final DesktopTrustedHost? saved;
}

class DesktopFrame {
  const DesktopFrame(this.status, this.bytes);
  final DesktopStatus status;
  final Uint8List bytes;
}

abstract interface class DesktopApiClient {
  Future<DesktopTrust> trustStatus();
  Future<String> restoreHost();
  Future<void> revokeTrust();
  Stream<DesktopFrame> frames();
  Future<void> keyframe();
  Future<void> helperAction(String action);
  Future<void> presented(int generation, int frameId);
  Future<DesktopCapabilities> capabilities();
  Future<DesktopSnapshot> status();
  Future<String> startHost({
    required String service,
    required int displayId,
    required bool allowControl,
    required bool relayOnly,
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
  });
  Future<void> stopHost({bool onExit = false});
  Future<void> allowControl(bool allow);
  Future<void> connect(String invitation, {required bool relayOnly});
  Future<void> disconnect();
  Future<void> control(bool enabled);
  Future<void> release();
  Future<void> input({
    required int generation,
    required int frameId,
    required int sequence,
    required Map<String, Object?> input,
  });
  Future<DesktopFrame?> frame(int after, int generation);
}

class DesktopApi implements DesktopApiClient {
  @override
  Future<DesktopTrust> trustStatus() async {
    final data = await _call('trust_status');
    return DesktopTrust(
      supported: data['supported'] == true,
      matchesRelay: data['matches_relay'] == true,
      saved: data['saved'] is Map
          ? DesktopTrustedHost.fromJson(
              Map<String, dynamic>.from(data['saved'] as Map),
            )
          : null,
    );
  }

  @override
  Future<String> restoreHost() async =>
      (await _call('host_restore'))['invitation'] as String;
  @override
  Future<void> revokeTrust() async {
    await _call('trust_revoke');
  }

  @override
  Stream<DesktopFrame> frames() => PbMapperService().desktopFrames().map(
    (data) => DesktopFrame(
      DesktopStatus.fromJson(data['status'] as Map<String, dynamic>),
      data['bytes'] as Uint8List,
    ),
  );
  @override
  Future<void> keyframe() async {
    await _call('keyframe');
  }

  @override
  Future<void> helperAction(String action) async {
    await _call('helper_action', {'action': action});
  }

  @override
  Future<void> presented(int generation, int frameId) async {
    await _call('presented', {'generation': generation, 'frame_id': frameId});
  }

  Future<Map<String, dynamic>> _call(
    String op, [
    Map<String, Object?> args = const {},
  ]) async {
    final result = await PbMapperService().desktopCall({'op': op, ...args});
    if (result['success'] != true) {
      throw StateError(
        result['message'] as String? ?? 'Desktop operation failed',
      );
    }
    return Map<String, dynamic>.from(result['data'] as Map? ?? {});
  }

  @override
  Future<DesktopCapabilities> capabilities() async {
    final data = await _call('capabilities');
    return DesktopCapabilities(
      host: (data['capabilities'] as Map)['host'] == true,
      displays: [
        for (final d in data['displays'] as List? ?? [])
          DesktopDisplay.fromJson(Map<String, dynamic>.from(d as Map)),
      ],
      message: data['message'] as String? ?? '',
      helper: data['helper'] is Map
          ? DesktopHelper.fromJson(
              Map<String, dynamic>.from(data['helper'] as Map),
            )
          : null,
    );
  }

  @override
  Future<DesktopSnapshot> status() async {
    final data = await _call('status');
    DesktopStatus? parse(String name) => data[name] == null
        ? null
        : DesktopStatus.fromJson(Map<String, dynamic>.from(data[name] as Map));
    return DesktopSnapshot(host: parse('host'), viewer: parse('viewer'));
  }

  @override
  Future<String> startHost({
    required String service,
    required int displayId,
    required bool allowControl,
    required bool relayOnly,
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
  }) async =>
      (await _call('host_start', {
            'service': service,
            'display_id': displayId,
            'allow_control': allowControl,
            'relay_only': relayOnly,
            'privileged': privileged,
            'remember': remember,
            'auto_start': autoStart,
          }))['invitation']
          as String;
  @override
  Future<void> stopHost({bool onExit = false}) async {
    await _call('host_stop', {'on_exit': onExit});
  }

  @override
  Future<void> allowControl(bool allow) async {
    await _call('host_permission', {'allow_control': allow});
  }

  @override
  Future<void> connect(String invitation, {required bool relayOnly}) async {
    final views = ui.PlatformDispatcher.instance.views;
    final refreshRate = views.isEmpty ? 60.0 : views.first.display.refreshRate;
    // A phone's display is a hard presentation ceiling. Avoid spending decoder
    // and network capacity on frames that its compositor cannot present.
    final fps = Platform.isAndroid && refreshRate.isFinite && refreshRate > 0
        ? refreshRate.round().clamp(30, 90)
        : 90;
    await _call('connect', {
      'invitation': invitation,
      'relay_only': relayOnly,
      'native_video': Platform.isWindows || Platform.isAndroid,
      'video': {
        'max_width': 1920,
        'max_height': 1080,
        'max_fps': fps,
        'bitrate': 12000000,
      },
    });
  }

  @override
  Future<void> disconnect() async {
    PbMapperService().stopDesktopFrames();
    await _call('disconnect');
  }

  @override
  Future<void> control(bool enabled) async {
    await _call('control', {'enabled': enabled});
  }

  @override
  Future<void> release() async {
    await _call('release');
  }

  @override
  Future<void> input({
    required int generation,
    required int frameId,
    required int sequence,
    required Map<String, Object?> input,
  }) async {
    await _call('input', {
      'event': {
        'generation': generation,
        'frame_id': frameId,
        'sequence': sequence,
        'input': input,
      },
    });
  }

  @override
  Future<DesktopFrame?> frame(int after, int generation) async {
    final data = await PbMapperService().desktopFrame(after, generation);
    return data == null
        ? null
        : DesktopFrame(
            DesktopStatus.fromJson(data['status'] as Map<String, dynamic>),
            data['bytes'] as Uint8List,
          );
  }
}
