import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';

export 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart'
    show DirectStage, RelayTransport, TunnelPath;

/// Which end of a tunnel this computer is.
enum TunnelKind {
  /// This computer publishes a local service under a key (`register`).
  share,

  /// This computer reaches a published key on a local port (`connect`).
  connect,
}

/// What a tunnel is doing, as the Rust side reports it.
enum TunnelHealth {
  running,

  /// In the retry loop: the relay or the local service is not answering yet.
  retrying,

  /// The last probe failed. The retry loop is still running — only [stopped]
  /// means there is no task left to stop.
  failed,
  stopped;

  static TunnelHealth parse(String raw) => switch (raw.toLowerCase()) {
    'running' => TunnelHealth.running,
    'retrying' => TunnelHealth.retrying,
    'failed' => TunnelHealth.failed,
    _ => TunnelHealth.stopped,
  };
}

/// One tunnel this computer owns: a share or a connection.
@immutable
class Tunnel {
  const Tunnel({
    required this.kind,
    required this.key,
    required this.localAddress,
    required this.protocol,
    required this.encrypted,
    required this.keepAlive,
    required this.health,
    this.message = '',
    this.createdAtMs = 0,
    this.namespace,
    this.routedNamespace,
    this.allowDirect = true,
    this.portMapping = false,
    this.path,
    this.relayTransport,
  });

  factory Tunnel.fromService(ServiceConfigInfo s) => Tunnel(
    kind: TunnelKind.share,
    key: s.serviceKey,
    localAddress: s.localAddress,
    protocol: s.protocol.toUpperCase(),
    encrypted: s.enableEncryption,
    keepAlive: s.enableKeepAlive,
    health: TunnelHealth.parse(s.status),
    message: s.statusMessage,
    createdAtMs: s.createdAtMs,
    allowDirect: s.allowDirect,
    portMapping: s.portMapping,
    path: s.path,
    relayTransport: s.relayTransport,
  );

  factory Tunnel.fromClient(ClientConfigInfo c) => Tunnel(
    kind: TunnelKind.connect,
    key: c.serviceKey,
    localAddress: c.localAddress,
    protocol: c.protocol.toUpperCase(),
    encrypted: false,
    keepAlive: c.enableKeepAlive,
    health: TunnelHealth.parse(c.status),
    message: c.statusMessage,
    createdAtMs: c.createdAtMs,
    namespace: c.namespace,
    routedNamespace: c.routedNamespace,
    allowDirect: c.allowDirect,
    portMapping: c.portMapping,
    path: c.path,
    relayTransport: c.relayTransport,
  );

  final TunnelKind kind;

  /// The service key on the relay.
  final String key;

  /// Where the service listens (a share) or where it appears (a connection).
  final String localAddress;

  /// `TCP` or `UDP`.
  final String protocol;

  /// Whether a share encrypts its traffic. Always false for a connection,
  /// which follows whatever the share chose.
  final bool encrypted;
  final bool keepAlive;
  final TunnelHealth health;

  /// The Rust side's last word on [health], if it had one.
  final String message;
  final int createdAtMs;

  /// For a connection to another credential's service, that credential's
  /// key id: only the administrator key reaches those. Null for one in this
  /// computer's own namespace, and for every share.
  final int? namespace;

  /// While a connection runs, the namespace the relay routes it in — which
  /// a credential changed since it started does not move. Null otherwise.
  final int? routedNamespace;

  /// Whether its traffic may go direct: for a connection, whether each
  /// session tries a direct path before the relay; for a share, whether it
  /// lets connections reach it that way.
  final bool allowDirect;
  final bool portMapping;

  /// How its sessions travel, while it runs.
  final TunnelPath? path;

  /// How it reaches the relay, while it runs: the carrier it started with,
  /// which a change in Settings does not move until it starts again.
  final RelayTransport? relayTransport;

  /// Whether what goes through the relay goes over QUIC.
  bool get overQuic => relayTransport == RelayTransport.quic;

  /// Which way its traffic goes, for a tunnel that is up; null otherwise,
  /// and for a share no connection has reached directly.
  PathMode? get pathMode {
    if (health != TunnelHealth.running) return null;
    if (!allowDirect) return PathMode.relayOnly;
    final path = this.path;
    if (kind == TunnelKind.share) {
      // A share cannot tell the sessions that came through the relay apart.
      return path != null && path.activeDirect > 0 ? PathMode.direct : null;
    }
    if (path == null) return PathMode.directFirst;
    return switch ((path.activeDirect > 0, path.activeRelay > 0)) {
      (true, true) => PathMode.mixed,
      (true, false) => PathMode.direct,
      (false, true) => PathMode.relay,
      // Idle: an attempt under way, or how the last one ended.
      (false, false) when path.attempt != null => PathMode.trying,
      (false, false) => switch (path.lastDirect) {
        true => PathMode.direct,
        false => PathMode.relay,
        null => PathMode.directFirst,
      },
    };
  }

