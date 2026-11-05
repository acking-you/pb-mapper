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

Map<String, dynamic>? _desktopFrameIsolate(
  (int, int, int) args, {
  bool wait = false,
}) {
  final ffi = PbMapperFFI();
  final read = wait ? ffi.pbMapperDesktopWaitFrame : ffi.pbMapperDesktopFrame;
  final buffer = read(Pointer<Void>.fromAddress(args.$1), args.$2, args.$3);
  if (buffer == nullptr) return null;
  try {
    final bytes = buffer.ref.data.asTypedList(buffer.ref.len);
    if (bytes.length < 4) throw StateError('Invalid desktop frame');
    final length = ByteData.sublistView(
      bytes,
      0,
      4,
    ).getUint32(0, Endian.little);
    if (length > bytes.length - 4) throw StateError('Invalid desktop metadata');
    return {
      'status': jsonDecode(
        utf8.decode(Uint8List.sublistView(bytes, 4, 4 + length)),
      ),
      'bytes': Uint8List.fromList(Uint8List.sublistView(bytes, 4 + length)),
    };
  } finally {
    ffi.pbMapperDesktopFreeFrame(buffer);
  }
}

// One worker per viewer, one outstanding owned frame. Credits prevent the
// isolate mailbox from becoming another unbounded video queue.
Future<void> _desktopFramesWorker((int, SendPort) args) async {
  final requests = ReceivePort();
  var stopped = false, credit = false;
  Completer<void>? ready;
  requests.listen((message) {
    if (message == false) {
      stopped = true;
    } else if (message == true) {
      credit = true;
    } else if (!stopped && message is (int, Map<String, Object?>)) {
      final (id, request) = message;
      try {
        args.$2.send((
          id,
          _callJsonIsolate({
            'op': 'desktop',
            'handleAddress': args.$1,
            'request': jsonEncode(request),
          }),
        ));
      } catch (error) {
        args.$2.send((id, {'success': false, 'message': error.toString()}));
      }
    }
    if (ready?.isCompleted == false) ready!.complete();
  });
  args.$2.send(requests.sendPort);
  var after = 0, generation = 0;
  try {
    while (!stopped) {
      if (!credit) {
        ready = Completer<void>();
        await ready.future;
      }
      if (stopped) break;
      if (!credit) continue;
      final frame = _desktopFrameIsolate((
        args.$1,
        after,
        generation,
      ), wait: true);
      if (frame == null) {
        await Future<void>.delayed(const Duration(milliseconds: 2));
        continue;
      }
      final status = frame['status'] as Map;
      after = status['frame_id'] as int;
      generation = status['generation'] as int;
      credit = false;
      args.$2.send(frame);
    }
  } catch (e) {
    args.$2.send(e.toString());
  } finally {
    requests.close();
    args.$2.send(null);
  }
}

/// Owns one worker until its exit acknowledgement. Cancellation never depends
/// on an async generator reaching another yield, including while it is paused.
@visibleForTesting
class DesktopFrameStream {
  DesktopFrameStream({required this.spawn, required this.onClosed}) {
    _controller = StreamController<Map<String, dynamic>>(
      onListen: _start,
      onPause: () => _paused = true,
      onResume: () {
        _paused = false;
        // Deliver a buffered event before deciding whether another credit is due.
        scheduleMicrotask(_credit);
      },
      onCancel: () {
        stop();
        return _closed.future;
      },
    );
  }
  final Future<Isolate> Function(SendPort, SendPort, SendPort) spawn;
  final void Function() onClosed;
  late final StreamController<Map<String, dynamic>> _controller;
  late final ReceivePort _messages, _exited, _errors;
  final _closed = Completer<void>();
  final _calls =
      <int, (Map<String, Object?>, Completer<Map<String, dynamic>>)>{};
  int _nextCall = 0;
  SendPort? _requests;
  bool _stopped = false,
      _paused = false,
      _outstanding = false,
      _finished = false;
  Stream<Map<String, dynamic>> get stream => _controller.stream;
  bool get acceptsCalls => !_stopped && !_finished;

  /// Short, session-bound operations share the already-running FFI worker.
  /// Connection setup and cancellation never use this serialized mailbox.
  Future<Map<String, dynamic>> call(Map<String, Object?> request) {
    if (!acceptsCalls) {
      return Future.error(StateError('Desktop worker stopped'));
    }
    if (_calls.length >= 16) {
      return Future.error(StateError('Desktop worker request limit reached'));
    }
    final id = ++_nextCall;
    final result = Completer<Map<String, dynamic>>();
    _calls[id] = (request, result);
    _requests?.send((id, request));
    return result.future;
  }

