/// A running tunnel's events and diagnostics, as the SDK reports them through
/// `pb_mapper_tunnel_events_call_json`.
///
/// Each model reads what the app shows and keeps the map it was read from,
/// so a report can be copied whole, with whatever a newer library adds.
library;

import 'package:pb_mapper_ui/src/common/polling.dart' show FirstWhereOrNull;

/// What a tunnel's event feed delivers: the tunnel's diagnostics as they are
/// now, and the event that brought them, if any. The first update carries
/// diagnostics alone.
class TunnelEventsUpdate {
  const TunnelEventsUpdate({this.diagnostics, this.event, this.missed = 0});

  /// Null once the tunnel itself is gone.
  final TunnelDiagnostics? diagnostics;
  final TunnelEvent? event;

  /// Events this reader fell behind by; [diagnostics] has caught up.
  final int missed;

  factory TunnelEventsUpdate.fromMap(Map<String, dynamic> map) =>
      TunnelEventsUpdate(
        diagnostics: switch (map['diagnostics']) {
          final Map<dynamic, dynamic> d => TunnelDiagnostics.fromMap(_map(d)),
          _ => null,
        },
        event: switch (map['event']) {
          final Map<dynamic, dynamic> e => TunnelEvent.fromMap(_map(e)),
          _ => null,
        },
        missed: map['type'] == 'lagged' ? _int(map['skipped']) : 0,
      );
}

/// A point-in-time view of a running tunnel.
class TunnelDiagnostics {
  TunnelDiagnostics.fromMap(this.raw)
    : relayTransport = raw['relay_transport'] as String?,
      phase = raw['last_attempt_phase'] as String?,
      attempts = _int(raw['attempts']),
      consecutiveFailures = _int(raw['consecutive_failures']),
      lastFailure = raw['last_failure'] as String?,
      lastSetup = _duration(raw['last_setup_latency_ms']),
      nextRetry = _duration(raw['next_retry_in_ms']),
      direct = switch (raw['direct']) {
        final Map<dynamic, dynamic> d => SessionCounts.fromMap(_map(d)),
        _ => null,
      },
      portMapping = switch (raw['port_mapping']) {
        final Map<dynamic, dynamic> m => PortMappingDiagnostics.fromMap(_map(m)),
        _ => null,
      },
      events = [
        for (final event in raw['events'] as List? ?? const [])
          if (event is Map) TunnelEvent.fromMap(_map(event)),
      ];

  final Map<String, dynamic> raw;

  /// `tcp` or `quic`.
  final String? relayTransport;

  /// The phase its relay attempt is in, as the SDK names it.
  final String? phase;
  final int attempts;
  final int consecutiveFailures;
  final String? lastFailure;
  final Duration? lastSetup;
  final Duration? nextRetry;
  final SessionCounts? direct;

  /// Present when the tunnel maps its direct endpoint's port.
  final PortMappingDiagnostics? portMapping;

  /// The latest outcomes, oldest first.
  final List<TunnelEvent> events;
}

/// Sessions by path. A share counts only those that reached it directly.
class SessionCounts {
  SessionCounts.fromMap(Map<String, dynamic> map)
    : direct = _int(map['direct_sessions']),
      relay = _int(map['relay_sessions']),
      fallbacks = _int(map['fallbacks']);

  final int direct;
  final int relay;
  final int fallbacks;
}

/// One event of a running tunnel.
class TunnelEvent {
  TunnelEvent.fromMap(this.raw)
    : seq = _int(raw['seq']),
      age = _duration(raw['age_ms']) ?? Duration.zero,
      repeats = _int(raw['repeats'], fallback: 1),
      detail = switch (raw['kind']) {
        final Map<dynamic, dynamic> kind => _map(kind),
        _ => const {},
      };

  final Map<String, dynamic> raw;

  /// Increases with every event, so a reader can tell what is new.
  final int seq;

  /// Time from the latest occurrence to when this was read.
  final Duration age;

  /// Occurrences folded into this entry.
  final int repeats;

