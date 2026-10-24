// The relay as a whole, as `pb_mapper_relay_call_json` reports it: who this
// computer is to the relay, the relay's inventories, its credentials and its
// security state. Re-exported by pb_mapper_api.dart.

/// A relay call that failed. [message] is for a person; [code] is the FFI's
/// error code (`SERVER_UNREACHABLE`, `INVALID_ARGUMENT`, …), for code.
class RelayCallException implements Exception {
  const RelayCallException(this.message, {this.code = 'INTERNAL'});

  final String message;
  final String code;

  @override
  String toString() => message;
}

/// What this computer's credential makes it, to the relay.
enum RelayRole {
  /// The relay's administrator key: every operation.
  admin,

  /// A temporary credential, confined to its own namespace.
  temporary,

  /// No usable credential.
  none;

  static RelayRole parse(Object? raw) => switch (raw) {
    'admin' => RelayRole.admin,
    'temporary' => RelayRole.temporary,
    _ => RelayRole.none,
  };
}

class RelayIdentity {
  const RelayIdentity({required this.role, this.keyId, this.error});

  final RelayRole role;

  /// A temporary credential's id, which is also its namespace.
  final int? keyId;

  /// Why there is no usable credential.
  final String? error;

  bool get isAdmin => role == RelayRole.admin;

  factory RelayIdentity.fromMap(Map<String, dynamic> map) => RelayIdentity(
    role: RelayRole.parse(map['role']),
    keyId: _int(map['keyId']),
    error: map['error'] as String?,
  );

  static const unknown = RelayIdentity(role: RelayRole.none);
}

/// The relay's credential subsystem, as an administrator sees it.
class RelayAuthStatus {
  const RelayAuthStatus({
    required this.safeMode,
    required this.capacity,
    required this.activeKeys,
    required this.expiredKeys,
    required this.revokedKeys,
    required this.legacyAllowed,
    required this.activeLegacyConnections,
    required this.lastLegacyConnectionAt,
    required this.authSuccesses,
    required this.authFailures,
    required this.serverInstanceId,
  });

  /// The relay could not load its credential state and refuses changes.
  final bool safeMode;
  final int capacity;
  final int activeKeys;
  final int expiredKeys;
  final int revokedKeys;

  /// Whether clients older than the v2 protocol may still connect.
  final bool legacyAllowed;
  final int activeLegacyConnections;

  /// Unix seconds.
  final int? lastLegacyConnectionAt;
  final int authSuccesses;
  final int authFailures;
  final String serverInstanceId;

  factory RelayAuthStatus.fromMap(Map<String, dynamic> map) => RelayAuthStatus(
    safeMode: map['safeMode'] == true,
    capacity: _int(map['capacity']) ?? 0,
    activeKeys: _int(map['activeKeys']) ?? 0,
    expiredKeys: _int(map['expiredKeys']) ?? 0,
    revokedKeys: _int(map['revokedKeys']) ?? 0,
    legacyAllowed: map['legacyAllowed'] == true,
    activeLegacyConnections: _int(map['activeLegacyConnections']) ?? 0,
    lastLegacyConnectionAt: _int(map['lastLegacyConnectionAt']),
    authSuccesses: _int(map['authSuccesses']) ?? 0,
    authFailures: _int(map['authFailures']) ?? 0,
    serverInstanceId: '${map['serverInstanceId'] ?? ''}',
  );
}

/// A service registered on the relay, from the administrator inventory.
///
/// The relay routes by [namespace] and [name] together: two credentials can
/// each register the same name, and those are two services.
class RelayService {
  const RelayService({
    required this.keyId,
    required this.namespace,
    required this.name,
    required this.transport,
    required this.encrypted,
    required this.connections,
  });

  /// The credential that registered it; 0 is the administrator's own.
  final int keyId;

  /// Whose services it is among: 0 for the administrator's, or a temporary
  /// credential's key id. Usually [keyId], unless the administrator
  /// registered it into a credential's namespace.
  final int namespace;
  final String name;
  final String transport;
  final bool encrypted;

  /// Control connections the relay holds for it.
  final int connections;

  factory RelayService.fromMap(Map<String, dynamic> map) => RelayService(
    keyId: _int(map['keyId']) ?? 0,
    namespace: _int(map['namespace']) ?? 0,
    name: '${map['name'] ?? ''}',
    transport: '${map['transport'] ?? ''}',
    encrypted: map['encrypted'] == true,
    connections: _int(map['connections']) ?? 0,
  );
}

/// A live control connection the relay holds for a service.
class RelayConnection {
  const RelayConnection({
    required this.keyId,
    required this.namespace,
    required this.service,
    required this.connId,
    required this.generation,
    required this.protocolVersion,
    required this.healthy,
    required this.transport,
    required this.encrypted,
    required this.lastRxAgeMs,
  });

  final int keyId;

  /// With [service], which service it serves; see [RelayService.namespace].
  final int namespace;
  final String service;
  final int connId;
  final int generation;
  final int protocolVersion;
  final bool healthy;
  final String transport;
  final bool encrypted;

  /// How long ago the relay last heard from it.
  final int lastRxAgeMs;

