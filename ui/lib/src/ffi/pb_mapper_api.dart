// Typed API wrapper around the raw FFI service for UI usage.
// This replaces the old generated bindings layer and keeps UI code simple.

import 'dart:async';

import 'pb_mapper_service.dart';

class OperationResult {
  final bool success;
  final String message;

  const OperationResult({required this.success, required this.message});
}

class ConfigStatus {
  final String serverAddress;
  final bool keepAliveEnabled;
  final String msgHeaderKey;
  final bool isolatedRelayAdminKeySet;
  final String isolatedRelayAdminKey;

  const ConfigStatus({
    required this.serverAddress,
    required this.keepAliveEnabled,
    required this.msgHeaderKey,
    this.isolatedRelayAdminKeySet = false,
    this.isolatedRelayAdminKey = '',
  });

  factory ConfigStatus.fromMap(Map<String, dynamic> map) {
    return ConfigStatus(
      serverAddress: _asString(
        map['serverAddress'],
        fallback: 'localhost:7666',
      ),
      keepAliveEnabled: _asBool(map['keepAliveEnabled'], fallback: true),
      msgHeaderKey: _asString(map['msgHeaderKey']),
      isolatedRelayAdminKeySet: _asBool(
        map['isolatedRelayAdminKeySet'],
        fallback: false,
      ),
      isolatedRelayAdminKey: _asString(map['isolatedRelayAdminKey']),
    );
  }
}

class ServiceConfigInfo {
  final String serviceKey;
  final String localAddress;
  final String protocol;
  final bool enableEncryption;
  final bool enableKeepAlive;
  final String status;
  final String statusMessage;
  final int createdAtMs;
  final int updatedAtMs;

  const ServiceConfigInfo({
    required this.serviceKey,
    required this.localAddress,
    required this.protocol,
    required this.enableEncryption,
    required this.enableKeepAlive,
    required this.status,
    required this.statusMessage,
    required this.createdAtMs,
    required this.updatedAtMs,
  });

  factory ServiceConfigInfo.fromMap(Map<String, dynamic> map) {
    return ServiceConfigInfo(
      serviceKey: _asString(map['serviceKey']),
      localAddress: _asString(map['localAddress']),
      protocol: _asString(map['protocol'], fallback: 'TCP'),
      enableEncryption: _asBool(map['enableEncryption']),
      enableKeepAlive: _asBool(map['enableKeepAlive']),
      status: _asString(map['status'], fallback: 'stopped'),
      statusMessage: _asString(map['statusMessage']),
      createdAtMs: _asInt(map['createdAtMs']),
      updatedAtMs: _asInt(map['updatedAtMs']),
    );
  }
}

class ClientConfigInfo {
  final String serviceKey;
  final String localAddress;
  final String protocol;
  final bool enableKeepAlive;
  final String status;
  final String statusMessage;
  final int createdAtMs;
  final int updatedAtMs;

  const ClientConfigInfo({
    required this.serviceKey,
    required this.localAddress,
    required this.protocol,
    required this.enableKeepAlive,
    required this.status,
    required this.statusMessage,
    required this.createdAtMs,
    required this.updatedAtMs,
  });

  factory ClientConfigInfo.fromMap(Map<String, dynamic> map) {
    return ClientConfigInfo(
      serviceKey: _asString(map['serviceKey']),
      localAddress: _asString(map['localAddress']),
      protocol: _asString(map['protocol'], fallback: 'TCP'),
      enableKeepAlive: _asBool(map['enableKeepAlive']),
      status: _asString(map['status'], fallback: 'stopped'),
      statusMessage: _asString(map['statusMessage']),
      createdAtMs: _asInt(map['createdAtMs']),
      updatedAtMs: _asInt(map['updatedAtMs']),
    );
  }
}

class LocalServerStatus {
  final bool isRunning;
  final int activeConnections;
  final int registeredServices;
  final int uptimeSeconds;

  const LocalServerStatus({
    required this.isRunning,
    required this.activeConnections,
    required this.registeredServices,
    required this.uptimeSeconds,
  });