  /// The event's own fields, its kind among them.
  final Map<String, dynamic> detail;

  /// As the SDK names it: `relay_ready`, `port_mapping_changed`, …
  String get kind => detail['kind'] as String? ?? '';

  /// `relay_failed`: why, as the SDK names it.
  String? get failure => detail['failure'] as String?;

  /// `direct_stage_changed` and `direct_fallback`.
  String? get stage => detail['stage'] as String?;

  /// `phase_changed`.
  String? get phase => detail['phase'] as String?;

  /// `session_opened` and `session_closed`: `direct` or `relay`.
  String? get path => detail['path'] as String?;
  Duration? get latency => _duration(detail['latency_ms']);
  Duration? get setup => _duration(detail['setup_ms']);
  Duration? get retryIn => _duration(detail['retry_in_ms']);

  /// `port_mapping_changed`: the mapping as it now stands.
  PortMappingStatus? get mapping => switch (detail['mapping']) {
    final Map<dynamic, dynamic> m => PortMappingStatus.fromMap(_map(m)),
    _ => null,
  };
}

/// Where a gateway mapping stands; see the SDK's `PortMappingStage`.
enum PortMappingStage {
  waitingForDirect('waiting_for_direct'),
  discovering('discovering'),
  gatewayFound('gateway_found'),
  discovered('discovered'),
  requesting('requesting'),
  active('active'),
  renewing('renewing'),
  expired('expired'),
  retrying('retrying'),
  networkChanged('network_changed'),
  releasing('releasing'),
  released('released'),
  releaseSent('release_sent'),
  failed('failed'),
  releaseFailed('release_failed'),
  cancelled('cancelled'),
  stopped('stopped');

  const PortMappingStage(this.wire);

  /// The SDK's name for it.
  final String wire;

  static PortMappingStage? parse(Object? raw) =>
      values.firstWhereOrNull((stage) => stage.wire == raw);
}

/// How a gateway is asked for a mapping.
enum PortMappingProtocol {
  pcp('pcp', 'PCP'),
  natPmp('nat_pmp', 'NAT-PMP'),
  upnp('upnp', 'UPnP');

  const PortMappingProtocol(this.wire, this.label);

  final String wire;
  final String label;

  static PortMappingProtocol? parse(Object? raw) =>
      values.firstWhereOrNull((protocol) => protocol.wire == raw);
}

/// A tunnel's port mapping: where it stands and what it took.
class PortMappingDiagnostics {
  PortMappingDiagnostics.fromMap(Map<String, dynamic> map)
    : current = PortMappingStatus.fromMap(
        map['current'] is Map ? _map(map['current'] as Map) : const {},
      ),
      attempts = _int(map['attempts']),
      renewals = _int(map['renewals']),
      failures = _int(map['failures']);

  final PortMappingStatus current;
  final int attempts;
  final int renewals;
  final int failures;
}

/// A gateway mapping after its latest change.
class PortMappingStatus {
  PortMappingStatus.fromMap(Map<String, dynamic> map)
    : stage = PortMappingStage.parse(map['stage']),
      localPort = (map['local_port'] as num?)?.toInt(),
      gateway = map['gateway'] as String?,
      protocol = PortMappingProtocol.parse(map['protocol']),
      externalAddress = map['external_addr'] as String?,
      active = map['active'] == true,
      lease = _duration(map['lease_lifetime_ms']),
      error = map['error'] as String?;

  /// Null for a stage this app does not know yet.
  final PortMappingStage? stage;
  final int? localPort;
  final String? gateway;
  final PortMappingProtocol? protocol;
  final String? externalAddress;

  /// Whether a lease is usable now, a renewal pending or failed included.
  final bool active;
  final Duration? lease;
  final String? error;
}

Map<String, dynamic> _map(Map<dynamic, dynamic> map) =>
    Map<String, dynamic>.from(map);

int _int(Object? value, {int fallback = 0}) =>
    (value as num?)?.toInt() ?? fallback;

Duration? _duration(Object? ms) =>
    ms is num ? Duration(milliseconds: ms.toInt()) : null;
