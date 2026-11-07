import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/painting.dart';
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

    await store.forget(id);
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