  factory LocalServerStatus.fromMap(Map<String, dynamic> map) {
    return LocalServerStatus(
      isRunning: _asBool(map['isRunning']),
      activeConnections: _asInt(map['activeConnections']),
      registeredServices: _asInt(map['registeredServices']),
      uptimeSeconds: _asInt(map['uptimeSeconds']),
    );
  }
}

class ServerStatusDetail {
  final bool serverAvailable;
  final List<String> registeredServices;
  final String serverMap;
  final String activeConnections;
  final String idleConnections;

  const ServerStatusDetail({
    required this.serverAvailable,
    required this.registeredServices,
    required this.serverMap,
    required this.activeConnections,
    required this.idleConnections,
  });

  factory ServerStatusDetail.fromMap(Map<String, dynamic> map) {
    return ServerStatusDetail(
      serverAvailable: _asBool(map['serverAvailable']),
      registeredServices: _asStringList(map['registeredServices']),
      serverMap: _asString(map['serverMap']),
      activeConnections: _asString(map['activeConnections']),
      idleConnections: _asString(map['idleConnections']),
    );
  }
}

class ServiceStatusSignal {
  final String serviceKey;
  final String status;
  final String message;

  const ServiceStatusSignal({
    required this.serviceKey,
    required this.status,
    required this.message,
  });

  factory ServiceStatusSignal.fromMap(Map<String, dynamic> map) {
    return ServiceStatusSignal(
      serviceKey: _asString(map['serviceKey']),
      status: _asString(map['status'], fallback: 'stopped'),
      message: _asString(map['message']),
    );
  }
}

class ClientStatusSignal {
  final String serviceKey;
  final String status;
  final String message;

  const ClientStatusSignal({
    required this.serviceKey,
    required this.status,
    required this.message,
  });

  factory ClientStatusSignal.fromMap(Map<String, dynamic> map) {
    return ClientStatusSignal(
      serviceKey: _asString(map['serviceKey']),
      status: _asString(map['status'], fallback: 'stopped'),
      message: _asString(map['message']),
    );
  }
}

/// One control connection the server is holding for a service key.
///
/// This is what the status page's "Server Map" is really made of. That section
/// used to render `serverMap`, a `format!("{map:?}")` of the whole thing on the
/// server side — every one of these fields was in there, flattened into a
/// Debug dump nothing could read.
class ServiceConnInfo {
  final int connId;
  final int generation;
  final int protocolVersion;
  final bool healthy;
  final Duration lastRxAge;

  const ServiceConnInfo({
    required this.connId,
    required this.generation,
    required this.protocolVersion,
    required this.healthy,
    required this.lastRxAge,
  });

  factory ServiceConnInfo.fromMap(Map<String, dynamic> map) => ServiceConnInfo(
    connId: _asInt(map['connId']),
    generation: _asInt(map['generation']),
    protocolVersion: _asInt(map['protocolVersion']),
    healthy: _asBool(map['healthy']),
    lastRxAge: Duration(milliseconds: _asInt(map['lastRxAgeMs'])),
  );
}

/// Everything the UI asks of the Rust side.
///
/// The views take one of these rather than reaching for [PbMapperApi], which
/// is a singleton over `dart:ffi` and so drags a loaded native library into
/// anything that touches it. That is why the view layer had no tests at all:
/// there was nothing to put in its place.
///
/// An interface rather than a subclassable base class on purpose. A fake that
/// forgets a method fails to compile here; a fake that inherited from the real
/// class would quietly fall through to the FFI instead.
abstract interface class PbMapperApiClient {
  Future<OperationResult> setAppDirectoryPath(String path);

