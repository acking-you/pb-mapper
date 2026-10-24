import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';

/// One service on the relay, as the Services section lists it.
class RelayServiceRow {
  const RelayServiceRow({
    required this.namespace,
    required this.name,
    this.info,
  });

  /// With [name], what the relay routes by: two credentials can each
  /// register the same name. See [RelayService.namespace].
  final int namespace;
  final String name;

  /// The administrator inventory's entry, when this computer has one.
  final RelayService? info;

  /// Unique on the relay, for keys: the bare name in the administrator's
  /// namespace, `namespace/name` in a credential's.
  String get id => relayServiceId(namespace, name);
}

String relayServiceId(int namespace, String name) =>
    namespace == 0 ? name : '$namespace/$name';

/// The relay as a whole, for the Relay page: who this computer is to it,
/// the direct sessions it observes, the relay this computer can run itself,
/// and — with the administrator key — everything the relay holds: its
/// services and connections, its temporary credentials, its security state.
///
/// Apart from [AppStore], which every page reads and which stays cheap: this
/// asks the relay several questions at once, so it does so only while the
/// Relay page is on screen ([watch]), and after anything here changes.
class RelayStore extends ChangeNotifier {
  RelayStore({required this.app, this.interval = const Duration(seconds: 10)})
    : _seenConfig = (app.relayAddress, app.config.msgHeaderKey) {
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

  /// Whether the administrator's service inventory has been read, which
  /// the Services section then lists in place of this credential's own.
  bool adminLoaded = false;

  /// Why the administrator inventories could not be read, last time.
  String? adminError;

  /// Why the direct sessions could not be read, last time.
  String? sessionsError;

  bool loading = false;
  DateTime? refreshedAt;

  /// A rotated administrator key this computer did not save, and the relay
  /// it is for: the relay's answer was lost, or Settings moved this computer
  /// elsewhere while it ran. It may be the only key that relay takes, so it
  /// is held here rather than by the page, which may be gone by the time it
  /// arrives, until the person says they have it.
  ({KeyRotation rotation, String address})? unsavedRotation;

  bool get isAdmin => identity.isAdmin;

  int _watchers = 0;
  Timer? _timer;
  bool _disposed = false;
  (String, String) _seenConfig;

  /// Bumped when the relay or the credential changes. A read that began
  /// before then answers for a relay no longer shown, and is dropped.
  int _generation = 0;

  /// A refresh was asked for while one ran; run another after it.
  bool _again = false;

  /// Administrator reads, numbered as they start, and for each listing the
  /// latest whose answer was applied. Within one relay a change reads its
  /// listings back while a refresh's may still be out; whichever answers
  /// last, the one that started earlier must not overwrite the other.
  int _adminReads = 0;
  final Map<String, int> _adminApplied = {};

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

  /// A new address or credential is a new relay, or a new identity on it:
  /// nothing read before applies, and nothing still in flight may land.
  void _onApp() {
    final config = (app.relayAddress, app.config.msgHeaderKey);
    if (config == _seenConfig) return;
    _seenConfig = config;
    _generation++;
    identity = RelayIdentity.unknown;
    identityLoaded = false;
    sessions = const [];
    sessionsError = null;
    _forgetAdmin();
    _notify();
    if (_watchers > 0) unawaited(refresh());
  }

  void _forgetAdmin() {
    authStatus = null;
    services = const [];
    connections = const [];
    keys = const [];
    adminLoaded = false;
    adminError = null;
  }

  /// Read everything this credential may see. Each part stands alone: one
  /// that fails says why and leaves the others to load.
  Future<void> refresh() async {
    if (_disposed) return;
    if (loading) {
      _again = true;
      return;
    }
    loading = true;
    _notify();
    final generation = _generation;
    try {
      await Future.wait([
        _loadLocal(),
        _loadIdentity(generation),
        if (app.hasRelay) _loadSessions(generation),
      ]);
    } finally {
      loading = false;
      refreshedAt = DateTime.now();
      _notify();
    }
    if (_again && !_disposed) {
      _again = false;
      await refresh();
    }
  }

  Future<void> _loadIdentity(int generation) async {
    RelayIdentity read;
    try {
      read = await api.relayIdentity();
    } catch (error) {
      read = RelayIdentity(role: RelayRole.none, error: _reason(error));
    }
    if (generation != _generation) return;
    identity = read;
    identityLoaded = true;
    if (read.isAdmin && app.hasRelay) {
      await _loadAdmin(generation);
    } else {
      _forgetAdmin();
    }
  }

  Future<void> _loadSessions(int generation) async {
    try {
      final read = await api.relaySessions();
      if (generation != _generation) return;
      sessions = read;
      sessionsError = null;
    } catch (error) {
      if (generation == _generation) sessionsError = _reason(error);
    }
  }

  Future<void> _loadLocal() async {
    try {
      local = await api.getLocalServerStatus();
    } catch (_) {}
  }

  /// Each listing on its own: one that fails (a connection list that times
  /// out) says why without blanking the ones that answered.
  Future<void> _loadAdmin([int? generation]) async {
    final asOf = generation ?? _generation;
    final started = ++_adminReads;
    String? failure;
    Future<void> load<T>(
      String listing,
      Future<T> Function() read,
      void Function(T value) apply,
    ) async {
      try {
        final value = await read();
        if (asOf != _generation) return;
        if (started < (_adminApplied[listing] ?? 0)) return;
        _adminApplied[listing] = started;
        apply(value);
      } catch (error) {
        failure ??= _reason(error);
      }
    }

    await Future.wait([
      load('auth', api.adminAuthStatus, (value) => authStatus = value),
      load('services', api.adminServices, (value) {
        services = value;
        adminLoaded = true;
      }),
      load('connections', api.adminConnections, (value) {
        connections = value;
      }),
      load('keys', api.adminKeys, (value) => keys = value),
    ]);
    // Only the latest read speaks for the relay now.
    if (asOf == _generation && started == _adminReads) adminError = failure;
  }

  static String _reason(Object error) =>
      error is RelayCallException ? error.message : '$error';

  /// The namespace this computer's own tunnels live in: a temporary
  /// credential's own, or the administrator's, 0.
  int get ownNamespace =>
      identity.role == RelayRole.temporary ? identity.keyId ?? 0 : 0;

  /// Every service on the relay, by name: the administrator inventory when
  /// there is one, which sees every namespace, or else what this
  /// credential's status query lists, all in its own namespace.
  List<RelayServiceRow> get serviceRows {
    final rows = adminLoaded
        ? [
            for (final s in services)
              RelayServiceRow(namespace: s.namespace, name: s.name, info: s),
          ]
        : [
            for (final name in app.relayServices)
              RelayServiceRow(namespace: ownNamespace, name: name),
          ];
    return rows..sort((a, b) {
      final byName = a.name.compareTo(b.name);
      return byName != 0 ? byName : a.namespace.compareTo(b.namespace);
    });
  }

  /// The connections the relay holds for [row].
  List<RelayConnection> connectionsOf(RelayServiceRow row) => [
    for (final c in connections)
      if (c.namespace == row.namespace && c.service == row.name) c,
  ];

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
    required int namespace,
    required String service,
    int? connId,
  }) => _then(
    api.adminRetireConnections(
      namespace: namespace,
      service: service,
      connId: connId,
    ),
    _loadAdmin,
  );

  Future<void> setLegacyProtocol({required bool allow}) =>
      _then(api.adminSetLegacyProtocol(allow: allow), _loadAdmin);

  Future<void> resetCredentials() =>
      _then(api.adminResetCredentials(), _loadAdmin);

  /// Rotate the administrator key. Once the relay is known to have taken
  /// the new key this computer has switched to it, and everything is read
  /// again with it; a key it could not switch to is kept in
  /// [unsavedRotation].
  Future<KeyRotation> rotateKey() async {
    final address = app.relayAddress;
    final rotation = await api.adminRotateKey();
    if (!rotation.saved) {
      unsavedRotation = (rotation: rotation, address: address);
      _notify();
    }
    if (rotation.confirmed) {
      await app.refresh(force: true);
      await refresh();
    }
    return rotation;
  }

  /// The person has the key in [unsavedRotation] somewhere safe.
  void keptUnsavedRotation() {
    unsavedRotation = null;
    _notify();
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
