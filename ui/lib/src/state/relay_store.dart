import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';

/// The relay as a whole, for the Relay page: who this computer is to it,
/// the direct sessions it observes, the relay this computer can run itself,
/// and — with the administrator key — everything the relay holds: its
/// services and connections, its temporary credentials, its security state.
///
/// Apart from [AppStore], which every page reads and which stays cheap: this
/// asks the relay several questions at once, so it does so only while the
/// Relay page is on screen ([watch]), and after anything here changes.
class RelayStore extends ChangeNotifier {
  RelayStore({required this.app, this.interval = const Duration(seconds: 10)}) {
    app.addListener(_onApp);
  }

  final AppStore app;
  final Duration interval;

  PbMapperApiClient get api => app.api;

  RelayIdentity identity = RelayIdentity.unknown;

  /// Whether [identity] has been read at all.
  bool identityLoaded = false;

  RelayAuthStatus? authStatus;
  List<RelayService> services = const [];
  List<RelayConnection> connections = const [];
  List<RelayKey> keys = const [];
  List<DirectSession> sessions = const [];
  LocalServerStatus local = const LocalServerStatus(
    isRunning: false,
    activeConnections: 0,
    registeredServices: 0,
    uptimeSeconds: 0,
  );

  /// Whether the administrator inventories have been read at all.
  bool adminLoaded = false;

  /// Why the administrator inventories could not be read, last time.
  String? adminError;

  /// Why the direct sessions could not be read, last time.
  String? sessionsError;

  bool loading = false;
  DateTime? refreshedAt;

  bool get isAdmin => identity.isAdmin;

  int _watchers = 0;
  Timer? _timer;
  bool _disposed = false;
  (String, String)? _seenConfig;

  /// Keep refreshing while something shows this; pair with [unwatch].
  void watch() {
    if (_watchers++ > 0) return;
    unawaited(refresh());
    _timer = Timer.periodic(interval, (_) => refresh());
  }

  void unwatch() {
    if (_watchers == 0 || --_watchers > 0) return;
    _timer?.cancel();
    _timer = null;
  }

  /// A new address or credential is a new identity, and new data.
  void _onApp() {
    final config = (app.relayAddress, app.config.msgHeaderKey);
    if (config == _seenConfig) return;
    final first = _seenConfig == null;
    _seenConfig = config;
    if (first) return;
    identityLoaded = false;
    adminLoaded = false;
    if (_watchers > 0) unawaited(refresh());
  }

  /// Read everything this credential may see.
  Future<void> refresh() async {
    if (loading || _disposed) return;
    loading = true;
    _notify();
    try {
      identity = await api.relayIdentity();
      identityLoaded = true;
      _seenConfig ??= (app.relayAddress, app.config.msgHeaderKey);
      await Future.wait([
        _loadLocal(),
        if (app.hasRelay) _loadSessions(),
        if (identity.isAdmin && app.hasRelay) _loadAdmin(),
      ]);
    } on RelayCallException catch (error) {
      adminError = error.message;
    } finally {
      loading = false;
      refreshedAt = DateTime.now();
      _notify();
    }
  }

  Future<void> _loadSessions() async {
    try {
      sessions = await api.relaySessions();
      sessionsError = null;
    } on RelayCallException catch (error) {
      sessionsError = error.message;
    }
  }

  Future<void> _loadLocal() async {
    try {
      local = await api.getLocalServerStatus();
    } catch (_) {}
  }

  Future<void> _loadAdmin() async {
    try {
      final results = await Future.wait([
        api.adminAuthStatus(),
        api.adminServices(),
        api.adminConnections(),
        api.adminKeys(),
      ]);
      authStatus = results[0] as RelayAuthStatus;
      services = results[1] as List<RelayService>;
      connections = results[2] as List<RelayConnection>;
      keys = results[3] as List<RelayKey>;
      adminLoaded = true;
      adminError = null;
    } on RelayCallException catch (error) {
      adminError = error.message;
    }
  }

  /// The administrator inventory's entry for [service], if it has one.
  RelayService? serviceNamed(String service) {
    for (final s in services) {
      if (s.name == service) return s;
    }
    return null;
  }

  /// The connections the relay holds for [service].
  List<RelayConnection> connectionsOf(String service) => [
    for (final c in connections)
      if (c.service == service) c,
  ];

  /// Every service on the relay: the administrator inventory when there is
  /// one, which sees every namespace, or else what this credential's status
  /// query lists.
  List<String> get serviceNames {
    if (adminLoaded) {
      return [for (final s in services) s.name]..sort();
    }
    return [...app.relayServices]..sort();
  }

  /// The label of credential [keyId], or null for the administrator's own (0)
  /// or one not listed.
  String? labelOf(int keyId) {
    for (final key in keys) {
      if (key.keyId == keyId) return key.label;
    }
    return null;
  }

  // --- Changes. Each throws a [RelayCallException] for the caller to show,
  // and reads back what it changed.

  Future<IssuedCredential> issueKey({required Duration ttl, String? label}) =>
      _then(api.adminIssueKey(ttl: ttl, label: label), _loadAdmin);

  Future<IssuedCredential> revealKey(int keyId) => api.adminRevealKey(keyId);

  Future<IssuedCredential> renewKey(int keyId, Duration ttl) =>
      _then(api.adminRenewKey(keyId, ttl), _loadAdmin);

  Future<RelayKey> revokeKey(int keyId) =>
      _then(api.adminRevokeKey(keyId), _loadAdmin);

  Future<int> cleanUpKeys() => _then(api.adminCleanUpKeys(), _loadAdmin);

  Future<int> retireConnections({
    required String service,
    int? keyId,
    int? connId,
  }) => _then(
    api.adminRetireConnections(service: service, keyId: keyId, connId: connId),
    _loadAdmin,
  );

  Future<void> setLegacyProtocol({required bool allow}) =>
      _then(api.adminSetLegacyProtocol(allow: allow), _loadAdmin);

  Future<void> resetCredentials() =>
      _then(api.adminResetCredentials(), _loadAdmin);

  /// Rotate the administrator key; this computer switches to the new one,
  /// which is returned to be shown once.
  Future<String> rotateKey() async {
    final key = await api.adminRotateKey();
    await app.refresh(force: true);
    await refresh();
    return key;
  }

  Future<OperationResult> startLocalRelay({
    required int port,
    required bool keepAlive,
  }) async {
    final result = await api.startServer(port: port, keepAlive: keepAlive);
    await _loadLocal();
    _notify();
    return result;
  }

  Future<OperationResult> stopLocalRelay() async {
    final result = await api.stopServer();
    await _loadLocal();
    _notify();
    return result;
  }

  Future<T> _then<T>(Future<T> change, Future<void> Function() reload) async {
    final result = await change;
    await reload();
    _notify();
    return result;
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    app.removeListener(_onApp);
    super.dispose();
  }
}
