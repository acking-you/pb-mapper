import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';

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
      other.message == message;

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
  );
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
  });

  final TunnelKind kind;
  final String key;
  final String localAddress;
  final String protocol;
  final bool encrypted;
  final bool keepAlive;

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
