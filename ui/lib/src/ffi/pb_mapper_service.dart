import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:isolate';

import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart';

import 'package:pb_mapper_ui/src/common/state_change.dart';

import 'pb_mapper_ffi.dart';
import 'tunnel_event_stream.dart';

/// Log entry from native library.
class LogMessage {
  final String level;
  final String message;
  final int timestamp;

  LogMessage({
    required this.level,
    required this.message,
    required this.timestamp,
  });
}

/// One JPEG picture from a desktop video source; [consumer] receipts it.
typedef DesktopJpegUnit = ({
  int consumer,
  int generation,
  int frameId,
  Uint8List bytes,
});

class PbMapperService {
  /// A desktop lifecycle op, on a worker isolate: these may block on the
  /// network.
  Future<Map<String, dynamic>> desktopCall(Map<String, Object?> request) =>
      _runJsonOnWorker('desktop', {'request': jsonEncode(request)});

  /// A desktop viewer hot op (`pb_mapper_desktop_viewer_call_json`), on the
  /// calling isolate: the export never blocks, so the UI isolate calls it
  /// directly.
  Map<String, dynamic> desktopViewerCallSync(Map<String, Object?> request) {
    final arg = jsonEncode(request).toNativeUtf8();
    try {
      return _decodeJsonStatic(
        _ffi,
        _ffi.pbMapperDesktopViewerCall(_requireHandle(), arg),
      );
    } finally {
      calloc.free(arg);
    }
  }

  /// Fills [scratch] from the per-frame view leaf: 1 a viewer, 0 none.
  int desktopView(Pointer<PbDesktopViewV1> scratch) =>
      _ffi.pbMapperDesktopView(_requireHandle(), scratch);

  /// What native video consumers compare with their own (D9).
  int instanceToken() => _ffi.pbMapperDesktopInstanceToken();

  /// Receipts a JPEG picture the window showed: true when Rust accepted it.
  bool desktopVideoPresented(int consumer, int generation, int frameId) =>
      _ffi.pbMapperDesktopVideoPresented(consumer, generation, frameId) ==
      PbVideo.unit;

  /// A JPEG source's pictures, pulled by a worker isolate one at a time: the
  /// next is pulled once the last was delivered and the subscription is not
  /// paused, so a listener that pauses until it has shown a picture holds
  /// the worker. Cancelling detaches the consumer.
  Stream<DesktopJpegUnit> desktopJpegUnits(int source) {
    final port = ReceivePort('desktop-jpeg');
    late final StreamController<DesktopJpegUnit> units;
    SendPort? control;
    int? consumer;
    var cancelled = false;
    // The worker starts with one credit; a delivered picture owes it back.
    var owed = false;

    void grant() {
      if (cancelled || !owed || units.isPaused) return;
      owed = false;
      control?.send(true);
    }

    void stop() {
      control?.send(false);
      // Detaching wakes a pull blocked in Rust at once.
      final id = consumer;
      if (id != null) _ffi.pbMapperDesktopVideoDetach(id);
    }

    void onMessage(Object? message) {
      switch (message) {
        case SendPort worker:
          control = worker;
          if (cancelled) stop();
        case int id when id > 0:
          consumer = id;
          if (cancelled) stop();
        case int code:
          if (!cancelled) {
            units.addError(
              StateError('desktop video source $source: attach failed ($code)'),
            );
          }
        case (int c, int g, int f, TransferableTypedData data):
          if (cancelled) return;
          owed = true;
          units.add((
            consumer: c,
            generation: g,
            frameId: f,
            bytes: data.materialize().asUint8List(),
          ));
          grant();
        case [Object? error, Object? _]:
          // The worker's uncaught error; its exit follows.
          if (!cancelled) units.addError(StateError('$error'));
        case null:
          // The worker exited.
          port.close();
          if (!cancelled) unawaited(units.close());
      }
    }

    // Synchronous, so a listener's pause inside its handler is seen before
    // the credit is granted.
    units = StreamController<DesktopJpegUnit>(
      sync: true,
      onListen: () {
        port.listen(onMessage);
        unawaited(
          Isolate.spawn(
            _desktopJpegWorker,
            (source, port.sendPort),
            onExit: port.sendPort,
            onError: port.sendPort,
            debugName: 'desktop-jpeg',
          ).then<void>(
            (_) {},
            onError: (Object error) {
              port.close();
              if (!cancelled) {
                units.addError(error);
                unawaited(units.close());
              }
            },
          ),
        );
      },
      onResume: grant,
      onCancel: () {
        cancelled = true;
        stop();
      },
    );
    return units.stream;
  }

