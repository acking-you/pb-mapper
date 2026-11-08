import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../state/desktop_models.dart';
import '../state/remote_computers.dart';
import 'pb_mapper_service.dart';

/// Where remembered computers are kept (D20). The only Dart that reaches the
/// `computers_*` ops and the Android Keystore channel; no record it returns
/// holds an invitation or a token, and none ever crosses into Dart.
abstract interface class RemoteComputerVault {
  /// Unlocks the vault: on Android this is where the Keystore unwraps the
  /// data key. Never throws; a vault that cannot be used is unavailable.
  Future<RememberSupport> open();
  Future<List<RemoteComputer>> list();

  /// Stores the live session's invitation under [id]. The vault refuses when
  /// the live session is not [service] with [fingerprint]: a late answer from
  /// the previous computer during a fast switch must not land under [id].
  Future<RemoteComputer> rememberSession({
    required String id,
    required String name,
    required String service,
    required String fingerprint,
    required String relay,
    required int namespace,
    required bool relayOnly,
    required DesktopQuality quality,
    required DesktopResolution resolution,
    required int maxFps,
    String? replaceId,
  });
  Future<RemoteComputer> update(
    String id, {
    String? name,
    DesktopQuality? quality,
    DesktopResolution? resolution,
    int? maxFps,
    DateTime? lastConnectedAt,
    String? lastPath,
    bool? lastControlAllowed,
  });

  /// Idempotent.
  Future<void> forget(String id);

  /// Deletes every remembered computer: the way out of a vault that can no
  /// longer be read.
  Future<void> reset();
}

/// A refused vault call. [code] is the FFI `ErrorCode`: `INVALID_ARGUMENT`
/// for a refusal (no session, another computer, the vault is full),
/// `NOT_FOUND`, `IO`, `PROTOCOL` (unreadable) or `VAULT_UNAVAILABLE`.
class RemoteComputerVaultException implements Exception {
  const RemoteComputerVaultException(this.code, [this.message = '']);
  final String code, message;
  @override
  String toString() => message.isEmpty ? code : '$code: $message';
}

/// The vault this build uses.
RemoteComputerVault defaultRemoteComputerVault() =>
    FfiRemoteComputerVault(PbMapperService());

/// A platform that cannot keep invitations securely: nothing is remembered.
final class UnavailableRemoteComputerVault implements RemoteComputerVault {
  const UnavailableRemoteComputerVault();

  static const _refused = RemoteComputerVaultException(
    'INVALID_ARGUMENT',
    'remembering computers is not supported here',
  );

  @override
  Future<RememberSupport> open() async =>
      const RememberSupport.unavailable('unsupported');
  @override
  Future<List<RemoteComputer>> list() async => const [];
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
    required DesktopResolution resolution,
    required int maxFps,
    String? replaceId,
  }) => Future.error(_refused);
  @override
  Future<RemoteComputer> update(
    String id, {
    String? name,
    DesktopQuality? quality,
    DesktopResolution? resolution,
    int? maxFps,
    DateTime? lastConnectedAt,
    String? lastPath,
    bool? lastControlAllowed,
  }) => Future.error(_refused);
  @override
  Future<void> forget(String id) async {}
  @override
  Future<void> reset() async {}
}

/// The vault in Rust (`desktop_computers.rs`), sealed with DPAPI on Windows
/// and with an Android-Keystore-wrapped AES key on Android. Elsewhere
/// [open] reports it unavailable.
final class FfiRemoteComputerVault implements RemoteComputerVault {
  FfiRemoteComputerVault(
    this._service, {
    MethodChannel keystore = const MethodChannel('pb_mapper/keystore'),
  }) : _keystore = keystore;

  final PbMapperService _service;
  final MethodChannel _keystore;

  Future<Object?> _call(
    String op, [
    Map<String, Object?> args = const {},
  ]) async {
    final result = await _service.desktopCall({'op': op, ...args});
    if (result['success'] != true) {
      throw RemoteComputerVaultException(
        result['code'] as String? ?? 'INTERNAL',
        result['message'] as String? ?? '',
      );
    }
    return result['data'];
  }