  Future<ConfigStatus> fetchConfig();
  Future<String> revealIsolatedRelayAdminKey();
  Future<OperationResult> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
  });

  Future<OperationResult> startServer({
    required int port,
    required bool keepAlive,
  });
  Future<OperationResult> stopServer();
  Future<LocalServerStatus> getLocalServerStatus();
  Future<ServerStatusDetail> getServerStatusDetail();
  Future<ServerStatusDetail> forceRefreshServerStatus();

  Future<List<ServiceConfigInfo>> getServiceConfigs();
  Future<ServiceStatusSignal> getServiceStatus(String serviceKey);

  /// What the remote server is holding for [serviceKey], structured.
  Future<List<ServiceConnInfo>> getServiceConns(String serviceKey);

  Future<OperationResult> registerService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableEncryption,
    required bool enableKeepAlive,
  });
  Future<OperationResult> unregisterService(String serviceKey);
  Future<OperationResult> deleteServiceConfig(String serviceKey);

  Future<List<ClientConfigInfo>> getClientConfigs();
  Future<ClientStatusSignal> getClientStatus(String serviceKey);
  Future<OperationResult> connectService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableKeepAlive,
  });
  Future<OperationResult> disconnectService(String serviceKey);
  Future<OperationResult> deleteClientConfig(String serviceKey);
}

class PbMapperApi implements PbMapperApiClient {
  static final PbMapperApi _instance = PbMapperApi._internal();
  factory PbMapperApi() => _instance;
  PbMapperApi._internal();

  final PbMapperService _service = PbMapperService();

  @override
  Future<OperationResult> setAppDirectoryPath(String path) async {
    final result = await _service.setAppDirectoryPath(path);
    return _resultFrom(result);
  }

  @override
  Future<ConfigStatus> fetchConfig() async {
    final result = await _service.getConfig();
    if (result['success'] == true) {
      return ConfigStatus.fromMap(_asMap(result['data']));
    }
    return const ConfigStatus(
      serverAddress: 'localhost:7666',
      keepAliveEnabled: true,
      msgHeaderKey: '',
    );
  }

  @override
  Future<String> revealIsolatedRelayAdminKey() async {
    final result = await _service.revealIsolatedRelayAdminKey();
    if (result['success'] == true) {
      return _asString(_asMap(result['data'])['isolatedRelayAdminKey']);
    }
    return '';
  }

