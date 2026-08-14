import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';

/// A [PbMapperApiClient] that answers from memory instead of over FFI.
///
/// The views could not be tested at all before this: they built the real
/// singleton in a field initialiser, so mounting one in a widget test meant
/// loading the native library. Every method is implemented — the interface
/// makes forgetting one a compile error rather than a call that escapes to
/// the FFI at run time.
class FakePbMapperApi implements PbMapperApiClient {
  FakePbMapperApi({
    this.services = const [],
    this.clients = const [],
    this.config = const ConfigStatus(
      serverAddress: 'lb7666.top:7666',
      keepAliveEnabled: true,
      msgHeaderKey: '',
    ),
  });

  List<ServiceConfigInfo> services;
  List<ClientConfigInfo> clients;
  ConfigStatus config;

  /// Every call that changes something, in order, so a test can assert what
  /// the view asked for rather than only what it drew.
  final List<String> calls = [];

  static const _ok = OperationResult(success: true, message: 'ok');

  @override
  Future<OperationResult> setAppDirectoryPath(String path) async => _ok;

  @override
  Future<ConfigStatus> fetchConfig() async => config;

  @override
  Future<OperationResult> updateConfig({
    required String serverAddress,
    required bool keepAlive,
    required String msgHeaderKey,
  }) async {
    calls.add('updateConfig($serverAddress)');
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

  @override
  Future<ServerStatusDetail> getServerStatusDetail() async =>
      const ServerStatusDetail(
        serverAvailable: true,
        registeredServices: [],
        serverMap: '',
        activeConnections: '',
        idleConnections: '',
      );

  @override
  Future<ServerStatusDetail> forceRefreshServerStatus() async =>
      getServerStatusDetail();

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
    return _ok;
  }

  @override
  Future<OperationResult> unregisterService(String serviceKey) async {
    calls.add('unregisterService($serviceKey)');
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
    return _ok;
  }

  @override
  Future<OperationResult> disconnectService(String serviceKey) async {
    calls.add('disconnectService($serviceKey)');
    return _ok;
  }

  @override
  Future<OperationResult> deleteClientConfig(String serviceKey) async {
    calls.add('deleteClientConfig($serviceKey)');
    clients = clients.where((c) => c.serviceKey != serviceKey).toList();
    return _ok;
  }
}

/// A connection, with only the fields a test usually cares about.
ClientConfigInfo fakeClient({
  required String serviceKey,
  String localAddress = '127.0.0.1:9090',
  String status = 'running',
}) => ClientConfigInfo(
  serviceKey: serviceKey,
  localAddress: localAddress,
  protocol: 'TCP',
  enableKeepAlive: true,
  status: status,
  statusMessage: '',
  createdAtMs: 1,
  updatedAtMs: 1,
);

/// A registered service, with only the fields a test usually cares about.
ServiceConfigInfo fakeService({
  required String serviceKey,
  String localAddress = '127.0.0.1:8080',
  String status = 'running',
}) => ServiceConfigInfo(
  serviceKey: serviceKey,
  localAddress: localAddress,
  protocol: 'TCP',
  enableEncryption: true,
  enableKeepAlive: true,
  status: status,
  statusMessage: '',
  createdAtMs: 1,
  updatedAtMs: 1,
);
