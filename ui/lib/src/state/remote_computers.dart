import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';

import '../common/polling.dart';
import '../ffi/remote_computer_vault.dart';
import 'app_store.dart' show RelayState;
import 'desktop_store.dart';

/// Rust keeps at most this many (D20).
const kMaxRememberedComputers = 50;

/// A computer this device remembers, as Dart sees it: never its invitation,
/// which stays in Rust's vault and is used by [id].
@immutable
class RemoteComputer {
  const RemoteComputer({
    required this.id,
    required this.name,
    required this.service,
    required this.fingerprint,
    required this.relay,
    required this.namespace,
    this.relayOnly = false,
    this.quality = DesktopQuality.balanced,
    this.lastConnectedAt,
    this.lastPath,
    this.lastControlAllowed,
  });

  factory RemoteComputer.fromJson(Map<String, dynamic> json) => RemoteComputer(
    id: json['id'] as String,
    name: json['name'] as String,
    service: json['service'] as String,
    fingerprint: json['fingerprint'] as String,
    relay: json['relay'] as String? ?? '',
    namespace: json['namespace'] as int? ?? 0,
    relayOnly: json['relay_only'] == true,
    quality: desktopQualityFromWire(json['quality']),
    lastConnectedAt: switch (json['last_connected_ms']) {
      final int ms => DateTime.fromMillisecondsSinceEpoch(ms),
      _ => null,
    },
    lastPath: json['last_path'] as String?,
    lastControlAllowed: json['last_control_allowed'] as bool?,
  );

  /// 32 lowercase hex characters, generated here.
  final String id;
  final String name;

  /// `desktop:…`.
  final String service;

  /// The host certificate's short form, `AB12-CD34-EF56-7890`.
  final String fingerprint;

  /// The relay address it was remembered under.
  final String relay;

  /// The key id of the credential it was remembered under (0: the
  /// administrator key). Desktop trust binds to the credential, and since a
  /// temporary key can be issued into a shared namespace, this is not
  /// necessarily the namespace its services are in (D29).
  final int namespace;
  final bool relayOnly;

  /// The Display menu's choice for this computer (D12).
  final DesktopQuality quality;
  final DateTime? lastConnectedAt;

  /// `direct` or `relay`.
  final String? lastPath;
  final bool? lastControlAllowed;

  @override
  bool operator ==(Object other) =>
      other is RemoteComputer &&
      other.id == id &&
      other.name == name &&
      other.service == service &&
      other.fingerprint == fingerprint &&
      other.relay == relay &&
      other.namespace == namespace &&
      other.relayOnly == relayOnly &&
      other.quality == quality &&
      other.lastConnectedAt == lastConnectedAt &&
      other.lastPath == lastPath &&
      other.lastControlAllowed == lastControlAllowed;
  @override
  int get hashCode => Object.hash(
    id,
    name,
    service,
    fingerprint,
    relay,
    namespace,
    relayOnly,
    quality,
    lastConnectedAt,
    lastPath,
    lastControlAllowed,
  );
}

/// Whether this device can remember computers. Widgets say why not in their
/// own words (`addRememberUnavailable`); [RememberUnavailable.reason] is for
/// the log.
sealed class RememberSupport {
  const RememberSupport();
  const factory RememberSupport.available() = RememberAvailable;
  const factory RememberSupport.unavailable(String reason) =
      RememberUnavailable;

  bool get available => this is RememberAvailable;
}

final class RememberAvailable extends RememberSupport {
  const RememberAvailable();
}

final class RememberUnavailable extends RememberSupport {
  const RememberUnavailable(this.reason);
  final String reason;
}

/// The remembered computers and their last pictures (D20, D23), over the
/// vault. Nothing is ever written in plain text: a vault that fails turns
/// [support] unavailable instead.
class RemoteComputerStore extends ChangeNotifier {
  /// [thumbnailDir] is the private directory last pictures live in; it is
  /// created when missing.
  RemoteComputerStore(
    this._vault, {
    required Future<Directory> Function() thumbnailDir,
    math.Random? random,
  }) : _thumbnailDir = thumbnailDir,
       _random = random ?? math.Random.secure();