  static final PbMapperService _instance = PbMapperService._internal();
  factory PbMapperService() => _instance;
  PbMapperService._internal();

  final PbMapperFFI _ffi = PbMapperFFI();
  Pointer<Void>? _handle;
  int _activeCalls = 0;
  bool _disposeRequested = false;
  bool _loggingInitialized = false;

  static final StreamController<LogMessage> _logController =
      StreamController<LogMessage>.broadcast();
  static Stream<LogMessage> get logStream => _logController.stream;

  static NativeCallable<LogCallbackNative>? _logCallable;

  static final StreamController<StateChange> _changeController =
      StreamController<StateChange>.broadcast();

  /// Fires when something the UI shows has gone stale, whoever changed it —
  /// this window, a terminal, or a background status probe.
  ///
  /// Events carry identity only; listeners re-read through the normal API.
  static Stream<StateChange> get changeStream => _changeController.stream;

  static NativeCallable<ChangeCallbackNative>? _changeCallable;

  /// Begin delivering [changeStream]. Safe to call more than once.
  void initChangeEvents() {
    if (_changeCallable != null) return;
    ensureInitialized();
    // `.listener` and not `.isolateLocal`: Rust calls this from its own worker
    // threads, where an isolate-local callback would be undefined behaviour.
    _changeCallable = NativeCallable<ChangeCallbackNative>.listener(
      _changeCallback,
    );
    _ffi.pbMapperSetChangeCallback(_changeCallable!.nativeFunction);
  }

  static void _changeCallback(Pointer<Utf8> payload) {
    try {
      if (payload == nullptr) return;
      final decoded = jsonDecode(payload.toDartString());
      if (decoded is Map<String, dynamic>) {
        _changeController.add(StateChange.fromMap(decoded));
      }
    } catch (_) {
      // A malformed event is not worth taking the app down for; the next one,
      // or the next user action, re-reads everything anyway.
    } finally {
      if (payload != nullptr) {
        PbMapperFFI().pbMapperFreeString(payload);
      }
    }
  }

  void ensureInitialized() {
    if (_disposeRequested) throw StateError('pb-mapper service is disposed');
    if (_handle == null || _handle == nullptr) {
      _handle = _ffi.pbMapperCreate();
    }
  }

  void initLogging() {
    if (_loggingInitialized) return;

    _logCallable = NativeCallable<LogCallbackNative>.listener(_logCallback);
    _ffi.pbMapperSetLogCallback(_logCallable!.nativeFunction);
    _ffi.pbMapperInitLogging();
    _loggingInitialized = true;
    ensureInitialized();
  }

  static void _logCallback(int level, Pointer<Utf8> message, int timestamp) {
    try {
      if (message == nullptr) return;
      final msg = message.toDartString();
      _logController.add(
        LogMessage(
          level: _levelName(level),
          message: msg,
          timestamp: timestamp,
        ),
      );
    } finally {
      if (message != nullptr) {
        PbMapperFFI().pbMapperFreeString(message);
      }
    }
  }

  static String _levelName(int level) {
    switch (level) {
      case 0:
        return 'TRACE';
      case 1:
        return 'DEBUG';
      case 2:
        return 'INFO';
      case 3:
        return 'WARN';
      case 4:
        return 'ERROR';
      default:
        return 'UNKNOWN';
    }
  }

  /// Start accepting commands from `pb_mapper_ui <verb>` in other processes.
  ///
  /// Synchronous and on the main isolate on purpose: it only spawns a listener
  /// onto the Rust runtime and returns, and it has to happen before the window
  /// appears so a command issued moments later is not refused.
  void startControlServer() {
    try {
      final result = _ffi.pbMapperStartControlServer(_requireHandle());
      final decoded = _decodeJsonStatic(_ffi, result);
      if (decoded['success'] != true) {
        debugPrint('control server: ${decoded['message']}');
      }
    } catch (e) {
      debugPrint('control server could not start: $e');
    }
  }

  void dispose() {
    _disposeRequested = true;
    _destroyIfIdle();
    // The tray exits the process immediately after dispose, even when an FFI
    // call still owns the handle. Drain disk logs before that exit as well.
    if (_loggingInitialized) _ffi.pbMapperShutdownLogging();
  }