  factory RelayConnection.fromMap(Map<String, dynamic> map) => RelayConnection(
    keyId: _int(map['keyId']) ?? 0,
    namespace: _int(map['namespace']) ?? 0,
    service: '${map['service'] ?? ''}',
    connId: _int(map['connId']) ?? 0,
    generation: _int(map['generation']) ?? 0,
    protocolVersion: _int(map['protocolVersion']) ?? 0,
    healthy: map['healthy'] == true,
    transport: '${map['transport'] ?? ''}',
    encrypted: map['encrypted'] == true,
    lastRxAgeMs: _int(map['lastRxAgeMs']) ?? 0,
  );
}

/// The state a temporary credential is in.
enum RelayKeyState {
  active,
  expired,
  revoked,
  unknown;

  static RelayKeyState parse(Object? raw) => switch (raw) {
    'active' => RelayKeyState.active,
    'expired' => RelayKeyState.expired,
    'revoked' => RelayKeyState.revoked,
    _ => RelayKeyState.unknown,
  };
}

/// A temporary credential, without its secret.
class RelayKey {
  const RelayKey({
    required this.keyId,
    required this.state,
    required this.issuedAt,
    required this.expiresAt,
    this.label,
  });

  final int keyId;
  final RelayKeyState state;

  /// Unix seconds.
  final int issuedAt;
  final int expiresAt;
  final String? label;

  factory RelayKey.fromMap(Map<String, dynamic> map) => RelayKey(
    keyId: _int(map['keyId']) ?? 0,
    state: RelayKeyState.parse(map['state']),
    issuedAt: _int(map['issuedAt']) ?? 0,
    expiresAt: _int(map['expiresAt']) ?? 0,
    label: switch ((map['label'] as String?)?.trim()) {
      final label? when label.isNotEmpty => label,
      _ => null,
    },
  );
}

/// A temporary credential with its secret: what was just issued, renewed or
/// revealed.
class IssuedCredential extends RelayKey {
  const IssuedCredential({
    required super.keyId,
    required super.state,
    required super.issuedAt,
    required super.expiresAt,
    super.label,
    required this.credential,
  });

  /// The `pbmt1_…` text another computer uses as its credential.
  final String credential;

  factory IssuedCredential.fromMap(Map<String, dynamic> map) {
    final key = RelayKey.fromMap(map);
    return IssuedCredential(
      keyId: key.keyId,
      state: key.state,
      issuedAt: key.issuedAt,
      expiresAt: key.expiresAt,
      label: key.label,
      credential: '${map['credential'] ?? ''}',
    );
  }
}

/// A rotated administrator key.
class KeyRotation {
  const KeyRotation({
    required this.key,
    required this.confirmed,
    required this.saved,
    this.reason,
    this.saveError,
    this.restartShares = const [],
    this.restartConnections = const [],
  });

  final String key;

  /// The relay is known to use [key]. When false its reply was lost and it
  /// could not be asked again: it may be using [key] or the old key.
  final bool confirmed;

  /// This computer switched to [key]. Not when the outcome is unknown, nor
  /// when Settings moved it to another relay or credential meanwhile.
  final bool saved;

  /// Why the outcome is unknown, when it is.
  final String? reason;

  /// Why this computer could not save [key], when it tried and failed.
  final String? saveError;

  /// Once [saved], this computer's shares and connections on the rotated
  /// relay, by key: each still holds the credential it started with, which
  /// the rotation retired, so each has to start again.
  final List<String> restartShares;
  final List<String> restartConnections;

  factory KeyRotation.fromMap(Map<String, dynamic> map) {
    final restart = map['restart'];
    List<String> keys(String kind) => switch (restart) {
      final Map<dynamic, dynamic> lists => [
        for (final key in lists[kind] as List? ?? const []) '$key',
      ],
      _ => const [],
    };
    return KeyRotation(
      key: '${map['adminKey'] ?? ''}',
      confirmed: map['confirmed'] == true,
      saved: map['saved'] == true,
      reason: map['reason'] as String?,
      saveError: map['saveError'] as String?,
      restartShares: keys('shares'),
      restartConnections: keys('connections'),
    );
  }
}

/// A direct (peer-to-peer) session the relay has heard about from its ends.
class DirectSession {
  const DirectSession({
    required this.id,
    required this.service,
    required this.transport,
    this.path,
    this.state,
    this.reason,
    this.ageMs,
    required this.consumerSeen,
    required this.providerSeen,
    required this.stale,
    required this.conflicting,
  });

  final String id;
  final String service;
  final String transport;

  /// `direct` or `relay`: the path traffic takes now.
  final String? path;

  /// `active` or `closed`.
  final String? state;
  final String? reason;
  final int? ageMs;
  final bool consumerSeen;
  final bool providerSeen;

  /// The ends' reports are old, or disagree.
  final bool stale;
  final bool conflicting;

  bool get isDirect => path == 'direct';

  factory DirectSession.fromMap(Map<String, dynamic> map) => DirectSession(
    id: '${map['id'] ?? ''}',
    service: '${map['service'] ?? ''}',
    transport: '${map['transport'] ?? ''}',
    path: map['path'] as String?,
    state: map['state'] as String?,
    reason: map['reason'] as String?,
    ageMs: _int(map['ageMs']),
    consumerSeen: map['consumerSeen'] == true,
    providerSeen: map['providerSeen'] == true,
    stale: map['stale'] == true,
    conflicting: map['conflicting'] == true,
  );
}

int? _int(Object? value) => switch (value) {
  final int v => v,
  final num v => v.toInt(),
  final String v => int.tryParse(v),
  _ => null,
};