  final RemoteComputerVault _vault;
  final Future<Directory> Function() _thumbnailDir;
  final math.Random _random;

  RememberSupport _support = const RememberSupport.unavailable('loading');
  List<RemoteComputer> _computers = const [];
  Directory? _thumbnails;
  final _versions = <String, int>{};
  Future<void>? _loading;
  bool _disposed = false;

  /// Unavailable until [load] succeeds.
  RememberSupport get support => _support;

  /// The most recently connected first, never-connected last, then by name.
  List<RemoteComputer> get computers => _computers;

  /// No room for another: [rememberSession] of a new one returns null.
  bool get full => _computers.length >= kMaxRememberedComputers;

  RemoteComputer? byId(String id) =>
      _computers.firstWhereOrNull((c) => c.id == id);

  /// The computer [info] names: the same service and the same identity.
  RemoteComputer? sameIdentity(DesktopInvitationInfo info) =>
      _computers.firstWhereOrNull(
        (c) => c.service == info.service && c.fingerprint == info.fingerprint,
      );

  /// A computer saved under [info]'s service with another identity: a
  /// conflict to replace on purpose, never a duplicate to merge.
  RemoteComputer? sameServiceOtherIdentity(DesktopInvitationInfo info) =>
      _computers.firstWhereOrNull(
        (c) => c.service == info.service && c.fingerprint != info.fingerprint,
      );

  /// 128 random bits as lowercase hex.
  String newId() => [
    for (var i = 0; i < 16; i++)
      _random.nextInt(256).toRadixString(16).padLeft(2, '0'),
  ].join();

  /// Opens the vault and reads the list; on Android this unlocks the vault,
  /// which a connect by id needs first. Calls share one load; one that left
  /// the vault unavailable may be tried again.
  Future<void> load() => _loading ??= _load();

  Future<void> _load() async {
    try {
      final dir = await _thumbnailDir();
      if (!dir.existsSync()) await dir.create(recursive: true);
      _thumbnails = dir;
    } catch (error) {
      // Without a private directory there are no last pictures; the
      // placeholder shows instead.
      debugPrint('No directory for remote thumbnails: $error');
    }
    var support = await _vault.open();
    var computers = const <RemoteComputer>[];
    if (support.available) {
      try {
        computers = await _vault.list();
      } catch (error) {
        debugPrint('Remembered computers could not be read: $error');
        support = const RememberSupport.unavailable('unreadable');
      }
    }
    if (_disposed) return;
    if (!support.available) _loading = null;
    _support = support;
    _computers = _sorted(computers);
    notifyListeners();
  }

  static List<RemoteComputer> _sorted(Iterable<RemoteComputer> computers) =>
      computers.toList()..sort((a, b) {
        final at = a.lastConnectedAt, bt = b.lastConnectedAt;
        if (at != bt) {
          if (at == null) return 1;
          if (bt == null) return -1;
          return bt.compareTo(at);
        }
        return a.name.compareTo(b.name);
      });

  void _put(RemoteComputer record, {String? replacing}) {
    if (_disposed) return;
    _computers = _sorted([
      for (final c in _computers)
        if (c.id != record.id && c.id != replacing) c,
      record,
    ]);
    notifyListeners();
  }

  /// A vault that failed is not used again: nothing falls back to plain text.
  void _failed(Object error) {
    debugPrint('Remembered computers failed: $error');
    if (_disposed) return;
    _support = const RememberSupport.unavailable('failed');
    _loading = null;
    notifyListeners();
  }

