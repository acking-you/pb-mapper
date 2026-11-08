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

/// The longest name the vault keeps, in Unicode scalar values (`runes`),
/// which is what Rust counts; a character can be several.
const kComputerNameMax = 128;

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
    this.resolution = DesktopResolution.p1080,
    this.maxFps = 0,
    this.lastConnectedAt,
    this.lastPath,
    this.lastControlAllowed,
  });

  /// A record saved before resolution and frame rate were chosen has
  /// neither: it is 1080p at Auto, as it was viewed then.
  factory RemoteComputer.fromJson(Map<String, dynamic> json) => RemoteComputer(
    id: json['id'] as String,
    name: json['name'] as String,
    service: json['service'] as String,
    fingerprint: json['fingerprint'] as String,
    relay: json['relay'] as String? ?? '',
    namespace: json['namespace'] as int? ?? 0,
    relayOnly: json['relay_only'] == true,
    quality: desktopQualityFromWire(json['quality']),
    resolution: desktopResolutionFromWire(json['resolution']),
    maxFps: json['max_fps'] as int? ?? 0,
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

  /// The Display menu's choices for this computer: the picture preference
  /// (D12), the resolution (D7) and the maximum frame rate, 0 for Auto
  /// (D8).
  final DesktopQuality quality;
  final DesktopResolution resolution;
  final int maxFps;
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
      other.resolution == resolution &&
      other.maxFps == maxFps &&
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
    resolution,
    maxFps,
    lastConnectedAt,
    lastPath,
    lastControlAllowed,
  );
}

