import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
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

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('saved computers that cannot be opened are cleared on request, with '
      'their last pictures', () async {
    final dir = await Directory.systemTemp.createTemp('pbm-remote-');
    addTearDown(() => dir.delete(recursive: true));
    final vault = FakeRemoteComputerVault()..unreadable = true;
    final store = RemoteComputerStore(vault, thumbnailDir: () async => dir);
    addTearDown(store.dispose);
    final picture = File('${dir.path}/${'a' * 32}.thumb');
    await picture.writeAsBytes(const [1]);

    await store.load();
    expect(
      store.support,
      isA<RememberUnavailable>().having((s) => s.reason, 'reason', 'unreadable'),
    );
    // Whose pictures these are cannot be told yet.
    expect(await picture.exists(), isTrue);

    await store.reset();
    expect(store.support.available, isTrue);
    expect(await picture.exists(), isFalse);
  });

  test('forgetting a computer deletes its thumbnail, and a changed identity is '
      'a conflict, not a duplicate', () async {
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

    final thumbnail = store.thumbnailFile(id)!;
    await thumbnail.writeAsBytes(const [1, 2, 3]);
    store.thumbnailWritten(id);
    expect(store.thumbnailVersion(id), 1);

    await store.forget(id);
    expect(await thumbnail.exists(), isFalse);
    expect(store.computers, isEmpty);
    expect(vault.records, isEmpty);
  });

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
