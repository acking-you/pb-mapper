import 'dart:async';

import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';

/// A [PbMapperApiClient] that answers from memory instead of over FFI.
///
/// Every method is implemented — the interface makes forgetting one a compile
/// error rather than a call that escapes to the FFI at run time. Writes change
/// what later reads return, so a view can be driven through a whole flow:
/// register, see it listed, stop it, see it stopped.
class FakePbMapperApi implements PbMapperApiClient {
  FakePbMapperApi({
    this.services = const [],
    this.clients = const [],
    this.serverServices = const [],
    this.conns = const {},
    this.relayUp = true,
    this.config = const ConfigStatus(
      serverAddress: 'lb7666.top:7666',
      keepAliveEnabled: true,
      msgHeaderKey: '',
    ),
  });

  List<ServiceConfigInfo> services;
  List<ClientConfigInfo> clients;
  ConfigStatus config;

  /// Whether the relay answers.
  bool relayUp;

  /// The status a tunnel gets when it is started.
  String startStatus = 'running';

  /// Refuse the next write with this message.
  String? failNext;

  /// How long reading the config takes, for a test about what is shown
  /// before it arrives.
  Duration configDelay = Duration.zero;

  /// Every call that changes something, in order, so a test can assert what
  /// the view asked for rather than only what it drew.
  final List<String> calls = [];

  static const _ok = OperationResult(success: true, message: 'ok');

  OperationResult? _refused() {
    final message = failNext;
    if (message == null) return null;
    failNext = null;
    return OperationResult(success: false, message: message);
  }

  @override
  Future<OperationResult> setAppDirectoryPath(String path) async => _ok;

  @override
  Future<ConfigStatus> fetchConfig() async {
    if (configDelay > Duration.zero) await Future<void>.delayed(configDelay);
    return config;
  }

  @override
  Future<String> revealIsolatedRelayAdminKey() async {
    calls.add('revealIsolatedRelayAdminKey');
    return config.isolatedRelayAdminKey;
  }