  /// Remembers the live session under [id], after its first picture (D20).
  /// [service] and [fingerprint] name the computer the caller believes is
  /// connected; the vault refuses when the live session is another one.
  /// Null when it was not remembered: the vault is unavailable or [full], or
  /// it refused.
  Future<RemoteComputer?> rememberSession({
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
    await load();
    if (!_support.available) return null;
    final replacing =
        byId(id) != null || (replaceId != null && byId(replaceId) != null);
    if (full && !replacing) return null;
    try {
      final record = await _vault.rememberSession(
        id: id,
        name: name,
        service: service,
        fingerprint: fingerprint,
        relay: relay,
        namespace: namespace,
        relayOnly: relayOnly,
        quality: quality,
        replaceId: replaceId,
      );
      _put(record, replacing: replaceId);
      return record;
    } catch (error) {
      if (error is RemoteComputerVaultException &&
          error.code == 'INVALID_ARGUMENT') {
        // Refused: the session ended or is another computer's by now.
        debugPrint('Not remembered: $error');
      } else {
        _failed(error);
      }
      return null;
    }
  }

  Future<void> _update(
    String id, {
    String? name,
    DesktopQuality? quality,
    DateTime? lastConnectedAt,
    String? lastPath,
    bool? lastControlAllowed,
  }) async {
    if (!_support.available || byId(id) == null) return;
    try {
      _put(
        await _vault.update(
          id,
          name: name,
          quality: quality,
          lastConnectedAt: lastConnectedAt,
          lastPath: lastPath,
          lastControlAllowed: lastControlAllowed,
        ),
      );
    } catch (error) {
      if (error is! RemoteComputerVaultException || error.code != 'NOT_FOUND') {
        _failed(error);
      } else if (!_disposed) {
        // Forgotten elsewhere meanwhile.
        _computers = [
          for (final c in _computers)
            if (c.id != id) c,
        ];
        notifyListeners();
      }
    }
  }

  Future<void> rename(String id, String name) => _update(id, name: name);

  Future<void> setQuality(String id, DesktopQuality quality) =>
      _update(id, quality: quality);

  Future<void> recordSession(
    String id, {
    required DateTime at,
    String? path,
    bool? controlAllowed,
  }) => _update(
    id,
    lastConnectedAt: at,
    lastPath: path,
    lastControlAllowed: controlAllowed,
  );

  /// Forgets [id] in the vault, then deletes its last picture (D23).
  Future<void> forget(String id) async {
    try {
      await _vault.forget(id);
    } catch (error) {
      _failed(error);
      return;
    }
    final file = thumbnailFile(id);
    if (file != null) {
      try {
        if (await file.exists()) await file.delete();
      } catch (error) {
        debugPrint('The last picture could not be deleted: $error');
      }
      _evict(file);
    }
    _versions.remove(id);
    if (_disposed) return;
    _computers = [
      for (final c in _computers)
        if (c.id != id) c,
    ];
    notifyListeners();
  }

  static final _validId = RegExp(r'^[0-9a-f]{32}$');

  /// Where [id]'s last picture lives; it may not exist. Null without a
  /// private directory, or for an id that is not one of ours.
  File? thumbnailFile(String id) {
    final dir = _thumbnails;
    if (dir == null || !_validId.hasMatch(id)) return null;
    return File('${dir.path}/$id.thumb');
  }

  /// Changes whenever [id]'s last picture is rewritten; image keys use it.
  int thumbnailVersion(String id) => _versions[id] ?? 0;

  /// A new last picture for [id] was written.
  void thumbnailWritten(String id) {
    final file = thumbnailFile(id);
    if (file == null || _disposed) return;
    _evict(file);
    _versions[id] = thumbnailVersion(id) + 1;
    notifyListeners();
  }

  static void _evict(File file) =>
      PaintingBinding.instance.imageCache.evict(FileImage(file));

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}

/// How a remembered computer stands, for its row (D22).
enum ComputerPresence { viewing, online, notSharing, unknown, otherRelay }

/// [credentialKeyId] is this computer's credential key id, null until it is
/// known. [desktopServices] are the relay's desktop services this credential
/// can connect to (`AppStore.desktopServiceNames`).
ComputerPresence presenceOf(
  RemoteComputer c, {
  required DesktopSessionView session,
  required RelayState relay,
  required String relayAddress,
  required int? credentialKeyId,
  required Set<String> desktopServices,
}) =>
    session.target?.computerId == c.id &&
        session.phase != DesktopPhase.idle &&
        session.phase != DesktopPhase.ended
    ? ComputerPresence.viewing
    : credentialKeyId == null
    ? ComputerPresence.unknown
    : c.relay != relayAddress || c.namespace != credentialKeyId
    ? ComputerPresence.otherRelay
    : relay != RelayState.online
    ? ComputerPresence.unknown
    : desktopServices.contains(c.service)
    ? ComputerPresence.online
    : ComputerPresence.notSharing;
