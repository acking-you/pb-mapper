import 'package:pb_mapper_ui/src/ffi/remote_computer_vault.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';

/// An in-memory vault. It can be unavailable or fail to [list], and it
/// refuses as Rust does: at most [capacity] computers, names of at most 128
/// Unicode scalar values, and (when [live] is set) only the computer the
/// live session is.
class FakeRemoteComputerVault implements RemoteComputerVault {
  FakeRemoteComputerVault({
    this.support = const RememberSupport.available(),
    this.capacity = kMaxRememberedComputers,
  });

  RememberSupport support;
  final int capacity;
  final records = <RemoteComputer>[];

  /// The code [list] fails with until [reset]: `PROTOCOL` is a file that is
  /// there but cannot be opened, `IO` a read that failed.
  String? listFails;

  /// The live session's service and short fingerprint, as Rust's
  /// `current_invitation()`; null accepts any.
  (String service, String fingerprint)? live;

  /// Every call by op name, with the arguments that change something.
  final List<(String op, Map<String, Object?> args)> calls = [];
  List<String> get ops => [for (final call in calls) call.$1];

  static const _refused = RemoteComputerVaultException('INVALID_ARGUMENT');

  @override
  Future<RememberSupport> open() async {
    calls.add(('open', const {}));
    return support;
  }

  @override
  Future<List<RemoteComputer>> list() async {
    calls.add(('list', const {}));
    if (listFails case final code?) throw RemoteComputerVaultException(code);
    return [...records];
  }

  @override
  Future<RemoteComputer> rememberSession({
    required String id,
    required String name,
    required String service,
    required String fingerprint,
    required String relay,
    required int namespace,
    required bool relayOnly,
    required DesktopQuality quality,
    String? replaceId,
  }) async {
    calls.add((
      'remember',
      {
        'id': id,
        'service': service,
        'fingerprint': fingerprint,
        'relay': relay,
        'namespace': namespace,
        'replace_id': replaceId,
      },
    ));
    if (!support.available || !_validName(name)) throw _refused;
    if (live case (final s, final f) when s != service || f != fingerprint) {
      throw _refused;
    }
    final at = records.indexWhere((c) => c.id == id || c.id == replaceId);
    if (at < 0 && records.length >= capacity) throw _refused;
    final record = RemoteComputer(
      id: id,
      name: name,
      service: service,
      fingerprint: fingerprint,
      relay: relay,
      namespace: namespace,
      relayOnly: relayOnly,
      quality: quality,
    );
    if (at < 0) {
      records.add(record);
    } else {
      records[at] = record;
    }
    return record;
  }

  @override
  Future<RemoteComputer> update(
    String id, {
    String? name,
    DesktopQuality? quality,
    DateTime? lastConnectedAt,
    String? lastPath,
    bool? lastControlAllowed,
  }) async {
    calls.add(('update', {'id': id}));
    final at = records.indexWhere((c) => c.id == id);
    if (at < 0) throw const RemoteComputerVaultException('NOT_FOUND');
    if (name != null && !_validName(name)) throw _refused;
    final c = records[at];
    return records[at] = RemoteComputer(
      id: c.id,
      name: name ?? c.name,
      service: c.service,
      fingerprint: c.fingerprint,
      relay: c.relay,
      namespace: c.namespace,
      relayOnly: c.relayOnly,
      quality: quality ?? c.quality,
      lastConnectedAt: lastConnectedAt ?? c.lastConnectedAt,
      lastPath: lastPath ?? c.lastPath,
      lastControlAllowed: lastControlAllowed ?? c.lastControlAllowed,
    );
  }

  @override
  Future<void> forget(String id) async {
    calls.add(('forget', {'id': id}));
    records.removeWhere((c) => c.id == id);
  }

  @override
  Future<void> reset() async {
    calls.add(('reset', const {}));
    records.clear();
    listFails = null;
  }

  /// Rust's `Record::check`, which counts `chars()`.
  static bool _validName(String name) =>
      name.trim().isNotEmpty && name.runes.length <= kComputerNameMax;
}