  Future<RemoteComputer> _record(String op, Map<String, Object?> args) async =>
      RemoteComputer.fromJson(
        Map<String, dynamic>.from(await _call(op, args) as Map),
      );

  @override
  Future<RememberSupport> open() async {
    try {
      final support = Map<String, dynamic>.from(
        await _call('computers_support') as Map,
      );
      if (support['available'] != true) {
        // The Rust reason is for the log; the page says it in its own words.
        debugPrint('Remembered computers unavailable: ${support['reason']}');
        return const RememberSupport.unavailable('unsupported');
      }
      if (support['keystore'] == true) await _unlock();
      return const RememberSupport.available();
    } catch (error) {
      debugPrint('Remembered computers could not be opened: $error');
      return const RememberSupport.unavailable('failed');
    }
  }

  /// Android: unwraps the data key with the Keystore and hands it to Rust,
  /// once per process. The key crosses Dart as an immutable base64 string
  /// that only garbage collection clears; that is accepted, because the
  /// unlocked vault sits in this process's memory anyway. Invitations never
  /// cross.
  Future<void> _unlock() async {
    final stored = Map<String, dynamic>.from(
      await _call('computers_key') as Map,
    );
    final wrapped = stored['wrapped'] as String?;
    String? gone;
    if (wrapped != null) {
      try {
        final key = await _keystore.invokeMethod<Uint8List>(
          'unwrap',
          base64Decode(wrapped),
        );
        if (key == null) {
          throw const RemoteComputerVaultException(
            'IO',
            'the Keystore unwrap failed',
          );
        }
        await _call('computers_unlock', {'key': base64Encode(key)});
        return;
      } on PlatformException catch (error) {
        // Any other Keystore failure may pass: the vault is left as it is,
        // and the next load tries again.
        if (error.code != 'KEY_GONE') rethrow;
        // The Keystore key is gone, or no longer opens its wrapped copy:
        // what it sealed can never be read again, so the vault starts
        // empty under a new key.
        gone = error.message ?? error.code;
      }
    }
    final key = Uint8List.fromList([
      for (var i = 0; i < 32; i++) _random.nextInt(256),
    ]);
    final sealed = await _keystore.invokeMethod<Uint8List>('wrap', key);
    if (sealed == null) {
      throw const RemoteComputerVaultException(
        'IO',
        'the Keystore wrap failed',
      );
    }
    // Only a Keystore that seals a new key starts the vault over: one that
    // cannot may be failing for now, and the next load tries again.
    if (gone != null) {
      await reset();
      debugPrint('Remembered computers reset: $gone');
    }
    await _call('computers_unlock', {
      'key': base64Encode(key),
      'wrapped': base64Encode(sealed),
    });
  }

  static final _random = math.Random.secure();

  @override
  Future<List<RemoteComputer>> list() async => [
    for (final record in await _call('computers_list') as List)
      RemoteComputer.fromJson(Map<String, dynamic>.from(record as Map)),
  ];

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
    required DesktopResolution resolution,
    required int maxFps,
    String? replaceId,
  }) => _record('computers_remember', {
    'id': id,
    'name': name,
    'service': service,
    'fingerprint': fingerprint,
    'relay': relay,
    'namespace': namespace,
    'relay_only': relayOnly,
    'quality': quality.name,
    'resolution': resolution.wire,
    'max_fps': maxFps,
    'replace_id': ?replaceId,
  });

  @override
  Future<RemoteComputer> update(
    String id, {
    String? name,
    DesktopQuality? quality,
    DesktopResolution? resolution,
    int? maxFps,
    DateTime? lastConnectedAt,
    String? lastPath,
    bool? lastControlAllowed,
  }) => _record('computers_update', {
    'id': id,
    'name': ?name,
    'quality': ?quality?.name,
    'resolution': ?resolution?.wire,
    'max_fps': ?maxFps,
    'last_connected_ms': ?lastConnectedAt?.millisecondsSinceEpoch,
    'last_path': ?lastPath,
    'last_control_allowed': ?lastControlAllowed,
  });

  @override
  Future<void> forget(String id) => _call('computers_forget', {'id': id});

  @override
  Future<void> reset() => _call('computers_reset');
}