  @override
  Future<OperationResult> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
  }) async {
    final result = await _service.updateConfig(
      serverAddress: serverAddress,
      keepAlive: keepAlive,
      msgHeaderKey: msgHeaderKey,
    );
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> startServer({
    required int port,
    required bool keepAlive,
  }) async {
    final result = await _service.startServer(port: port, keepAlive: keepAlive);
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> stopServer() async {
    final result = await _service.stopServer();
    return _resultFrom(result);
  }

  @override
  Future<LocalServerStatus> getLocalServerStatus() async {
    final result = await _service.getLocalServerStatus();
    if (result['success'] == true) {
      return LocalServerStatus.fromMap(_asMap(result['data']));
    }
    return const LocalServerStatus(
      isRunning: false,
      activeConnections: 0,
      registeredServices: 0,
      uptimeSeconds: 0,
    );
  }

  @override
  Future<ServerStatusDetail> getServerStatusDetail() async {
    final result = await _service.getServerStatusDetail();
    if (result['success'] == true) {
      return ServerStatusDetail.fromMap(_asMap(result['data']));
    }
    return const ServerStatusDetail(
      serverAvailable: false,
      registeredServices: [],
      serverMap: '',
      activeConnections: '',
      idleConnections: '',
    );
  }

  @override
  Future<ServerStatusDetail> forceRefreshServerStatus() async {
    final result = await _service.forceRefreshServerStatus();
    if (result['success'] == true) {
      return ServerStatusDetail.fromMap(_asMap(result['data']));
    }
    return const ServerStatusDetail(
      serverAvailable: false,
      registeredServices: [],
      serverMap: '',
      activeConnections: '',
      idleConnections: '',
    );
  }

  @override
  Future<List<ServiceConnInfo>> getServiceConns(String serviceKey) async {
    final result = await _service.getServiceConns(serviceKey);
    // `data` is the array itself here, not an object wrapping one: the Rust
    // side serialises the Vec straight through.
    if (result['success'] == true && result['data'] is List) {
      return (result['data'] as List)
          .whereType<Map>()
          .map(
            (item) => ServiceConnInfo.fromMap(Map<String, dynamic>.from(item)),
          )
          .toList();
    }
    return [];
  }

  @override
  Future<List<ServiceConfigInfo>> getServiceConfigs() async {
    final result = await _service.getServiceConfigs();
    if (result['success'] == true) {
      final data = _asMap(result['data']);
      final servicesRaw = data['services'];
      if (servicesRaw is List) {
        return servicesRaw
            .map(
              (item) => item is Map<String, dynamic>
                  ? ServiceConfigInfo.fromMap(item)
                  : ServiceConfigInfo.fromMap(
                      Map<String, dynamic>.from(item as Map),
                    ),
            )
            .toList();
      }
    }
    return [];
  }

  @override
  Future<ServiceStatusSignal> getServiceStatus(String serviceKey) async {
    final result = await _service.getServiceStatus(serviceKey);
    if (result['success'] == true) {
      return ServiceStatusSignal.fromMap(_asMap(result['data']));
    }
    return ServiceStatusSignal(
      serviceKey: serviceKey,
      status: 'failed',
      message: _asString(result['message']),
    );
  }

  @override
  Future<OperationResult> registerService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableEncryption,
    required bool enableKeepAlive,
  }) async {
    final result = await _service.registerService(
      serviceKey: serviceKey,
      localAddress: localAddress,
      protocol: protocol,
      enableEncryption: enableEncryption,
      enableKeepAlive: enableKeepAlive,
    );
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> unregisterService(String serviceKey) async {
    final result = await _service.unregisterService(serviceKey);
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> deleteServiceConfig(String serviceKey) async {
    final result = await _service.deleteServiceConfig(serviceKey);
    return _resultFrom(result);
  }

  @override
  Future<List<ClientConfigInfo>> getClientConfigs() async {
    final result = await _service.getClientConfigs();
    if (result['success'] == true) {
      final data = _asMap(result['data']);
      final clientsRaw = data['clients'];
      if (clientsRaw is List) {
        return clientsRaw
            .map(
              (item) => item is Map<String, dynamic>
                  ? ClientConfigInfo.fromMap(item)
                  : ClientConfigInfo.fromMap(
                      Map<String, dynamic>.from(item as Map),
                    ),
            )
            .toList();
      }
    }
    return [];
  }

  @override
  Future<ClientStatusSignal> getClientStatus(String serviceKey) async {
    final result = await _service.getClientStatus(serviceKey);
    if (result['success'] == true) {
      return ClientStatusSignal.fromMap(_asMap(result['data']));
    }
    return ClientStatusSignal(
      serviceKey: serviceKey,
      status: 'failed',
      message: _asString(result['message']),
    );
  }

  @override
  Future<OperationResult> connectService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableKeepAlive,
  }) async {
    final result = await _service.connectService(
      serviceKey: serviceKey,
      localAddress: localAddress,
      protocol: protocol,
      enableKeepAlive: enableKeepAlive,
    );
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> disconnectService(String serviceKey) async {
    final result = await _service.disconnectService(serviceKey);
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> deleteClientConfig(String serviceKey) async {
    final result = await _service.deleteClientConfig(serviceKey);
    return _resultFrom(result);
  }

  OperationResult _resultFrom(Map<String, dynamic> result) {
    return OperationResult(
      success: result['success'] == true,
      message: _asString(result['message'], fallback: 'Unknown result'),
    );
  }
}

Map<String, dynamic> _asMap(dynamic value) {
  if (value is Map<String, dynamic>) {
    return value;
  }
  if (value is Map) {
    return Map<String, dynamic>.from(value);
  }
  return <String, dynamic>{};
}

String _asString(dynamic value, {String fallback = ''}) {
  if (value == null) return fallback;
  if (value is String) return value;
  return value.toString();
}

bool _asBool(dynamic value, {bool fallback = false}) {
  if (value is bool) return value;
  if (value is num) return value != 0;
  if (value is String) return value.toLowerCase() == 'true';
  return fallback;
}

int _asInt(dynamic value, {int fallback = 0}) {
  if (value is int) return value;
  if (value is num) return value.toInt();
  if (value is String) return int.tryParse(value) ?? fallback;
  return fallback;
}

List<String> _asStringList(dynamic value) {
  if (value is List) {
    return value.map((item) => _asString(item)).toList();
  }
  return const [];
}
