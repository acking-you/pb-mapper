import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:pb_mapper_ui/src/common/polling.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';

/// Where the relay stands, as far as this window knows.
enum RelayState {
  /// No address configured.
  unset,

  /// An address, but no answer yet.
  checking,
  online,
  offline,
}

/// Everything the pages show, read once and kept fresh in one place.
///
/// Each page used to fetch its own copy and subscribe to the change stream
/// separately, so the same list was read three times after one click and two
/// pages could disagree for a moment about whether a tunnel was up. The pages
/// and the galaxy now listen to this, and every write goes through it.
///
/// Freshness comes from the Rust change stream (a tunnel's state moving, a
/// terminal command, a config edit), a slow relay check, since nothing on this
/// side is told when another computer registers a key, and a periodic read of
/// this computer's tunnels, without which the stream would go quiet (see
/// [tunnelInterval]).
class AppStore extends ChangeNotifier {
  AppStore({
    required this.api,
    Stream<StateChange>? changes,
    this.relayInterval = const Duration(seconds: 12),
    this.tunnelInterval = const Duration(seconds: 6),
  }) : _changes = changes ?? PbMapperService.changeStream;

  final PbMapperApiClient api;
  final Stream<StateChange> _changes;

  /// How often the relay is asked what it holds while nothing else asks.
  final Duration relayInterval;

  /// How often this computer's tunnels are read again. The Rust side probes a
  /// tunnel's health only when its cached status is read and has gone stale,
  /// and only a probe that sees a transition sends a change event; with no
  /// reads, a tunnel that fails after it settled would stay drawn as running.
  final Duration tunnelInterval;

  ConfigStatus config = const ConfigStatus(
    serverAddress: '',
    keepAliveEnabled: true,
    msgHeaderKey: '',
  );

  /// What this computer shares and what it connects to, oldest first.
  List<Tunnel> shares = const [];
  List<Tunnel> connections = const [];

  /// The relay's last answer, or null before the first one.
  ServerStatusDetail? relay;

  /// Whether the first read finished, so a page can tell "nothing yet" from
  /// "nothing at all".
  bool loaded = false;

  bool _checking = false;
  int _relayAsk = 0;
  bool _started = false;
  bool _disposed = false;
  ChangeSubscription? _subscription;
  Timer? _relayTimer;
  Timer? _tunnelTimer;
  final Completer<void> _firstRead = Completer<void>();

  String get relayAddress => config.serverAddress.trim();

  bool get hasRelay => relayAddress.isNotEmpty;

  RelayState get relayState {
    if (!hasRelay) return RelayState.unset;
    final relay = this.relay;
    if (relay == null) return RelayState.checking;
    return relay.serverAvailable ? RelayState.online : RelayState.offline;
  }

  /// Every tunnel this computer owns, shares first.
  List<Tunnel> get tunnels => [...shares, ...connections];

  /// Keys the relay lists right now; empty while it is not answering.
  List<String> get relayServices {
    final relay = this.relay;
    if (relay == null || !relay.serverAvailable) return const [];
    return relay.registeredServices;
  }

  /// Keys on the relay that this computer does not share itself: what there
  /// is to connect to.
  List<String> get remoteServices {
    final mine = {for (final s in shares) s.key};
    return [
      for (final key in relayServices)
        if (!mine.contains(key)) key,
    ]..sort();
  }

  /// This computer's connection to [key], if it has one.
  Tunnel? connectionTo(String key) =>
      connections.firstWhereOrNull((c) => c.key == key);

  /// The tunnel of [kind] for [key], if there is one.
  Tunnel? find(TunnelKind kind, String key) =>
      (kind == TunnelKind.share ? shares : connections).firstWhereOrNull(
        (t) => t.key == key,
      );

  /// Completes once [start]'s first read of the config and the tunnels is in,
  /// without waiting for the relay to answer.
  Future<void> get ready => _firstRead.future;

  /// Begin reading and listening. Safe to call more than once.
  Future<void> start() async {
    if (_started) return;
    _started = true;
    _subscription = ChangeSubscription.listen(_changes, {
      StateChangeKind.services,
      StateChangeKind.clients,
      StateChangeKind.server,
      StateChangeKind.config,
    }, _onChange);
    _relayTimer = Timer.periodic(relayInterval, (_) => checkRelay());
    _tunnelTimer = Timer.periodic(tunnelInterval, (_) => _loadTunnels());
    await refresh(force: true);
  }

  /// Re-read whatever a burst of changes touched.
  void _onChange(Set<StateChangeKind> kinds) {
    if (_disposed) return;
    if (kinds.contains(StateChangeKind.unknown)) {
      unawaited(refresh());
      return;
    }
    if (kinds.contains(StateChangeKind.services)) unawaited(_loadShares());
    if (kinds.contains(StateChangeKind.clients)) unawaited(_loadConnections());
    // A config change asks the relay again anyway, at its new address.
    if (kinds.contains(StateChangeKind.config)) {
      unawaited(_loadConfig().then((_) => checkRelay(force: true)));
    } else if (kinds.contains(StateChangeKind.server)) {
      unawaited(checkRelay());
    }
  }

  /// Read everything again; [force] makes the relay answer now rather than
  /// from the Rust side's cache.
  Future<void> refresh({bool force = false}) async {
    await Future.wait([_loadConfig(), _loadTunnels()]);
    loaded = true;
    if (!_firstRead.isCompleted) _firstRead.complete();
    _notify();
    await checkRelay(force: force);
  }

