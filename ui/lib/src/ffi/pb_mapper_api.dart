// Typed API wrapper around the raw FFI service for UI usage.
// This replaces the old generated bindings layer and keeps UI code simple.

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:pb_mapper_ui/src/common/polling.dart' show FirstWhereOrNull;

import 'pb_mapper_service.dart';
import 'relay_models.dart';

export 'relay_models.dart';

class OperationResult {
  final bool success;
  final String message;

  /// What a successful call answered besides its word, when it did.
  final Map<String, dynamic> data;

  const OperationResult({
    required this.success,
    required this.message,
    this.data = const {},
  });
}

/// What saving the relay settings left to do: this computer's tunnels on
/// that relay, under that credential, still on the carrier it had before.
/// Each keeps it until started again.
class CarrierMove {
  const CarrierMove({this.shares = const [], this.connections = const []});

  final List<String> shares;
  final List<String> connections;

  bool get isEmpty => shares.isEmpty && connections.isEmpty;
  int get length => shares.length + connections.length;

  factory CarrierMove.fromMap(Object? raw) {
    final map = raw is Map ? raw : const {};
    return CarrierMove(
      shares: _asStringList(map['shares']),
      connections: _asStringList(map['connections']),
    );
  }
}

class ConfigStatus {
  final String serverAddress;
  final bool keepAliveEnabled;
  final String msgHeaderKey;

  /// How this computer reaches the relay.
  final RelayTransport relayTransport;
  final bool isolatedRelayAdminKeySet;
  final String isolatedRelayAdminKey;