  // Worker isolates borrow the handle: one still in a call, a tunnel event
  // read waiting on a quiet tunnel among them, must return before it is freed.
  void _destroyIfIdle() {
    if (!_disposeRequested || _activeCalls != 0) return;
    if (_handle != null && _handle != nullptr) {
      _ffi.pbMapperDestroy(_handle!);
    }
    _handle = null;
    _logCallable?.close();
    _logCallable = null;
  }

  Pointer<Void> _requireHandle() {
    ensureInitialized();
    return _handle!;
  }

  // Run FFI calls off the UI isolate to avoid blocking Flutter frames.
  Future<Map<String, dynamic>> _runJsonOnWorker(
    String op,
    Map<String, dynamic> args,
  ) async {
    final handle = _requireHandle();
    _activeCalls++;
    try {
      return await compute(_callJsonIsolate, {
        'op': op,
        'handleAddress': handle.address,
        ...args,
      });
    } finally {
      _activeCalls--;
      _destroyIfIdle();
    }
  }

  Future<Map<String, dynamic>> setAppDirectoryPath(String path) {
    return _runJsonOnWorker('setAppDirectoryPath', {'path': path});
  }

  Future<Map<String, dynamic>> startServer({
    required int port,
    required bool keepAlive,
    int quicPort = 0,
  }) {
    return _runJsonOnWorker('startServer', {
      'port': port,
      'keepAlive': keepAlive,
      'quicPort': quicPort,
    });
  }

  Future<Map<String, dynamic>> stopServer() {
    return _runJsonOnWorker('stopServer', {});
  }