  /// Ask the relay what it holds. A periodic check gives way to one already
  /// running; a [force]d one does not, and the latest ask owns the answer, so
  /// a slow reply about an old address cannot land after a new one.
  Future<void> checkRelay({bool force = false}) async {
    if (_disposed) return;
    if (!hasRelay) {
      if (relay != null) {
        relay = null;
        _notify();
      }
      return;
    }
    if (_checking && !force) return;
    final ask = ++_relayAsk;
    _checking = true;
    ServerStatusDetail next;
    try {
      next = force
          ? await api.forceRefreshServerStatus()
          : await api.getServerStatusDetail();
    } catch (_) {
      next = const ServerStatusDetail(
        serverAvailable: false,
        registeredServices: [],
        serverMap: '',
        activeConnections: '',
        idleConnections: '',
      );
    }
    if (ask != _relayAsk) return;
    _checking = false;
    relay = next;
    _notify();
  }

  Future<void> _loadConfig() async {
    try {
      config = await api.fetchConfig();
    } catch (_) {
      // Keep what was there: a failed read is not a cleared config.
    }
    _notify();
  }

  Future<void> _loadShares() async {
    try {
      final next =
          (await api.getServiceConfigs()).map(Tunnel.fromService).toList()
            ..sort((a, b) => a.createdAtMs.compareTo(b.createdAtMs));
      if (!listEquals(next, shares)) {
        shares = next;
        _notify();
      }
    } catch (_) {}
  }

  Future<void> _loadConnections() async {
    try {
      final next =
          (await api.getClientConfigs()).map(Tunnel.fromClient).toList()
            ..sort((a, b) => a.createdAtMs.compareTo(b.createdAtMs));
      if (!listEquals(next, connections)) {
        connections = next;
        _notify();
      }
    } catch (_) {}
  }

  Future<void> _loadTunnels() async {
    if (_disposed) return;
    await Future.wait([_loadShares(), _loadConnections()]);
  }

  Future<void> _reload(TunnelKind kind) =>
      kind == TunnelKind.share ? _loadShares() : _loadConnections();

  // --- Writes. Each returns the Rust side's verdict for the caller to show.

  /// Start the tunnel [draft] describes. Accepting the request is not the
  /// same as coming up: follow with [settle] to learn how it went.
  Future<OperationResult> create(TunnelDraft draft) async {
    final result = await _apply(draft);
    await _reload(draft.kind);
    return result;
  }

  Future<OperationResult> _apply(TunnelDraft draft) => switch (draft.kind) {
    TunnelKind.share => api.registerService(
      serviceKey: draft.key,
      localAddress: draft.localAddress,
      protocol: draft.protocol,
      enableEncryption: draft.encrypted,
      enableKeepAlive: draft.keepAlive,
    ),
    TunnelKind.connect => api.connectService(
      serviceKey: draft.key,
      localAddress: draft.localAddress,
      protocol: draft.protocol,
      enableKeepAlive: draft.keepAlive,
    ),
  };

  /// Replace [old] with [next], putting [old] back if [next] is refused.
  ///
  /// A share or connection under the same key is replaced in place by the
  /// Rust side. A connection is stopped first, since the new one usually binds
  /// the port the old one holds. A share is not: the Rust side refuses a share
  /// (bad address, relay unreachable) before it touches the one it would
  /// replace, so a refused share edit normally leaves the old share running.
  /// Whatever the refusal left down that was up before is started again.
  Future<OperationResult> update(Tunnel old, TunnelDraft next) async {
    if (old.kind == TunnelKind.connect && old.isLive) {
      await api.disconnectService(old.key);
    }
    final result = await _apply(next);
    if (result.success) {
      if (old.key != next.key) await _remove(old);
    } else if (old.isLive) {
      await _reload(old.kind);
      if (find(old.kind, old.key)?.isLive != true) await _apply(old.draft);
    }
    await _reload(old.kind);
    return result;
  }

  /// Start a stopped tunnel again with what it was saved with.
  Future<OperationResult> resume(Tunnel tunnel) => create(tunnel.draft);

  /// Stop [tunnel]'s background task, keeping it saved.
  Future<OperationResult> stop(Tunnel tunnel) async {
    final result = tunnel.kind == TunnelKind.share
        ? await api.unregisterService(tunnel.key)
        : await api.disconnectService(tunnel.key);
    await _reload(tunnel.kind);
    return result;
  }

  /// Stop [tunnel] and forget it.
  Future<OperationResult> remove(Tunnel tunnel) async {
    final result = await _remove(tunnel);
    await _reload(tunnel.kind);
    return result;
  }

  Future<OperationResult> _remove(Tunnel tunnel) =>
      tunnel.kind == TunnelKind.share
      ? api.deleteServiceConfig(tunnel.key)
      : api.deleteClientConfig(tunnel.key);

  /// Wait for the tunnel to leave its retry loop, and return where it landed
  /// (null if it never appeared). Retrying counts as unsettled; a relay that
  /// is down keeps it there until [timeout].
  Future<Tunnel?> settle(
    TunnelKind kind,
    String key, {
    Duration timeout = const Duration(seconds: 20),
  }) async {
    await waitUntilSettled(
      timeout: timeout,
      changes: _changes,
      attempt: () async {
        if (_disposed) return true;
        await _reload(kind);
        final tunnel = find(kind, key);
        return tunnel != null && tunnel.health != TunnelHealth.retrying;
      },
    );
    return find(kind, key);
  }

  /// Save the relay's address and this computer's credential, then ask the
  /// relay whether it answers.
  Future<OperationResult> saveRelay({
    required String address,
    required String credential,
    required bool keepAlive,
  }) async {
    final result = await api.updateConfig(
      serverAddress: address.trim(),
      keepAlive: keepAlive,
      msgHeaderKey: credential.trim(),
    );
    await _loadConfig();
    relay = null;
    _notify();
    await checkRelay(force: true);
    return result;
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _subscription?.cancel();
    _relayTimer?.cancel();
    _tunnelTimer?.cancel();
    super.dispose();
  }
}
