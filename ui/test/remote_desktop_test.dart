import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'fake_desktop_api.dart';
import 'fake_remote_computer_vault.dart';

const _fingerprint = 'AB12-CD34-EF56-7890';

AddComputerResult _add(String name) => AddComputerResult(
  invitation: 'pbd1.$name-secret',
  info: DesktopInvitationInfo(
    service: 'desktop:$name',
    name: name,
    fingerprint: _fingerprint,
    format: DesktopInvitationFormat.pbd1,
  ),
  name: name,
);

void main() {
  testWidgets(
    'a new computer is remembered only after its first picture; leaving the '
    'picture releases the viewer demand and Back to picture takes it again; '
    'an identity change never touches the saved computer, nor does its new '
    'invitation unless remembered; a session left paused is ended',
    (tester) async {
      final api = FakeDesktopApi();
      final desktop = DesktopStore(api);
      final vault = FakeRemoteComputerVault();
      final computers = RemoteComputerStore(
        vault,
        // No private directory: no last pictures, which this test leaves
        // to the store's own test.
        thumbnailDir: () => Future.error(UnsupportedError('no directory')),
      );
      final viewer = ViewerHost(desktop);
      final remote = RemoteDesktop(
        desktop: desktop,
        computers: computers,
        viewer: viewer,
        prefs: ViewerPrefs(persist: false),
        relayAddress: () => 'relay.example:7666',
        credentialKeyId: () => 3,
      );
      await computers.load();
      List<String> vaultOps() => [
        for (final op in vault.ops)
          if (op != 'open' && op != 'list') op,
      ];

      // A host whose identity does not match is never remembered.
      api.failNext('connect', 'IDENTITY_CHANGED');
      await remote.connectNew(_add('office'));
      await tester.pump();
      expect(desktop.session.value.phase, DesktopPhase.identityChanged);
      expect(viewer.showing, isTrue);
      expect(vaultOps(), isEmpty);

      // Connecting is not enough; the first picture is.
      await remote.connectNew(_add('home'));
      await tester.pump();
      expect(desktop.session.value.phase, DesktopPhase.connecting);
      expect(vaultOps(), isEmpty);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(desktop.session.value.phase, DesktopPhase.live);
      await tester.pump();
      final id = desktop.session.value.target!.computerId!;
      expect(vault.calls.where((c) => c.$1 == 'remember').single.$2, {
        'id': id,
        'service': 'desktop:home',
        'fingerprint': _fingerprint,
        'relay': 'relay.example:7666',
        'namespace': 3,
        'replace_id': null,
      });
      expect(computers.byId(id)?.name, 'home');
      expect(computers.byId(id)?.lastConnectedAt, isNotNull);
      expect(remote.title, 'home');

      // Leaving the picture pauses the session; going back resumes it.
      api.calls.clear();
      remote.back();
      await tester.pump();
      expect(viewer.showing, isFalse);
      expect(api.ops, contains('pause'));
      expect(api.ops, isNot(contains('disconnect')));
      viewer.open();
      await tester.pump();
      expect(api.ops, contains('resume'));

      // Left paused, the session gives up the host's only slot in the end.
      remote.back();
      await tester.pump(kPausedSessionTimeout - const Duration(seconds: 1));
      expect(api.ops, isNot(contains('disconnect')));
      await tester.pump(const Duration(seconds: 1));
      expect(api.ops, contains('disconnect'));
      expect(desktop.session.value.phase, DesktopPhase.idle);

      // Opening it by id, the host now presents another identity: nothing
      // about the saved computer changes.
      final saved = computers.byId(id)!;
      vault.calls.clear();
      api.failNext('connect', 'IDENTITY_CHANGED');
      await remote.open(saved);
      await tester.pump();
      expect(api.argsOf('connect').last, containsPair('computer_id', id));
      expect(desktop.session.value.phase, DesktopPhase.identityChanged);
      await remote.disconnect();
      await tester.pump();
      expect(vaultOps(), isEmpty);
      expect(computers.byId(id), saved);
      expect(viewer.showing, isFalse);

      // Its new invitation, connected without remembering it, replaces
      // nothing: the session is not the saved computer's to record.
      await remote.connectNew(
        AddComputerResult(
          invitation: 'pbd1.home-reissued',
          info: const DesktopInvitationInfo(
            service: 'desktop:home',
            name: 'home',
            fingerprint: 'FFFF-0000-FFFF-0000',
            format: DesktopInvitationFormat.pbd1,
          ),
          name: 'home',
          remember: false,
          replaceId: id,
        ),
      );
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      await tester.pump();
      expect(desktop.session.value.phase, DesktopPhase.live);
      expect(desktop.session.value.target?.computerId, isNot(id));
      await remote.disconnect();
      await tester.pump();
      expect(vaultOps(), isEmpty);
      expect(computers.byId(id), saved);

      remote.dispose();
      viewer.dispose();
      computers.dispose();
      desktop.dispose();
    },
  );
}