/// Whether this device can remember computers. Widgets say why not in their
/// own words: `unreadable` is saved computers that cannot be opened, which
/// [RemoteComputerStore.reset] clears; any other [RememberUnavailable.reason]
/// is for the log (`addRememberUnavailable`).
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

  /// Bumped whenever a computer's last picture is rewritten.
  final _versions = <String, int>{};

  /// The last picture each computer was shown with, dropped from the image
  /// cache once it is rewritten or forgotten.
  final _shown = <String, ImageProvider>{};
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
        // Only a file that was read and cannot be opened is offered for
        // clearing; any other failure may pass, and the next load tries
        // again.
        support = RememberSupport.unavailable(
          error is RemoteComputerVaultException && error.code == 'PROTOCOL'
              ? 'unreadable'
              : 'failed',
        );
      }
    }
    if (support.available) _deleteStrayPictures(computers);
    if (_disposed) return;
    if (!support.available) _loading = null;
    _support = support;
    _computers = _sorted(computers);
    notifyListeners();
  }

  /// Deletes the last pictures of computers the vault does not have: it was
  /// reset, or a forget could not delete one (D23). At most a few dozen
  /// small files, once a load.
  void _deleteStrayPictures(List<RemoteComputer> computers) {
    final dir = _thumbnails;
    if (dir == null) return;
    final kept = {for (final c in computers) c.id};
    try {
      for (final entry in dir.listSync()) {
        final name = entry.uri.pathSegments.last;
        if (entry is! File || !name.endsWith(_pictureSuffix)) continue;
        final id = name.substring(0, name.length - _pictureSuffix.length);
        if (kept.contains(id)) continue;
        entry.deleteSync();
        _forgetPicture(id);
      }
    } catch (error) {
      debugPrint('Old last pictures could not be deleted: $error');
    }
  }

  /// Deletes every remembered computer and its last picture, then loads
  /// again: the way out of saved computers that cannot be opened.
  Future<void> reset() async {
    try {
      await _vault.reset();
    } catch (error) {
      // Still unreadable after the load below, so the page still offers it.
      debugPrint('Remembered computers could not be reset: $error');
    }
    _loading = null;
    await load();
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
    DesktopResolution resolution = DesktopResolution.p1080,
    int maxFps = 0,
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
        resolution: resolution,
        maxFps: maxFps,
        replaceId: replaceId,
      );
      _put(record, replacing: replaceId);
      // Replaced under another id, the old record's last picture is
      // nobody's.
      if (replaceId != null && replaceId != record.id) {
        await _deleteThumbnail(replaceId);
      }
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
    DesktopResolution? resolution,
    int? maxFps,
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
          resolution: resolution,
          maxFps: maxFps,
          lastConnectedAt: lastConnectedAt,
          lastPath: lastPath,
          lastControlAllowed: lastControlAllowed,
        ),
      );
    } catch (error) {
      final code = error is RemoteComputerVaultException ? error.code : null;
      if (code == 'INVALID_ARGUMENT') {
        // Refused: the record stays as it was, and the vault still works.
        debugPrint('Not changed: $error');
      } else if (code != 'NOT_FOUND') {
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

  Future<void> setResolution(String id, DesktopResolution resolution) =>
      _update(id, resolution: resolution);

  Future<void> setMaxFps(String id, int fps) => _update(id, maxFps: fps);

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
    await _deleteThumbnail(id);
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
    return File('${dir.path}/$id$_pictureSuffix');
  }

  static const _pictureSuffix = '.thumb';

  /// [id]'s last picture, decoded [width] pixels wide; null without a
  /// private directory. Its image-cache key carries the picture's version,
  /// so a rewritten file is decoded afresh: nothing cached from the old
  /// file, or from a load that ran before the file was written, is served
  /// for it.
  ImageProvider? thumbnailImage(String id, {required int width}) {
    final file = thumbnailFile(id);
    if (file == null) return null;
    return _shown[id] = ResizeImage(
      _ThumbnailImage(file, _versions[id] ?? 0),
      width: width,
    );
  }

  /// A new last picture for [id] was written.
  void thumbnailWritten(String id) {
    if (thumbnailFile(id) == null || _disposed) return;
    _evict(id);
    _versions[id] = (_versions[id] ?? 0) + 1;
    notifyListeners();
  }

  void _evict(String id) => unawaited(_shown.remove(id)?.evict());

  /// Forgets the shown image and the version of [id]'s last picture, whose
  /// file is gone.
  void _forgetPicture(String id) {
    _evict(id);
    _versions.remove(id);
  }

  /// Deletes the last picture of [id], whose record is gone (D23).
  Future<void> _deleteThumbnail(String id) async {
    final file = thumbnailFile(id);
    if (file != null) {
      try {
        if (await file.exists()) await file.delete();
      } catch (error) {
        debugPrint('The last picture could not be deleted: $error');
      }
    }
    _forgetPicture(id);
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}

/// One version of a last picture: the file is rewritten in place, so its
/// version is part of the cache key, and each rewrite is a new image.
@immutable
class _ThumbnailImage extends FileImage {
  const _ThumbnailImage(super.file, this.version);
  final int version;

  @override
  bool operator ==(Object other) =>
      other is _ThumbnailImage &&
      other.file.path == file.path &&
      other.version == version;

  @override
  int get hashCode => Object.hash(file.path, version);
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
}) => session.target?.computerId == c.id && session.open
    ? ComputerPresence.viewing
    : listedPresence(
        service: c.service,
        relay: c.relay,
        namespace: c.namespace,
        relayState: relay,
        relayAddress: relayAddress,
        credentialKeyId: credentialKeyId,
        desktopServices: desktopServices,
      );

/// How a computer sharing [service] on [relay] under the credential
/// [namespace] stands, as the relay this computer uses lists it: what
/// [presenceOf] says apart from viewing, and what the viewer says of the
/// computer it shows.
ComputerPresence listedPresence({
  required String service,
  required String relay,
  required int namespace,
  required RelayState relayState,
  required String relayAddress,
  required int? credentialKeyId,
  required Set<String> desktopServices,
}) => credentialKeyId == null
    ? ComputerPresence.unknown
    : relay != relayAddress || namespace != credentialKeyId
    ? ComputerPresence.otherRelay
    : relayState != RelayState.online
    ? ComputerPresence.unknown
    : desktopServices.contains(service)
    ? ComputerPresence.online
    : ComputerPresence.notSharing;