  /// Unique across both kinds: a computer can share and connect one key.
  String get id => '${kind.name}:$key';

  /// Whether a background task exists to stop — not whether it works.
  bool get isLive => health != TunnelHealth.stopped;

  /// The Rust side's explanation, when there is something wrong to explain.
  /// A stopped tunnel's message only restates that it is stopped.
  String? get problem =>
      (health == TunnelHealth.failed || health == TunnelHealth.retrying) &&
          message.isNotEmpty
      ? message
      : null;

  TunnelDraft get draft => TunnelDraft(
    kind: kind,
    key: key,
    localAddress: localAddress,
    protocol: protocol,
    encrypted: encrypted,
    keepAlive: keepAlive,
    namespace: namespace,
    allowDirect: allowDirect,
    portMapping: portMapping,
  );

  @override
  bool operator ==(Object other) =>
      other is Tunnel &&
      other.kind == kind &&
      other.key == key &&
      other.localAddress == localAddress &&
      other.protocol == protocol &&
      other.encrypted == encrypted &&
      other.keepAlive == keepAlive &&
      other.health == health &&
      other.message == message &&
      other.namespace == namespace &&
      other.routedNamespace == routedNamespace &&
      other.allowDirect == allowDirect &&
      other.portMapping == portMapping &&
      other.path == path &&
      other.relayTransport == relayTransport;

  @override
  int get hashCode => Object.hash(
    kind,
    key,
    localAddress,
    protocol,
    encrypted,
    keepAlive,
    health,
    message,
    namespace,
    routedNamespace,
    allowDirect,
    portMapping,
    path,
    relayTransport,
  );
}

/// Which way a running tunnel's traffic goes, as it is shown.
enum PathMode {
  /// Kept on the relay by choice.
  relayOnly,

  /// Nothing has gone through it yet; each session will try direct first.
  directFirst,

  /// A direct path is being set up.
  trying,

  /// Straight between the two computers, now or last time.
  direct,

  /// Through the relay, now or last time.
  relay,

  /// Some sessions direct and some through the relay, now.
  mixed,
}

/// What the new-tunnel form produces.
@immutable
class TunnelDraft {
  const TunnelDraft({
    required this.kind,
    required this.key,
    required this.localAddress,
    this.protocol = 'TCP',
    this.encrypted = true,
    this.keepAlive = true,
    this.namespace,
    this.allowDirect = true,
    this.portMapping = false,
  });

  final TunnelKind kind;
  final String key;
  final String localAddress;
  final String protocol;
  final bool encrypted;
  final bool keepAlive;

  /// See [Tunnel.namespace]. A connection only.
  final int? namespace;

  /// See [Tunnel.allowDirect].
  final bool allowDirect;
  final bool portMapping;

  /// The address a fresh form starts with: the conventional local service
  /// port for a share, a free-looking one for a connection.
  static String defaultAddress(TunnelKind kind) =>
      kind == TunnelKind.share ? '127.0.0.1:8080' : '127.0.0.1:9090';
}

/// Whether [value] is a `host:port` with a port in range.
///
/// IPv6 literals are accepted bracketed (`[::1]:8080`), the form the Rust
/// resolver takes; a bare `::1:8080` is ambiguous and refused.
bool looksLikeHostPort(String value) {
  final text = value.trim();
  final colon = text.lastIndexOf(':');
  if (colon <= 0 || colon == text.length - 1) return false;
  final host = text.substring(0, colon);
  if (host.contains(':') && !(host.startsWith('[') && host.endsWith(']'))) {
    return false;
  }
  final port = int.tryParse(text.substring(colon + 1));
  return port != null && port > 0 && port <= 65535;
}

/// Whether [key] can be a process credential: empty (none yet), a
/// 32-character administrator key, or a `pbmt1_` temporary credential.
bool looksLikeCredential(String key) {
  final text = key.trim();
  return text.isEmpty || text.length == 32 || text.startsWith('pbmt1_');
}

/// The colour of a tunnel's state.
Color healthColor(ColorScheme scheme, TunnelHealth health) => switch (health) {
  TunnelHealth.running => successColor(scheme),
  TunnelHealth.retrying => cautionColor(scheme),
  TunnelHealth.failed => scheme.error,
  TunnelHealth.stopped => scheme.outline,
};

/// The colour of a service the relay lists that this computer does not use:
/// present, but nobody here is on it.
Color availableColor(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? const Color(0xFFB9B9B5)
    : const Color(0xFF8E8E92);