  void stop() {
    _stopped = true;
    _cancelCalls();
    _requests?.send(false);
  }

  void _cancelCalls() {
    for (final pending in _calls.values) {
      pending.$2.completeError(StateError('Desktop worker stopped'));
    }
    _calls.clear();
  }

  void _credit() {
    if (_stopped || _paused || _outstanding || _requests == null) return;
    _outstanding = true;
    _requests!.send(true);
  }

  void _start() {
    _messages = ReceivePort();
    _exited = ReceivePort();
    _errors = ReceivePort();
    _messages.listen((message) {
      if (message is SendPort) {
        _requests = message;
        if (_stopped) {
          message.send(false);
        } else {
          for (final pending in _calls.entries) {
            message.send((pending.key, pending.value.$1));
          }
          _credit();
        }
      } else if (message is (int, Map)) {
        final pending = _calls.remove(message.$1);
        pending?.$2.complete(Map<String, dynamic>.from(message.$2));
      } else if (message is Map) {
        _outstanding = false;
        if (!_stopped) {
          _controller.add(Map<String, dynamic>.from(message));
          scheduleMicrotask(_credit);
        }
      } else if (message is String && !_stopped) {
        _controller.addError(StateError(message));
        stop();
      }
    });
    _errors.listen((message) {
      if (!_stopped) {
        _controller.addError(
          StateError('Desktop frame worker failed: $message'),
        );
      }
      stop();
    });
    _exited.listen((_) => _finish());
    unawaited(() async {
      try {
        await spawn(_messages.sendPort, _exited.sendPort, _errors.sendPort);
      } catch (error, stack) {
        if (!_stopped) _controller.addError(error, stack);
        _finish();
      }
    }());
  }

  void _finish() {
    if (_finished) return;
    _finished = true;
    _stopped = true;
    _cancelCalls();
    _messages.close();
    _exited.close();
    _errors.close();
    onClosed();
    _closed.complete();
    unawaited(_controller.close());
  }
}

class PbMapperService {
  final _desktopFrameWorkers = <DesktopFrameStream>{};
  int _desktopFramesEpoch = 0;
  void stopDesktopFrames() {
    _desktopFramesEpoch++;
    for (final worker in _desktopFrameWorkers) {
      worker.stop();
    }
  }

  Stream<Map<String, dynamic>> desktopFrames() {
    final epoch = _desktopFramesEpoch;
    var pinned = false;
    late DesktopFrameStream reader;
    reader = DesktopFrameStream(
      spawn: (messages, exited, errors) {
        final handle = _requireHandle();
        _activeCalls++;
        pinned = true;
        _desktopFrameWorkers.add(reader);
        if (epoch != _desktopFramesEpoch) reader.stop();
        return Isolate.spawn(
          _desktopFramesWorker,
          (handle.address, messages),
          onExit: exited,
          onError: errors,
          errorsAreFatal: true,
        );
      },
      onClosed: () {
        _desktopFrameWorkers.remove(reader);
        if (pinned) {
          _activeCalls--;
          _destroyIfIdle();
        }
      },
    );
    return reader.stream;
  }

  Future<Map<String, dynamic>> desktopCall(Map<String, Object?> request) {
    if (const {
      'presented',
      'keyframe',
      'input',
      'control',
      'release',
    }.contains(request['op'])) {
      for (final worker in _desktopFrameWorkers) {
        if (worker.acceptsCalls) return worker.call(request);
      }
    }
    return _runJsonOnWorker('desktop', {'request': jsonEncode(request)});
  }

  Future<Map<String, dynamic>?> desktopFrame(int after, int generation) async {
    final handle = _requireHandle();
    _activeCalls++;
    try {
      return await compute(_desktopFrameIsolate, (
        handle.address,
        after,
        generation,
      ));
    } finally {
      _activeCalls--;
      _destroyIfIdle();
    }
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
    ensureInitialized();

    _logCallable = NativeCallable<LogCallbackNative>.listener(_logCallback);
    _ffi.pbMapperSetLogCallback(_logCallable!.nativeFunction);
    _ffi.pbMapperInitLogging();
    _loggingInitialized = true;
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
    stopDesktopFrames();
    _destroyIfIdle();
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