  @override
  Future<OperationResult> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
  }) async {
    calls.add('updateConfig($serverAddress)');
    if (_refused() case final refused?) return refused;
    config = ConfigStatus(
      serverAddress: serverAddress,
      keepAliveEnabled: keepAlive,
      msgHeaderKey: msgHeaderKey,
    );
    return _ok;
  }

  @override
  Future<OperationResult> startServer({
    required int port,
    required bool keepAlive,
  }) async {
    calls.add('startServer($port)');
    if (_refused() case final refused?) return refused;
    localRelayPort = port;
    return _ok;
  }

  @override
  Future<OperationResult> stopServer() async {
    calls.add('stopServer');
    localRelayPort = null;
    return _ok;
  }

  /// The port of the relay this app can run itself, while it runs.
  int? localRelayPort;
  bool get localRelayRunning => localRelayPort != null;

  @override
  Future<LocalServerStatus> getLocalServerStatus() async => LocalServerStatus(
    isRunning: localRelayRunning,
    port: localRelayPort,
    activeConnections: localRelayRunning ? 2 : 0,
    registeredServices: localRelayRunning ? 1 : 0,
    uptimeSeconds: localRelayRunning ? 3720 : 0,
  );

  /// What the relay reports as registered — not the same list as [services],
  /// which is what this machine has configured locally.
  List<String> serverServices;

  @override
  Future<ServerStatusDetail> getServerStatusDetail() async =>
      ServerStatusDetail(
        serverAvailable: relayUp,
        registeredServices: relayUp ? serverServices : const [],
        serverMap: '',
        activeConnections: relayUp ? '[41, 42]' : '',
        idleConnections: '',
        error: relayUp ? null : 'connection refused',
        latencyMs: relayUp ? 18 : null,
      );

  @override
  Future<ServerStatusDetail> forceRefreshServerStatus() async =>
      getServerStatusDetail();

  /// What the relay is holding, keyed by service. Absent means none.
  Map<String, List<ServiceConnInfo>> conns;

  @override
  Future<List<ServiceConnInfo>> getServiceConns(String serviceKey) async =>
      conns[serviceKey] ?? const [];

  @override
  Future<List<ServiceConfigInfo>> getServiceConfigs() async => services;

  @override
  Future<ServiceStatusSignal> getServiceStatus(String serviceKey) async =>
      ServiceStatusSignal(
        serviceKey: serviceKey,
        status: 'running',
        message: '',
      );

  @override
  Future<OperationResult> registerService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableEncryption,
    required bool enableKeepAlive,
  }) async {
    calls.add('registerService($serviceKey)');
    if (_refused() case final refused?) return refused;
    services = [
      for (final s in services)
        if (s.serviceKey != serviceKey) s,
      fakeService(
        serviceKey: serviceKey,
        localAddress: localAddress,
        protocol: protocol,
        encrypted: enableEncryption,
        status: startStatus,
      ),
    ];
    return _ok;
  }

  @override
  Future<OperationResult> unregisterService(String serviceKey) async {
    calls.add('unregisterService($serviceKey)');
    if (_refused() case final refused?) return refused;
    services = [
      for (final s in services)
        s.serviceKey == serviceKey
            ? fakeService(
                serviceKey: s.serviceKey,
                localAddress: s.localAddress,
                status: 'stopped',
              )
            : s,
    ];
    return _ok;
  }

  @override
  Future<OperationResult> deleteServiceConfig(String serviceKey) async {
    calls.add('deleteServiceConfig($serviceKey)');
    services = services.where((s) => s.serviceKey != serviceKey).toList();
    return _ok;
  }

  @override
  Future<List<ClientConfigInfo>> getClientConfigs() async => clients;

  @override
  Future<ClientStatusSignal> getClientStatus(String serviceKey) async =>
      ClientStatusSignal(
        serviceKey: serviceKey,
        status: 'running',
        message: '',
      );

  @override
  Future<OperationResult> connectService({
    required String serviceKey,
    required String localAddress,
    required String protocol,
    required bool enableKeepAlive,
    int? namespace,
  }) async {
    calls.add(
      namespace == null
          ? 'connectService($serviceKey)'
          : 'connectService($serviceKey@$namespace)',
    );
    if (_refused() case final refused?) return refused;
    clients = [
      for (final c in clients)
        if (c.serviceKey != serviceKey) c,
      fakeClient(
        serviceKey: serviceKey,
        localAddress: localAddress,
        status: startStatus,
        namespace: namespace,
      ),
    ];
    return _ok;
  }

  @override
  Future<OperationResult> disconnectService(String serviceKey) async {
    calls.add('disconnectService($serviceKey)');
    clients = [
      for (final c in clients)
        c.serviceKey == serviceKey
            ? fakeClient(
                serviceKey: c.serviceKey,
                localAddress: c.localAddress,
                status: 'stopped',
                namespace: c.namespace,
              )
            : c,
    ];
    return _ok;
  }

  @override
  Future<OperationResult> deleteClientConfig(String serviceKey) async {
    calls.add('deleteClientConfig($serviceKey)');
    clients = clients.where((c) => c.serviceKey != serviceKey).toList();
    return _ok;
  }

  // --- The relay as a whole.

  /// Who this computer is to the relay. An administrator unless a test says
  /// otherwise, so every page has something to show.
  RelayIdentity identity = const RelayIdentity(role: RelayRole.admin);

  /// Temporary credentials, and the secret of each.
  List<RelayKey> keys = [];
  final Map<int, String> secrets = {};
  int _nextKeyId = 1;

  /// What the relay holds for each service, and who registered it.
  List<RelayConnection> relayConnections = [];
  Map<String, int> serviceOwners = {};
  List<DirectSession> sessions = [];
  bool legacyAllowed = false;

  /// Unix seconds, fixed so a test can reason about expiry.
  int now = 1_790_000_000;

  void _relayReachable() {
    if (!relayUp) {
      throw const RelayCallException(
        'Failed to connect to server: connection refused',
        code: 'SERVER_UNREACHABLE',
      );
    }
  }

  void _admin() {
    _relayReachable();
    if (!identity.isAdmin) {
      throw const RelayCallException(
        "This computer's credential is not the relay's administrator key",
        code: 'INVALID_ARGUMENT',
      );
    }
  }

  RelayKey _key(int keyId) => keys.firstWhere(
    (k) => k.keyId == keyId,
    orElse: () => throw RelayCallException(
      'credential $keyId not found',
      code: 'PROTOCOL',
    ),
  );

  IssuedCredential _issued(RelayKey key) => IssuedCredential(
    keyId: key.keyId,
    state: key.state,
    issuedAt: key.issuedAt,
    expiresAt: key.expiresAt,
    label: key.label,
    credential: secrets[key.keyId] ?? '',
  );

  void _replace(RelayKey key) =>
      keys = [for (final k in keys) k.keyId == key.keyId ? key : k];

  @override
  Future<RelayIdentity> relayIdentity() async => identity;

  @override
  Future<List<DirectSession>> relaySessions() async {
    _relayReachable();
    return sessions;
  }

  @override
  Future<RelayAuthStatus> adminAuthStatus() async {
    _admin();
    int count(RelayKeyState state) =>
        keys.where((k) => k.state == state).length;
    return RelayAuthStatus(
      safeMode: false,
      capacity: 1024,
      activeKeys: count(RelayKeyState.active),
      expiredKeys: count(RelayKeyState.expired),
      revokedKeys: count(RelayKeyState.revoked),
      legacyAllowed: legacyAllowed,
      activeLegacyConnections: 0,
      lastLegacyConnectionAt: null,
      authSuccesses: 128,
      authFailures: 3,
      serverInstanceId: '7f3a9c01',
    );
  }

  @override
  Future<List<RelayService>> adminServices() async {
    _admin();
    RelayService service(int namespace, String name) => RelayService(
      keyId: namespace,
      namespace: namespace,
      name: name,
      transport: 'TCP',
      encrypted: true,
      connections: relayConnections
          .where((c) => c.namespace == namespace && c.service == name)
          .length,
    );
    return [
      for (final name in serverServices)
        service(serviceOwners[name] ?? 0, name),
      for (final (namespace, name) in namespacedServices)
        service(namespace, name),
    ];
  }

  /// Services the administrator inventory lists in a credential's namespace
  /// besides [serverServices]: a name here may repeat one there.
  List<(int, String)> namespacedServices = [];

  @override
  Future<List<RelayConnection>> adminConnections() async {
    _admin();
    if (connectionsFail) {
      throw const RelayCallException(
        'relay.example:7666 did not finish `connections` within 30 seconds',
        code: 'TIMEOUT',
      );
    }
    return relayConnections;
  }

  /// The connection listing times out, while the other listings answer.
  bool connectionsFail = false;

  @override
  Future<List<RelayKey>> adminKeys() async {
    _admin();
    final answered = keys;
    await adminGate?.future;
    return answered;
  }

  /// Holds the credential list's answer back until completed, to stand for
  /// a relay that is slow to reply.
  Completer<void>? adminGate;

  @override
  Future<IssuedCredential> adminIssueKey({
    required Duration ttl,
    String? label,
  }) async {
    _admin();
    calls.add('adminIssueKey(${ttl.inSeconds}, $label)');
    if (ttl > maxTtl) {
      throw const RelayCallException(
        'The relay refused: temporary-key TTL exceeds the maximum',
        code: 'PROTOCOL',
      );
    }
    final id = _nextKeyId++;
    final key = RelayKey(
      keyId: id,
      state: RelayKeyState.active,
      issuedAt: now,
      expiresAt: now + ttl.inSeconds,
      label: label,
    );
    keys = [...keys, key];
    secrets[id] = 'pbmt1_${id}_${'s' * 40}';
    return _issued(key);
  }

  @override
  Future<IssuedCredential> adminRevealKey(int keyId) async {
    _admin();
    return _issued(_key(keyId));
  }

  @override
  Future<IssuedCredential> adminRenewKey(int keyId, Duration ttl) async {
    _admin();
    calls.add('adminRenewKey($keyId, ${ttl.inSeconds})');
    final old = _key(keyId);
    final renewed = RelayKey(
      keyId: keyId,
      state: RelayKeyState.active,
      issuedAt: old.issuedAt,
      expiresAt: now + ttl.inSeconds,
      label: old.label,
    );
    _replace(renewed);
    return _issued(renewed);
  }

  @override
  Future<RelayKey> adminRevokeKey(int keyId) async {
    _admin();
    calls.add('adminRevokeKey($keyId)');
    final old = _key(keyId);
    final revoked = RelayKey(
      keyId: keyId,
      state: RelayKeyState.revoked,
      issuedAt: old.issuedAt,
      expiresAt: old.expiresAt,
      label: old.label,
    );
    _replace(revoked);
    return revoked;
  }

  @override
  Future<int> adminCleanUpKeys() async {
    _admin();
    calls.add('adminCleanUpKeys');
    final before = keys.length;
    keys = keys.where((k) => k.state == RelayKeyState.active).toList();
    return before - keys.length;
  }

  @override
  Future<int> adminRetireConnections({
    required int namespace,
    required String service,
    int? connId,
  }) async {
    _admin();
    calls.add('adminRetireConnections($namespace, $service, $connId)');
    bool hit(RelayConnection c) =>
        c.namespace == namespace &&
        c.service == service &&
        (connId == null || c.connId == connId);
    final retired = relayConnections.where(hit).length;
    relayConnections = relayConnections.where((c) => !hit(c)).toList();
    return retired;
  }

  @override
  Future<void> adminSetLegacyProtocol({required bool allow}) async {
    _admin();
    calls.add('adminSetLegacyProtocol($allow)');
    legacyAllowed = allow;
  }

  @override
  Future<void> adminResetCredentials() async {
    _admin();
    calls.add('adminResetCredentials');
    keys = [];
    secrets.clear();
  }

  @override
  Future<KeyRotation> adminRotateKey() async {
    _admin();
    calls.add('adminRotateKey');
    const rotated = 'R0tatedAdminKey-0123456789abcdef';
    if (!rotationAnswers) {
      return const KeyRotation(
        key: rotated,
        confirmed: false,
        saved: false,
        reason: 'connection reset',
      );
    }
    config = ConfigStatus(
      serverAddress: config.serverAddress,
      keepAliveEnabled: config.keepAliveEnabled,
      msgHeaderKey: rotated,
    );
    return KeyRotation(
      key: rotated,
      confirmed: true,
      saved: true,
      restartShares: [
        for (final s in services)
          if (s.status != 'stopped') s.serviceKey,
      ],
      restartConnections: [
        for (final c in clients)
          if (c.status != 'stopped') c.serviceKey,
      ],
    );
  }

  /// Whether the relay's answer to a rotation arrives. When it does not, the
  /// relay may or may not have taken the new key.
  bool rotationAnswers = true;

  /// The longest lifetime the relay accepts for a credential.
  Duration maxTtl = const Duration(days: 30);
}

