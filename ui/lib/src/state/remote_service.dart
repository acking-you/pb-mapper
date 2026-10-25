import 'package:flutter/foundation.dart';

/// How a relay service is told apart on this side: the bare name in the
/// administrator's namespace, `namespace/name` in a temporary credential's.
/// The relay routes by the two together, so two credentials can each
/// register the same name.
String relayServiceId(int namespace, String name) =>
    namespace == 0 ? name : '$namespace/$name';

/// A service the relay lists, as this computer can reach it.
@immutable
class RemoteService {
  const RemoteService({required this.name, this.namespace = 0, int? owner})
    : owner = owner ?? namespace;

  final String name;

  /// Whose services it is among: 0 for the administrator's, or a temporary
  /// credential's key id.
  final int namespace;

  /// The credential that registered it: usually [namespace], unless the
  /// administrator registered it into a credential's namespace.
  final int owner;

  String get id => relayServiceId(namespace, name);

  @override
  bool operator ==(Object other) =>
      other is RemoteService &&
      other.name == name &&
      other.namespace == namespace &&
      other.owner == owner;

  @override
  int get hashCode => Object.hash(name, namespace, owner);
}

/// What a service is called where it is shown: its name, and whose it is
/// when it is in another credential's [namespace], as `cache · #3`.
String remoteLabel(String name, int? namespace) =>
    namespace == null || namespace == 0 ? name : '$name · #$namespace';
