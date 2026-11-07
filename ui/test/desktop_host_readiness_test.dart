import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';

class _HostProbe extends FakeDesktopApi {
  Completer<DesktopHostEnable>? enableResult;
  bool rejectRevoke = false;

  @override
  Future<void> revokeTrust() {
    if (rejectRevoke) {
      throw StateError('service management unavailable');
    }
    return super.revokeTrust();
  }

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) async {
    if (enableResult case final result?) {
      calls.add(('host_enable', {'service': service, 'display_id': displayId}));
      return result.future;
    }
    return super.enableHost(service, displayId, allowControl: allowControl);
  }
}

class _PendingHostProbe extends FakeDesktopApi {
  _PendingHostProbe() {
    trust = const DesktopTrust(
      supported: true,
      matchesRelay: true,
      saved: DesktopTrustedHost(
        relay: 'test',
        service: 'desktop:pending',
        displayId: 1,
        background: true,
        autoStart: true,
        invitation: 'pbd1.pending',
      ),
    );
  }

  final restored = Completer<String>();

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) async {
    calls.add(('host_enable', {'service': service}));
    throw StateError('helper configuration reply was lost');
  }

  @override
  Future<String?> restoreHost() async {
    calls.add(('host_restore', const {}));
    if (argsOf('host_restore').length == 1) {
      throw StateError('helper is restarting');
    }
    return restored.future;
  }
}

void main() {
  testWidgets('revoke reflects who owns the service', (tester) async {
    final api = _HostProbe();
    final store = DesktopStore(api);
    await store.enableHost(
      displayId: 1,
      allowControl: true,
      service: 'desktop:machine',
    );
    var host = store.hostView.value;
    expect(host.sharing, isTrue);
    final invitation = host.invitation;
    expect(invitation, isNotNull);

    // Service management refuses: the service still shares, and says so.
    api.rejectRevoke = true;
    await store.revokeTrust();
    host = store.hostView.value;
    expect(host.sharing, isTrue);
    expect(host.invitation, invitation);
    expect(host.error, isNotNull);
    api.rejectRevoke = false;
    await store.revokeTrust();
    host = store.hostView.value;
    expect(host.sharing, isFalse);
    expect(host.configured, isFalse);
    expect(host.invitation, isNull);
    store.dispose();
    await tester.pump();
  });

  testWidgets(
    'one-click setup resumes after IPC failure but stop rejects a late reply',
    (tester) async {
      final api = _PendingHostProbe();
      final store = DesktopStore(api);
      await store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:pending',
      );
      expect(api.argsOf('host_enable'), hasLength(1));
      expect(api.argsOf('host_restore'), isEmpty);
      await tester.pump(const Duration(seconds: 1));
      expect(api.argsOf('host_restore'), hasLength(1));
      expect(store.hostView.value.error, isNotNull);
      await tester.pump(const Duration(seconds: 2));
      expect(api.argsOf('host_restore'), hasLength(2));
      expect(api.argsOf('host_enable'), hasLength(1));

      await store.stopHost();
      api.restored.complete('obsolete invitation');
      await tester.pump();
      await tester.pump(const Duration(seconds: 20));
      expect(store.hostView.value.sharing, isFalse);
      expect(store.hostView.value.invitation, isNot('obsolete invitation'));
      expect(api.argsOf('host_restore'), hasLength(2));
      expect(api.argsOf('helper_action'), isEmpty);
      store.dispose();
      await tester.pump();
    },
  );

  testWidgets(
    'a failed read of the remembered grant is retried, not taken for one '
    'that cannot be read',
    (tester) async {
      final api = FakeDesktopApi()
        ..trust = const DesktopTrust(
          supported: true,
          matchesRelay: true,
          saved: DesktopTrustedHost(
            relay: 'test',
            service: 'desktop:here',
            displayId: 1,
            autoStart: true,
            invitation: 'pbd1.here',
          ),
        )
        ..failNext('trust_status', 'IO');
      final store = DesktopStore(api);
      await store.restoreOnStartup();
      expect(store.hostView.value.trustUnreadable, isFalse);
      expect(api.argsOf('host_restore'), isEmpty);
      await tester.pump(const Duration(seconds: 1));
      expect(api.argsOf('host_restore'), hasLength(1));
      expect(store.hostView.value.sharing, isTrue);
      store.dispose();
      await tester.pump();
    },
  );

  testWidgets(
    'a computer that cannot share restores no remembered grant and retries '
    'nothing',
    (tester) async {
      // Windows without Media Foundation, with a background grant saved
      // while it had it.
      final api = _PendingHostProbe()..available = const DesktopCapabilities();
      final store = DesktopStore(api);
      await store.restoreOnStartup();
      await tester.pump(const Duration(seconds: 60));
      expect(api.argsOf('host_restore'), isEmpty);
      expect(api.argsOf('host_enable'), isEmpty);
      expect(store.hostView.value.sharing, isFalse);
      store.dispose();
      await tester.pump();
    },
  );

  testWidgets(
    'restart waits for a successful handoff and cancellation keeps UI alive',
    (tester) async {
      final api = _HostProbe()..enableResult = Completer<DesktopHostEnable>();
      var exits = 0;
      final store = DesktopStore(api, onRestartReady: () => exits++);
      var enabling = store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:machine',
      );
      // A second enable while the first runs is ignored.
      await store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:machine',
      );
      expect(api.argsOf('host_enable'), hasLength(1));
      expect(exits, 0);
      api.enableResult!.completeError(StateError('consent cancelled'));
      await enabling;
      expect(exits, 0);
      expect(store.hostView.value.restarting, isFalse);
      expect(store.hostView.value.error, isNotNull);
      await tester.pump(const Duration(seconds: 20));
      expect(api.argsOf('host_restore'), isEmpty);
      expect(api.argsOf('host_enable'), hasLength(1));

      api.enableResult = Completer<DesktopHostEnable>();
      enabling = store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:machine',
      );
      expect(exits, 0);
      api.enableResult!.complete(const DesktopHostEnable(restart: true));
      await enabling;
      expect(exits, 1);
      expect(store.hostView.value.restarting, isTrue);
      expect(store.hostView.value.error, isNull);
      store.dispose();
    },
  );
}