/// A connection the relay holds, as the administrator inventory lists it.
RelayConnection fakeRelayConnection({
  required String service,
  required int connId,
  int keyId = 0,
  int? namespace,
  bool healthy = true,
  int lastRxAgeMs = 900,
}) => RelayConnection(
  keyId: keyId,
  namespace: namespace ?? keyId,
  service: service,
  connId: connId,
  generation: 1,
  protocolVersion: 2,
  healthy: healthy,
  transport: 'TCP',
  encrypted: true,
  lastRxAgeMs: lastRxAgeMs,
);

/// A control connection the relay is holding.
ServiceConnInfo fakeConn({
  required int connId,
  bool healthy = true,
  int generation = 1,
  int protocolVersion = 2,
  Duration lastRxAge = const Duration(milliseconds: 800),
}) => ServiceConnInfo(
  connId: connId,
  generation: generation,
  protocolVersion: protocolVersion,
  healthy: healthy,
  lastRxAge: lastRxAge,
);

/// A connection, with only the fields a test usually cares about.
ClientConfigInfo fakeClient({
  required String serviceKey,
  String localAddress = '127.0.0.1:9090',
  String status = 'running',
  String message = '',
  int? namespace,
  int? routedNamespace,
}) => ClientConfigInfo(
  serviceKey: serviceKey,
  localAddress: localAddress,
  protocol: 'TCP',
  enableKeepAlive: true,
  namespace: namespace,
  routedNamespace: routedNamespace,
  status: status,
  statusMessage: message,
  createdAtMs: 1,
  updatedAtMs: 1,
);

/// A registered service, with only the fields a test usually cares about.
ServiceConfigInfo fakeService({
  required String serviceKey,
  String localAddress = '127.0.0.1:8080',
  String protocol = 'TCP',
  bool encrypted = true,
  String status = 'running',
  String message = '',
}) => ServiceConfigInfo(
  serviceKey: serviceKey,
  localAddress: localAddress,
  protocol: protocol,
  enableEncryption: encrypted,
  enableKeepAlive: true,
  status: status,
  statusMessage: message,
  createdAtMs: 1,
  updatedAtMs: 1,
);
