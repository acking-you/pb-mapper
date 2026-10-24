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
    return _ok;
  }

  @override
  Future<OperationResult> stopServer() async {
    calls.add('stopServer');
    return _ok;
  }

  @override
  Future<LocalServerStatus> getLocalServerStatus() async =>
      const LocalServerStatus(
        isRunning: false,
        activeConnections: 0,
        registeredServices: 0,
        uptimeSeconds: 0,
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
        activeConnections: '',
        idleConnections: '',
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
  }) async {
    calls.add('connectService($serviceKey)');
    if (_refused() case final refused?) return refused;
    clients = [
      for (final c in clients)
        if (c.serviceKey != serviceKey) c,
      fakeClient(
        serviceKey: serviceKey,
        localAddress: localAddress,
        status: startStatus,
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
}

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
}) => ClientConfigInfo(
  serviceKey: serviceKey,
  localAddress: localAddress,
  protocol: 'TCP',
  enableKeepAlive: true,
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