  Future<Map<String, dynamic>> registerService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableEncryption,
    required bool enableKeepAlive,
    required bool allowDirect,
    required int portMapping,
  }) {
    return _runJsonOnWorker('registerService', {
      'serviceKey': serviceKey,
      'localAddress': localAddress,
      'protocol': protocol,
      'enableEncryption': enableEncryption,
      'enableKeepAlive': enableKeepAlive,
      'allowDirect': allowDirect,
      'portMapping': portMapping,
    });
  }

  Future<Map<String, dynamic>> unregisterService(String serviceKey) {
    return _runJsonOnWorker('unregisterService', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> deleteServiceConfig(String serviceKey) {
    return _runJsonOnWorker('deleteServiceConfig', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> connectService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableKeepAlive,
    int? namespace,
    required bool allowDirect,
    required int portMapping,
  }) {
    return _runJsonOnWorker('connectService', {
      'serviceKey': serviceKey,
      'localAddress': localAddress,
      'protocol': protocol,
      'enableKeepAlive': enableKeepAlive,
      'namespace': namespace,
      'allowDirect': allowDirect,
      'portMapping': portMapping,
    });
  }

  Future<Map<String, dynamic>> disconnectService(String serviceKey) {
    return _runJsonOnWorker('disconnectService', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> deleteClientConfig(String serviceKey) {
    return _runJsonOnWorker('deleteClientConfig', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> getConfig() {
    return _runJsonOnWorker('getConfig', {});
  }

  Future<Map<String, dynamic>> revealIsolatedRelayAdminKey() {
    return _runJsonOnWorker('revealIsolatedRelayAdminKey', {});
  }

  Future<Map<String, dynamic>> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
    required String relayTransport,
  }) {
    return _runJsonOnWorker('updateConfig', {
      'serverAddress': serverAddress,
      'keepAlive': keepAlive,
      'msgHeaderKey': msgHeaderKey,
      'relayTransport': relayTransport,
    });
  }

  Future<Map<String, dynamic>> getServiceConfigs() {
    return _runJsonOnWorker('getServiceConfigs', {});
  }

  Future<Map<String, dynamic>> getServiceStatus(String serviceKey) {
    return _runJsonOnWorker('getServiceStatus', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> getServiceConns(String serviceKey) {
    return _runJsonOnWorker('getServiceConns', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> getClientConfigs() {
    return _runJsonOnWorker('getClientConfigs', {});
  }

  Future<Map<String, dynamic>> getClientStatus(String serviceKey) {
    return _runJsonOnWorker('getClientStatus', {'serviceKey': serviceKey});
  }

  Future<Map<String, dynamic>> getLocalServerStatus() {
    return _runJsonOnWorker('getLocalServerStatus', {});
  }

  Future<Map<String, dynamic>> getServerStatusDetail() {
    return _runJsonOnWorker('getServerStatusDetail', {});
  }

  Future<Map<String, dynamic>> forceRefreshServerStatus() {
    return _runJsonOnWorker('forceRefreshServerStatus', {});
  }

  /// A relay status or administrator operation: [request] carries its `op`
  /// and arguments (see `relay.rs`).
  Future<Map<String, dynamic>> relayCall(Map<String, Object?> request) {
    return _runJsonOnWorker('relayCall', {'request': jsonEncode(request)});
  }

  /// A running tunnel's events (see `tunnel_events.rs`), read one at a time
  /// while listened to.
  Stream<Map<String, dynamic>> tunnelEvents({
    required String serviceKey,
    required bool share,
  }) => tunnelEventStream(
    (request) =>
        _runJsonOnWorker('tunnelEvents', {'request': jsonEncode(request)}),
    serviceKey: serviceKey,
    share: share,
  );
}

Map<String, dynamic> _decodeJsonStatic(PbMapperFFI ffi, Pointer<Utf8> ptr) {
  if (ptr == nullptr) {
    return {'success': false, 'message': 'Null response'};
  }

  try {
    final raw = ptr.toDartString();
    if (raw.isEmpty) {
      return {'success': false, 'message': 'Empty response'};
    }
    final decoded = jsonDecode(raw);
    if (decoded is Map<String, dynamic>) {
      return decoded;
    }
    return {'success': false, 'message': 'Invalid JSON response'};
  } catch (e) {
    return {'success': false, 'message': 'JSON decode error: $e'};
  } finally {
    ffi.pbMapperFreeString(ptr);
  }
}

/// The JPEG consumer of one desktop video source, on its own isolate:
/// `next_v1` blocks, so it never runs on the UI isolate. It holds no handle;
/// the video exports take none.
///
/// To the main isolate it sends its control port, then the consumer id (or
/// the failed attach's code), then each picture. It pulls only with a credit:
/// one to start, one more per `true` on the control port; `false` stops it.
Future<void> _desktopJpegWorker((int, SendPort) args) async {
  final (source, main) = args;
  final ffi = PbMapperFFI();
  final control = ReceivePort();
  var credits = 1;
  var stopped = false;
  Completer<void>? wake;
  control.listen((message) {
    if (message == true) {
      credits++;
    } else {
      stopped = true;
    }
    wake?.complete();
    wake = null;
  });
  main.send(control.sendPort);
  final consumer = ffi.pbMapperDesktopVideoAttach(source);
  main.send(consumer);
  if (consumer <= 0) {
    control.close();
    return;
  }
  // MAX_PAYLOAD; NEED_CAPACITY grows it and keeps the unit queued.
  var capacity = 4 << 20;
  var buffer = malloc<Uint8>(capacity);
  final meta = calloc<PbDesktopVideoMetaV1>();
  try {
    while (!stopped) {
      if (credits == 0) {
        await (wake = Completer<void>()).future;
        continue;
      }
      final code = ffi.pbMapperDesktopVideoNext(
        consumer,
        50,
        buffer,
        capacity,
        meta,
      );
      if (code == PbVideo.unit) {
        credits--;
        final unit = meta.ref;
        main.send((
          consumer,
          unit.generation,
          unit.frameId,
          TransferableTypedData.fromList([buffer.asTypedList(unit.len)]),
        ));
      } else if (code == PbVideo.needCapacity) {
        malloc.free(buffer);
        capacity = meta.ref.len;
        buffer = malloc<Uint8>(capacity);
      } else if (code != PbVideo.timeout && code != PbVideo.paused) {
        // CLOSED, NOT_ATTACHED or INVALID: the source is gone.
        break;
      }
      // Lets the control port deliver a credit or the stop between pulls.
      await Future<void>.delayed(Duration.zero);
    }
  } finally {
    ffi.pbMapperDesktopVideoDetach(consumer);
    malloc.free(buffer);
    calloc.free(meta);
    control.close();
  }
}

// Background isolate entry point for synchronous FFI calls.
Map<String, dynamic> _callJsonIsolate(Map<String, dynamic> params) {
  final ffi = PbMapperFFI();
  final handleAddress = params['handleAddress'] as int? ?? 0;
  if (handleAddress == 0) {
    return {'success': false, 'message': 'Handle not initialized'};
  }
  final handle = Pointer<Void>.fromAddress(handleAddress);
  final op = params['op'] as String? ?? '';

  Pointer<Utf8>? arg1;
  Pointer<Utf8>? arg2;
  Pointer<Utf8>? arg3;
  Pointer<Utf8> result = nullptr;

  try {
    switch (op) {
      case 'setAppDirectoryPath':
        final path = params['path'] as String? ?? '';
        arg1 = path.isEmpty ? nullptr : path.toNativeUtf8();
        result = ffi.pbMapperSetAppDir(handle, arg1);
        break;
      case 'startServer':
        result = ffi.pbMapperStartServer(
          handle,
          params['port'] as int,
          (params['keepAlive'] as bool) ? 1 : 0,
          params['quicPort'] as int? ?? 0,
        );
        break;
      case 'stopServer':
        result = ffi.pbMapperStopServer(handle);
        break;
      case 'tunnelEvents':
        arg1 = (params['request'] as String).toNativeUtf8();
        result = ffi.pbMapperTunnelEventsCall(handle, arg1);
        break;
      case 'desktop':
        arg1 = (params['request'] as String).toNativeUtf8();
        result = ffi.pbMapperDesktopCall(handle, arg1);
        break;
      case 'registerService':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        arg2 = (params['localAddress'] as String).toNativeUtf8();
        arg3 = (params['protocol'] as String).toNativeUtf8();
        result = ffi.pbMapperRegisterService(
          handle,
          arg1,
          arg2,
          arg3,
          (params['enableEncryption'] as bool) ? 1 : 0,
          (params['enableKeepAlive'] as bool) ? 1 : 0,
          (params['allowDirect'] as bool) ? 1 : 0,
          params['portMapping'] as int,
        );
        break;
      case 'unregisterService':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperUnregisterService(handle, arg1);
        break;
      case 'deleteServiceConfig':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperDeleteServiceConfig(handle, arg1);
        break;
      case 'connectService':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        arg2 = (params['localAddress'] as String).toNativeUtf8();
        arg3 = (params['protocol'] as String).toNativeUtf8();
        result = ffi.pbMapperConnectService(
          handle,
          arg1,
          arg2,
          arg3,
          (params['enableKeepAlive'] as bool) ? 1 : 0,
          params['namespace'] as int? ?? -1,
          (params['allowDirect'] as bool) ? 1 : 0,
          params['portMapping'] as int,
        );
        break;
      case 'disconnectService':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperDisconnectService(handle, arg1);
        break;
      case 'deleteClientConfig':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperDeleteClientConfig(handle, arg1);
        break;
      case 'getConfig':
        result = ffi.pbMapperGetConfig(handle);
        break;
      case 'revealIsolatedRelayAdminKey':
        result = ffi.pbMapperRevealIsolatedAdminKey(handle);
        break;
      case 'updateConfig':
        arg1 = (params['serverAddress'] as String).toNativeUtf8();
        arg2 = (params['msgHeaderKey'] as String).toNativeUtf8();
        arg3 = (params['relayTransport'] as String? ?? 'tcp').toNativeUtf8();
        result = ffi.pbMapperUpdateConfig(
          handle,
          arg1,
          (params['keepAlive'] as bool) ? 1 : 0,
          arg2,
          arg3,
        );
        break;
      case 'getServiceConfigs':
        result = ffi.pbMapperGetServiceConfigs(handle);
        break;
      case 'getServiceStatus':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperGetServiceStatus(handle, arg1);
        break;
      case 'getServiceConns':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperGetServiceConns(handle, arg1);
        break;
      case 'getClientConfigs':
        result = ffi.pbMapperGetClientConfigs(handle);
        break;
      case 'getClientStatus':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperGetClientStatus(handle, arg1);
        break;
      case 'getLocalServerStatus':
        result = ffi.pbMapperGetLocalServerStatus(handle);
        break;
      case 'getServerStatusDetail':
        result = ffi.pbMapperGetServerStatusDetail(handle);
        break;
      case 'forceRefreshServerStatus':
        result = ffi.pbMapperForceRefreshServerStatus(handle);
        break;
      case 'relayCall':
        arg1 = (params['request'] as String).toNativeUtf8();
        result = ffi.pbMapperRelayCall(handle, arg1);
        break;
      default:
        return {'success': false, 'message': 'Unknown op: $op'};
    }
  } catch (e) {
    return {'success': false, 'message': 'FFI call failed: $e'};
  } finally {
    if (arg1 != null && arg1 != nullptr) {
      calloc.free(arg1);
    }
    if (arg2 != null && arg2 != nullptr) {
      calloc.free(arg2);
    }
    if (arg3 != null && arg3 != nullptr) {
      calloc.free(arg3);
    }
  }

  return _decodeJsonStatic(ffi, result);
}