  const ConfigStatus({
    required this.serverAddress,
    required this.keepAliveEnabled,
    required this.msgHeaderKey,
    this.relayTransport = RelayTransport.tcp,
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
      relayTransport: RelayTransport.parse(map['relayTransport']),
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

  /// Whether connections may reach it directly rather than through the relay.
  final bool allowDirect;
  final String status;
  final String statusMessage;

  /// How its sessions travel, while it runs.
  final TunnelPath? path;

  /// How it reaches the relay, while it runs.
  final RelayTransport? relayTransport;
  final int createdAtMs;
  final int updatedAtMs;

  const ServiceConfigInfo({
    required this.serviceKey,
    required this.localAddress,
    required this.protocol,
    required this.enableEncryption,
    required this.enableKeepAlive,
    this.allowDirect = true,
    required this.status,
    required this.statusMessage,
    this.path,
    this.relayTransport,
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
      allowDirect: _asBool(map['allowDirect'], fallback: true),
      status: _asString(map['status'], fallback: 'stopped'),
      statusMessage: _asString(map['statusMessage']),
      path: TunnelPath.maybeFrom(map['path']),
      relayTransport: RelayTransport.maybe(map['relayTransport']),
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

  /// The temporary credential whose service it connects to, when it is not
  /// this credential's own.
  final int? namespace;

  /// While it runs, the namespace the relay routes it in, from the credential
  /// it was started with: a credential changed since does not move it.
  final int? routedNamespace;

  /// Whether each session tries a direct path before the relay.
  final bool allowDirect;
  final String status;
  final String statusMessage;

  /// How its sessions travel, while it runs.
  final TunnelPath? path;

  /// How it reaches the relay, while it runs.
  final RelayTransport? relayTransport;
  final int createdAtMs;
  final int updatedAtMs;

  const ClientConfigInfo({
    required this.serviceKey,
    required this.localAddress,
    required this.protocol,
    required this.enableKeepAlive,
    this.namespace,
    this.routedNamespace,
    this.allowDirect = true,
    required this.status,
    required this.statusMessage,
    this.path,
    this.relayTransport,
    required this.createdAtMs,
    required this.updatedAtMs,
  });

  factory ClientConfigInfo.fromMap(Map<String, dynamic> map) {
    return ClientConfigInfo(
      serviceKey: _asString(map['serviceKey']),
      localAddress: _asString(map['localAddress']),
      protocol: _asString(map['protocol'], fallback: 'TCP'),
      enableKeepAlive: _asBool(map['enableKeepAlive']),
      namespace: map['namespace'] == null ? null : _asInt(map['namespace']),
      routedNamespace: map['routedNamespace'] == null
          ? null
          : _asInt(map['routedNamespace']),
      allowDirect: _asBool(map['allowDirect'], fallback: true),
      status: _asString(map['status'], fallback: 'stopped'),
      statusMessage: _asString(map['statusMessage']),
      path: TunnelPath.maybeFrom(map['path']),
      relayTransport: RelayTransport.maybe(map['relayTransport']),
      createdAtMs: _asInt(map['createdAtMs']),
      updatedAtMs: _asInt(map['updatedAtMs']),
    );
  }
}

/// How far a direct attempt got. Every stage before [committing] can still
/// give way to the relay.
enum DirectStage {
  /// Asking the relay whether the sharing computer offers direct.
  negotiating,

  /// Learning this computer's public address, and asking for a session.
  discovering,

  /// Dialling the sharing computer through both networks' NATs.
  punching,

  /// The path is up; the sharing computer is opening its service.
  committing;

  static DirectStage? parse(Object? raw) =>
      DirectStage.values.firstWhereOrNull((stage) => stage.name == raw);
}

/// How a running tunnel's sessions travel: each TCP connection or UDP flow
/// through it goes direct or through the relay, decided as it opens. A
/// connection counts both and its attempts; a share counts only the
/// sessions that reach it directly.
@immutable
class TunnelPath {
  const TunnelPath({
    this.activeDirect = 0,
    this.activeRelay = 0,
    this.directSessions = 0,
    this.relaySessions = 0,
    this.attempt,
    this.lastDirect,
    this.setupMs,
    this.fallbackStage,
  });

  /// Sessions open now, on each path.
  final int activeDirect;
  final int activeRelay;

  /// Sessions opened since the tunnel started, on each path.
  final int directSessions;
  final int relaySessions;

  /// The stage reached by a direct attempt in flight.
  final DirectStage? attempt;

  /// How the latest attempt ended: direct, the relay, or none yet.
  final bool? lastDirect;

  /// How long the latest direct path took to set up.
  final int? setupMs;

  /// Where the latest attempt gave way to the relay. The tunnel then sends
  /// its sessions straight to the relay for a while before trying again.
  final DirectStage? fallbackStage;

  static TunnelPath? maybeFrom(Object? raw) {
    if (raw is! Map) return null;
    final last = raw['last'];
    final lastPath = last is Map ? last['path'] : null;
    return TunnelPath(
      activeDirect: _asInt(raw['activeDirect']),
      activeRelay: _asInt(raw['activeRelay']),
      directSessions: _asInt(raw['directSessions']),
      relaySessions: _asInt(raw['relaySessions']),
      attempt: DirectStage.parse(raw['attempt']),
      lastDirect: switch (lastPath) {
        'direct' => true,
        'relay' => false,
        _ => null,
      },
      setupMs: last is Map && last['setupMs'] != null
          ? _asInt(last['setupMs'])
          : null,
      fallbackStage: last is Map ? DirectStage.parse(last['stage']) : null,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is TunnelPath &&
      other.activeDirect == activeDirect &&
      other.activeRelay == activeRelay &&
      other.directSessions == directSessions &&
      other.relaySessions == relaySessions &&
      other.attempt == attempt &&
      other.lastDirect == lastDirect &&
      other.setupMs == setupMs &&
      other.fallbackStage == fallbackStage;

  @override
  int get hashCode => Object.hash(
    activeDirect,
    activeRelay,
    directSessions,
    relaySessions,
    attempt,
    lastDirect,
    setupMs,
    fallbackStage,
  );
}

class LocalServerStatus {
  final bool isRunning;

  /// The port it listens on, while it runs.
  final int? port;

  /// The UDP port it takes QUIC on, while it runs with one.
  final int? quicPort;
  final int activeConnections;
  final int registeredServices;
  final int uptimeSeconds;

  const LocalServerStatus({
    required this.isRunning,
    this.port,
    this.quicPort,
    required this.activeConnections,
    required this.registeredServices,
    required this.uptimeSeconds,
  });

  factory LocalServerStatus.fromMap(Map<String, dynamic> map) {
    return LocalServerStatus(
      isRunning: _asBool(map['isRunning']),
      port: map['port'] == null ? null : _asInt(map['port']),
      quicPort: map['quicPort'] == null ? null : _asInt(map['quicPort']),
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

  /// Why the relay did not answer.
  final String? error;

  /// How long it took to answer, when it did.
  final int? latencyMs;

  /// The UDP port it offers QUIC on, when it said it offers it.
  final int? quicPort;

  /// Whether it said whether it offers QUIC. It is asked over TCP, so this
  /// holds even when QUIC itself does not get through.
  final bool quicKnown;

  const ServerStatusDetail({
    required this.serverAvailable,
    required this.registeredServices,
    required this.serverMap,
    required this.activeConnections,
    required this.idleConnections,
    this.error,
    this.latencyMs,
    this.quicPort,
    this.quicKnown = false,
  });

  factory ServerStatusDetail.fromMap(Map<String, dynamic> map) {
    return ServerStatusDetail(
      serverAvailable: _asBool(map['serverAvailable']),
      registeredServices: _asStringList(map['registeredServices']),
      serverMap: _asString(map['serverMap']),
      activeConnections: _asString(map['activeConnections']),
      idleConnections: _asString(map['idleConnections']),
      error: map['error'] as String?,
      latencyMs: map['latencyMs'] == null ? null : _asInt(map['latencyMs']),
      quicPort: map['quicPort'] == null ? null : _asInt(map['quicPort']),
      quicKnown: _asBool(map['quicKnown']),
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

  /// Save the relay settings. Its [OperationResult.data] carries a
  /// [CarrierMove] under `move` when the carrier changed.
  Future<OperationResult> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
    RelayTransport relayTransport = RelayTransport.tcp,
  });

  /// Run a relay on [port]; with [quicPort], one that also takes QUIC there.
  Future<OperationResult> startServer({
    required int port,
    required bool keepAlive,
    int? quicPort,
  });
  Future<OperationResult> stopServer();
  Future<LocalServerStatus> getLocalServerStatus();
  Future<ServerStatusDetail> getServerStatusDetail();
  Future<ServerStatusDetail> forceRefreshServerStatus();

  Future<List<ServiceConfigInfo>> getServiceConfigs();
  Future<ServiceStatusSignal> getServiceStatus(String serviceKey);

  /// What the remote server is holding for [serviceKey], structured.
  Future<List<ServiceConnInfo>> getServiceConns(String serviceKey);

  /// Share [serviceKey]. With [allowDirect], connections may reach it
  /// directly rather than through the relay.
  Future<OperationResult> registerService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableEncryption,
    required bool enableKeepAlive,
    bool allowDirect = true,
  });
  Future<OperationResult> unregisterService(String serviceKey);
  Future<OperationResult> deleteServiceConfig(String serviceKey);

  Future<List<ClientConfigInfo>> getClientConfigs();
  Future<ClientStatusSignal> getClientStatus(String serviceKey);

  /// Connect to [serviceKey]. [namespace] is the temporary credential whose
  /// service it is, which only the administrator key may name; null for this
  /// credential's own. With [allowDirect] each session tries a direct path
  /// before the relay.
  Future<OperationResult> connectService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableKeepAlive,
    int? namespace,
    bool allowDirect = true,
  });
  Future<OperationResult> disconnectService(String serviceKey);
  Future<OperationResult> deleteClientConfig(String serviceKey);

  // --- The relay as a whole (`pb_mapper_relay_call_json`). Each throws a
  // [RelayCallException] when the relay or the credential says no.

  /// What this computer's credential makes it, to the relay.
  Future<RelayIdentity> relayIdentity();

  /// Direct sessions the relay has heard about, in this credential's view.
  Future<List<DirectSession>> relaySessions();

  /// How this computer reaches the relay now, from the connections it holds.
  Future<RelayLink> relayLink();

  // Administrator key only.
  Future<RelayAuthStatus> adminAuthStatus();
  Future<List<RelayService>> adminServices();
  Future<List<RelayConnection>> adminConnections();
  Future<List<RelayKey>> adminKeys();
  Future<IssuedCredential> adminIssueKey({
    required Duration ttl,
    String? label,
  });
  Future<IssuedCredential> adminRevealKey(int keyId);
  Future<IssuedCredential> adminRenewKey(int keyId, Duration ttl);
  Future<RelayKey> adminRevokeKey(int keyId);

  /// Forget expired and revoked credentials; returns how many.
  Future<int> adminCleanUpKeys();

  /// Drop the control connections the relay holds for [service] in
  /// [namespace] (just [connId], when given); returns how many it dropped.
  Future<int> adminRetireConnections({
    required int namespace,
    required String service,
    int? connId,
  });
  Future<void> adminSetLegacyProtocol({required bool allow});

  /// Erase every temporary credential the relay knows.
  Future<void> adminResetCredentials();

  /// Rotate the administrator key to a generated one. This computer
  /// switches to it once the relay is known to have taken it.
  Future<KeyRotation> adminRotateKey();
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
    RelayTransport relayTransport = RelayTransport.tcp,
  }) async {
    final result = await _service.updateConfig(
      serverAddress: serverAddress,
      keepAlive: keepAlive,
      msgHeaderKey: msgHeaderKey,
      relayTransport: relayTransport.name,
    );
    return _resultFrom(result);
  }

  @override
  Future<OperationResult> startServer({
    required int port,
    required bool keepAlive,
    int? quicPort,
  }) async {
    final result = await _service.startServer(
      port: port,
      keepAlive: keepAlive,
      quicPort: quicPort ?? 0,
    );
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
  Future<ServerStatusDetail> getServerStatusDetail() async =>
      _statusDetail(await _service.getServerStatusDetail());

  @override
  Future<ServerStatusDetail> forceRefreshServerStatus() async =>
      _statusDetail(await _service.forceRefreshServerStatus());

  /// The relay's status, or why it could not be read: the FFI's message is
  /// a sentence written for a person, and Settings' Test shows it.
  static ServerStatusDetail _statusDetail(Map<String, dynamic> result) {
    if (result['success'] == true) {
      return ServerStatusDetail.fromMap(_asMap(result['data']));
    }
    final message = _asString(result['message']).trim();
    return ServerStatusDetail(
      serverAvailable: false,
      registeredServices: const [],
      serverMap: '',
      activeConnections: '',
      idleConnections: '',
      error: message.isEmpty ? null : message,
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
    bool allowDirect = true,
  }) async {
    final result = await _service.registerService(
      serviceKey: serviceKey,
      localAddress: localAddress,
      protocol: protocol,
      enableEncryption: enableEncryption,
      enableKeepAlive: enableKeepAlive,
      allowDirect: allowDirect,
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
    int? namespace,
    bool allowDirect = true,
  }) async {
    final result = await _service.connectService(
      serviceKey: serviceKey,
      localAddress: localAddress,
      protocol: protocol,
      enableKeepAlive: enableKeepAlive,
      namespace: namespace,
      allowDirect: allowDirect,
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
      data: _asMap(result['data']),
    );
  }

  /// One relay operation's `data`, or its failure as a
  /// [RelayCallException].
  Future<dynamic> _relay(
    String op, [
    Map<String, Object?> args = const {},
  ]) async {
    final result = await _service.relayCall({'op': op, ...args});
    if (result['success'] != true) {
      throw RelayCallException(
        _asString(result['message'], fallback: 'The relay call failed'),
        code: _asString(result['code'], fallback: 'INTERNAL'),
      );
    }
    return result['data'];
  }

  Future<List<T>> _relayList<T>(
    String op,
    T Function(Map<String, dynamic>) parse,
  ) async {
    final data = await _relay(op);
    return [
      if (data is List)
        for (final item in data.whereType<Map>()) parse(_asMap(item)),
    ];
  }

  @override
  Future<RelayIdentity> relayIdentity() async =>
      RelayIdentity.fromMap(_asMap(await _relay('credential')));

  @override
  Future<List<DirectSession>> relaySessions() =>
      _relayList('sessions', DirectSession.fromMap);

  @override
  Future<RelayLink> relayLink() async =>
      RelayLink.fromMap(_asMap(await _relay('link')));

  @override
  Future<RelayAuthStatus> adminAuthStatus() async =>
      RelayAuthStatus.fromMap(_asMap(await _relay('auth_status')));

  @override
  Future<List<RelayService>> adminServices() =>
      _relayList('services', RelayService.fromMap);

  @override
  Future<List<RelayConnection>> adminConnections() =>
      _relayList('connections', RelayConnection.fromMap);

  @override
  Future<List<RelayKey>> adminKeys() => _relayList('keys', RelayKey.fromMap);

  @override
  Future<IssuedCredential> adminIssueKey({
    required Duration ttl,
    String? label,
  }) async => IssuedCredential.fromMap(
    _asMap(
      await _relay('key_issue', {'ttlSeconds': ttl.inSeconds, 'label': label}),
    ),
  );

  @override
  Future<IssuedCredential> adminRevealKey(int keyId) async =>
      IssuedCredential.fromMap(
        _asMap(await _relay('key_reveal', {'keyId': keyId})),
      );

  @override
  Future<IssuedCredential> adminRenewKey(int keyId, Duration ttl) async =>
      IssuedCredential.fromMap(
        _asMap(
          await _relay('key_renew', {
            'keyId': keyId,
            'ttlSeconds': ttl.inSeconds,
          }),
        ),
      );

  @override
  Future<RelayKey> adminRevokeKey(int keyId) async =>
      RelayKey.fromMap(_asMap(await _relay('key_revoke', {'keyId': keyId})));

  @override
  Future<int> adminCleanUpKeys() async =>
      _asInt(_asMap(await _relay('key_gc'))['removed']);

  @override
  Future<int> adminRetireConnections({
    required int namespace,
    required String service,
    int? connId,
  }) async => _asInt(
    _asMap(
      // The relay names the namespace `keyId` here: a temporary
      // credential's namespace is its key id.
      await _relay('retire', {
        'keyId': namespace,
        'service': service,
        'connId': connId,
      }),
    )['retired'],
  );

  @override
  Future<void> adminSetLegacyProtocol({required bool allow}) async {
    await _relay('legacy_protocol', {'allow': allow});
  }

  @override
  Future<void> adminResetCredentials() async {
    await _relay('reset_auth_state');
  }

  @override
  Future<KeyRotation> adminRotateKey() async =>
      KeyRotation.fromMap(_asMap(await _relay('rotate_root_key')));
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
