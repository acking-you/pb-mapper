import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/painting.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/ffi/remote_computer_vault.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_actions.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';

import 'fake_pb_mapper_api.dart';
import 'fake_remote_computer_vault.dart';

const _office = DesktopInvitationInfo(
  service: 'desktop:office',
  name: 'office',
  fingerprint: 'AB12-CD34-EF56-7890',
  format: DesktopInvitationFormat.pbd1,
);

/// Answers the vault's ops as Rust does on Android, with a wrapped key on
/// disk.
class _AndroidVaultService implements PbMapperService {
  /// The vault's ops, and the Keystore calls between them.
  final ops = <String>[];

  @override
  Future<Map<String, dynamic>> desktopCall(Map<String, Object?> request) async {
    final op = request['op'] as String;
    ops.add(op);
    return {
      'success': true,
      'data': switch (op) {
        'computers_support' => {'available': true, 'keystore': true},
        'computers_key' => {'wrapped': base64Encode(Uint8List(60))},
        _ => const <String, Object?>{},
      },
    };
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('only a Keystore key that is gone starts the vault over; any other '
      'Keystore failure leaves it for the next load', () async {
    const keystore = MethodChannel('test/keystore');
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    addTearDown(() => messenger.setMockMethodCallHandler(keystore, null));
    for (final (code, starts) in [('KEYSTORE', false), ('KEY_GONE', true)]) {
      final service = _AndroidVaultService();
      messenger.setMockMethodCallHandler(keystore, (call) async {
        service.ops.add(call.method);
        if (call.method == 'unwrap') throw PlatformException(code: code);
        return Uint8List(60);
      });
      final vault = FfiRemoteComputerVault(service, keystore: keystore);
      expect((await vault.open()).available, starts);
      // The new key is sealed before the old vault goes: a Keystore that
      // cannot seal one now leaves the vault for the next load.
      expect(service.ops, [
        'computers_support',
        'computers_key',
        'unwrap',
        if (starts) ...['wrap', 'computers_reset', 'computers_unlock'],
      ]);
    }
  });

  test('saved computers that cannot be opened are cleared on request, with '
      'their last pictures', () async {
    final dir = await Directory.systemTemp.createTemp('pbm-remote-');
    addTearDown(() => dir.delete(recursive: true));
    final vault = FakeRemoteComputerVault()..listFails = 'IO';
    final store = RemoteComputerStore(vault, thumbnailDir: () async => dir);
    addTearDown(store.dispose);
    final picture = File('${dir.path}/${'a' * 32}.thumb');
    await picture.writeAsBytes(const [1]);

    // A read that failed may pass: nothing is offered for clearing.
    await store.load();
    expect(
      store.support,
      isA<RememberUnavailable>().having((s) => s.reason, 'reason', 'failed'),
    );

    vault.listFails = 'PROTOCOL';
    await store.load();
    expect(
      store.support,
      isA<RememberUnavailable>().having(
        (s) => s.reason,
        'reason',
        'unreadable',
      ),
    );
    // Whose pictures these are cannot be told yet.
    expect(await picture.exists(), isTrue);

    await store.reset();
    expect(store.support.available, isTrue);
    expect(await picture.exists(), isFalse);
  });

  test('a name the field takes is one the vault keeps, and a refused rename '
      'leaves the vault working', () async {
    final dir = await Directory.systemTemp.createTemp('pbm-remote-');
    addTearDown(() => dir.delete(recursive: true));
    final store = RemoteComputerStore(
      FakeRemoteComputerVault(),
      thumbnailDir: () async => dir,
    );
    addTearDown(store.dispose);
    await store.load();
    final id = store.newId();
    await store.rememberSession(
      id: id,
      name: 'Office',
      service: _office.service,
      fingerprint: _office.fingerprint,
      relay: 'relay.example:7666',
      namespace: 0,
      relayOnly: false,
      quality: DesktopQuality.balanced,
    );

    // A flag is one character of two scalar values; Rust counts the latter.
    final flags = '🇺🇳' * 100;
    final typed = computerNameLimit
        .formatEditUpdate(TextEditingValue.empty, TextEditingValue(text: flags))
        .text;
    await store.rename(id, typed);
    expect(store.byId(id)?.name, typed);

    await store.rename(id, flags);
    expect(store.support.available, isTrue);
    expect(store.byId(id)?.name, typed);
  });

  test('a full name refuses a keystroke rather than losing its end, and a '
      'composition is cut with the name', () {
    const max = kComputerNameMax;
    // Gboard composes the word under the cursor even for Latin letters.
    final full = TextEditingValue(
      text: 'a' * max,
      selection: const TextSelection.collapsed(offset: 1),
      composing: const TextRange(start: 0, end: 1),
    );
    final typed = computerNameLimit.formatEditUpdate(
      full,
      TextEditingValue(
        text: 'ab${'a' * (max - 1)}',
        selection: const TextSelection.collapsed(offset: 2),
        composing: const TextRange(start: 0, end: 2),
      ),
    );
    expect(typed, full);

    // Pinyin composing "ni" across the limit.
    final composed = computerNameLimit.formatEditUpdate(
      TextEditingValue(
        text: 'a' * (max - 1),
        selection: const TextSelection.collapsed(offset: max - 1),
      ),
      TextEditingValue(
        text: '${'a' * (max - 1)}ni',
        selection: const TextSelection.collapsed(offset: max + 1),
        composing: const TextRange(start: max - 1, end: max + 1),
      ),
    );
    expect(composed.text, '${'a' * (max - 1)}n');
    expect(composed.composing, const TextRange(start: max - 1, end: max));
  });

  test('a name refuses an invitation pasted over it, yet one that already '
      'has one can still be edited', () {
    // About 115 characters, as Rust encodes one: it fits a name whole.
    final invitation = 'pbd1.${'A' * 110}';
    const name = TextEditingValue(
      text: 'office-pc',
      selection: TextSelection(baseOffset: 0, extentOffset: 9),
    );
    final pasted = TextEditingValue(
      text: invitation,
      selection: TextSelection.collapsed(offset: invitation.length),
    );
    expect(computerNameLimit.formatEditUpdate(name, pasted), name);

    // A backspace at its end.
    final cut = TextEditingValue(
      text: invitation.substring(0, invitation.length - 1),
    );
    expect(
      computerNameLimit.formatEditUpdate(
        TextEditingValue(text: invitation),
        cut,
      ),
      cut,
    );
  });

  test('a changed identity is a conflict, not a duplicate, and a computer '
      'replaced or forgotten takes its thumbnail with it', () async {
    final dir = await Directory.systemTemp.createTemp('pbm-remote-');
    addTearDown(() => dir.delete(recursive: true));
    final vault = FakeRemoteComputerVault();
    final store = RemoteComputerStore(vault, thumbnailDir: () async => dir);
    addTearDown(store.dispose);
    await store.load();
    expect(store.support.available, isTrue);

    final id = store.newId();
    expect(id, matches(RegExp(r'^[0-9a-f]{32}$')));
    final saved = await store.rememberSession(
      id: id,
      name: 'Office',
      service: _office.service,
      fingerprint: _office.fingerprint,
      relay: 'relay.example:7666',
      namespace: 0,
      relayOnly: false,
      quality: DesktopQuality.balanced,
    );
    expect(saved?.id, id);

    // The same identity is the saved computer; a new certificate under the
    // same service is a conflict to replace on purpose, never a duplicate.
    const reissued = DesktopInvitationInfo(
      service: 'desktop:office',
      name: 'office',
      fingerprint: 'FFFF-0000-FFFF-0000',
      format: DesktopInvitationFormat.pbd1,
    );
    expect(store.sameIdentity(_office)?.id, id);
    expect(store.sameServiceOtherIdentity(_office), isNull);
    expect(store.sameIdentity(reissued), isNull);
    expect(store.sameServiceOtherIdentity(reissued)?.id, id);

    // Replaced under another id, its last picture goes with its record.
    final replaced = store.thumbnailFile(id)!;
    await replaced.writeAsBytes(const [1, 2, 3]);
    final next = store.newId();
    await store.rememberSession(
      id: next,
      name: 'Office',
      service: reissued.service,
      fingerprint: reissued.fingerprint,
      relay: 'relay.example:7666',
      namespace: 0,
      relayOnly: false,
      quality: DesktopQuality.balanced,
      replaceId: id,
    );
    expect(store.computers.map((c) => c.id), [next]);
    expect(await replaced.exists(), isFalse);

    final thumbnail = store.thumbnailFile(next)!;
    await thumbnail.writeAsBytes(const [1, 2, 3]);

    await store.forget(next);
    expect(await thumbnail.exists(), isFalse);
    expect(store.computers, isEmpty);
    expect(vault.records, isEmpty);
  });

  test(
    'a rewritten last picture is shown, at every width it was shown at',
    () async {
      final dir = await Directory.systemTemp.createTemp('pbm-remote-');
      addTearDown(() => dir.delete(recursive: true));
      final store = RemoteComputerStore(
        FakeRemoteComputerVault(),
        thumbnailDir: () async => dir,
      );
      addTearDown(store.dispose);
      await store.load();
      final id = store.newId();
      final file = store.thumbnailFile(id)!;

      /// Writes a 4 × [height] picture, as a viewer closing does.
      Future<void> write(int height) async {
        final recorder = ui.PictureRecorder();
        Canvas(recorder).drawColor(const Color(0xFF3366FF), BlendMode.src);
        final image = await recorder.endRecording().toImage(4, height);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        image.dispose();
        await file.writeAsBytes(png!.buffer.asUint8List(), flush: true);
      }

      /// The picture's shape as the card would decode it [width] wide.
      Future<double> shown(int width) async {
        final decoded = Completer<ImageInfo>();
        store
            .thumbnailImage(id, width: width)!
            .resolve(ImageConfiguration.empty)
            .addListener(
              ImageStreamListener(
                (info, _) => decoded.complete(info),
                onError: decoded.completeError,
              ),
            );
        final image = (await decoded.future).image;
        return image.width / image.height;
      }

      // Shown portrait and landscape, then rewritten.
      await write(2);
      expect(await shown(8), 2);
      expect(await shown(16), 2);
      await write(4);
      store.thumbnailWritten(id);
      expect(await shown(8), 1);
      expect(await shown(16), 1);
    },
  );

  test('presence follows the credential a computer was remembered under, and '
      "the administrator's viewer reaches only its own namespace", () async {
    final api = FakePbMapperApi(serverServices: ['desktop:office', 'web'])
      // The administrator also lists credential #3's desktop.
      ..namespacedServices = [(3, 'desktop:lab')];
    final app = AppStore(
      api: api,
      changes: const Stream.empty(),
      vault: FakeRemoteComputerVault(),
    );
    addTearDown(app.dispose);
    await app.start();
    expect(app.desktopServiceNames, {'desktop:office'});
    // Desktop services are not tunnels to connect to (D26).
    expect([for (final s in app.remoteServices) s.name], ['web']);

    RemoteComputer computer(String service, {int keyId = 0}) => RemoteComputer(
      id: '0' * 32,
      name: service,
      service: service,
      fingerprint: _office.fingerprint,
      relay: app.relayAddress,
      namespace: keyId,
    );
    ComputerPresence presence(RemoteComputer c) => presenceOf(
      c,
      session: const DesktopSessionView(),
      relay: app.relayState,
      relayAddress: app.relayAddress,
      credentialKeyId: app.credentialKeyId,
      desktopServices: app.desktopServiceNames,
    );
    expect(presence(computer('desktop:office')), ComputerPresence.online);
    expect(presence(computer('desktop:lab')), ComputerPresence.notSharing);
    // Remembered under temporary credential #3: not this one's to open.
    expect(
      presence(computer('desktop:office', keyId: 3)),
      ComputerPresence.otherRelay,
    );

    // Temporary credential #3, issued into a shared namespace: the relay
    // lists that namespace's services, whatever its number.
    api.identity = const RelayIdentity(role: RelayRole.temporary, keyId: 3);
    await app.checkRelay(force: true);
    expect(app.credentialKeyId, 3);
    expect(app.desktopServiceNames, {'desktop:office'});
    expect(
      presence(computer('desktop:office', keyId: 3)),
      ComputerPresence.online,
    );
    expect(presence(computer('desktop:office')), ComputerPresence.otherRelay);
  });
}
