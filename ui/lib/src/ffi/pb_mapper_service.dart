import 'dart:async';
import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart';

import 'pb_mapper_ffi.dart';

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

class PbMapperService {
  static final PbMapperService _instance = PbMapperService._internal();
  factory PbMapperService() => _instance;
  PbMapperService._internal();

  final PbMapperFFI _ffi = PbMapperFFI();
  Pointer<Void>? _handle;
  bool _loggingInitialized = false;

  static final StreamController<LogMessage> _logController =
      StreamController<LogMessage>.broadcast();
  static Stream<LogMessage> get logStream => _logController.stream;

  static NativeCallable<LogCallbackNative>? _logCallable;

  void ensureInitialized() {
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

  void dispose() {
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
    return compute(_callJsonIsolate, {
      'op': op,
      'handleAddress': handle.address,
      ...args,
    });
  }

  Future<Map<String, dynamic>> setAppDirectoryPath(String path) {
    return _runJsonOnWorker('setAppDirectoryPath', {'path': path});
  }

  Future<Map<String, dynamic>> startServer({
    required int port,
    required bool keepAlive,
  }) {
    return _runJsonOnWorker('startServer', {
      'port': port,
      'keepAlive': keepAlive,
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
  }) {
    return _runJsonOnWorker('registerService', {
      'serviceKey': serviceKey,
      'localAddress': localAddress,
      'protocol': protocol,
      'enableEncryption': enableEncryption,
      'enableKeepAlive': enableKeepAlive,
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
  }) {
    return _runJsonOnWorker('connectService', {
      'serviceKey': serviceKey,
      'localAddress': localAddress,
      'protocol': protocol,
      'enableKeepAlive': enableKeepAlive,
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

  Future<Map<String, dynamic>> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
  }) {
    return _runJsonOnWorker('updateConfig', {
      'serverAddress': serverAddress,
      'keepAlive': keepAlive,
      'msgHeaderKey': msgHeaderKey,
    });
  }

  Future<Map<String, dynamic>> getServiceConfigs() {
    return _runJsonOnWorker('getServiceConfigs', {});
  }

  Future<Map<String, dynamic>> getServiceStatus(String serviceKey) {
    return _runJsonOnWorker('getServiceStatus', {'serviceKey': serviceKey});
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
        );
        break;
      case 'stopServer':
        result = ffi.pbMapperStopServer(handle);
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
      case 'updateConfig':
        arg1 = (params['serverAddress'] as String).toNativeUtf8();
        arg2 = (params['msgHeaderKey'] as String).toNativeUtf8();
        result = ffi.pbMapperUpdateConfig(
          handle,
          arg1,
          (params['keepAlive'] as bool) ? 1 : 0,
          arg2,
        );
        break;
      case 'getServiceConfigs':
        result = ffi.pbMapperGetServiceConfigs(handle);
        break;
      case 'getServiceStatus':
        arg1 = (params['serviceKey'] as String).toNativeUtf8();
        result = ffi.pbMapperGetServiceStatus(handle, arg1);
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
