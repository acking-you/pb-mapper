import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/ffi/display_refresh.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';

const _target = DesktopTarget(invitation: 'pbd1.target', name: 'Target');

/// Connects [store], which must hold a viewer demand, and reports a live
/// session.
Future<void> _live(
  WidgetTester tester,
  DesktopStore store,
  FakeDesktopApi api, {
  DesktopTarget target = _target,
  bool controlling = false,
  int generation = 1,
}) async {
  await store.connect(target);
  api.viewerStatusValue = liveStatus(
    controlling: controlling,
    generation: generation,
  );
  await tester.pump(const Duration(milliseconds: 500));
  expect(store.session.value.phase, DesktopPhase.live);
}

/// Moves the app through valid lifecycle transitions to [state].
void _lifecycle(WidgetTester tester, List<AppLifecycleState> states) {
  for (final state in states) {
    tester.binding.handleAppLifecycleStateChanged(state);
  }
}

/// A host status read that, while [held] is set, answers only once it
/// completes, with what the host says then.
class _HeldStatusApi extends FakeDesktopApi {
  Completer<void>? held;

  @override
  Future<DesktopHostSnapshot> hostStatus() async {
    await super.hostStatus();
    await held?.future;
    return hostSnapshot;
  }
}

/// A host whose stop never returns until [wedge] completes.
class _WedgedStopApi extends FakeDesktopApi {
  final wedge = Completer<void>();

  @override
  Future<void> stopHost({bool onExit = false}) async {
    await super.stopHost(onExit: onExit);
    await wedge.future;
  }
}

void main() {
  testWidgets(
    'input is sent with the geometry generation only and dropped when not live',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api, controlling: true, generation: 3);
      final epoch = store.geometry.value!.epoch;
      expect(store.geometry.value!.generation, 3);
      expect(store.session.value.inputLive, isTrue);

      // An epoch from before a reconnect or display change is dropped.
      expect(
        store.send(
          DesktopInput.button(RemoteButton.left, true),
          geometryEpoch: epoch + 1,
        ),
        isFalse,
      );
      expect(api.argsOf('input'), isEmpty);

      // Moves coalesce, and the newest goes first in the atomic batch.
      for (var x = 1; x <= 3; x++) {
        expect(
          store.send(DesktopInput.move(x, x), geometryEpoch: epoch),
          isTrue,
        );
      }
      expect(
        store.sendSequence([
          DesktopInput.button(RemoteButton.left, true, dragStart: true),
          DesktopInput.button(RemoteButton.left, false),
        ], geometryEpoch: epoch),
        isTrue,
      );
      expect(api.inputs, [
        {'kind': 'move', 'x': 1, 'y': 1},
        {'kind': 'move', 'x': 3, 'y': 3},
        {'kind': 'button', 'button': 'left', 'down': true, 'drag_start': true},
        {'kind': 'button', 'button': 'left', 'down': false},
      ]);
      // Rust stamps frame ids and sequences: Dart sends the generation only.
      for (final batch in api.argsOf('input')) {
        expect(batch.keys, unorderedEquals(['generation', 'inputs']));
        expect(batch['generation'], 3);
      }

      // A stale picture is not live: nothing leaves.
      final sent = api.argsOf('input').length;
      api.viewerStatusValue = liveStatus(
        controlling: true,
        fresh: false,
        generation: 3,
      );
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.inputLive, isFalse);
      expect(
        store.send(DesktopInput.key(RemoteKey.a, true), geometryEpoch: epoch),
        isFalse,
      );
      expect(api.argsOf('input'), hasLength(sent));
      api.viewerStatusValue = liveStatus(controlling: true, generation: 3);
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.geometry.value!.epoch, epoch);

      // NOT_FRESH from Rust is a silent drop; control stays.
      api.failNext('input', 'NOT_FRESH');
      expect(
        store.send(DesktopInput.key(RemoteKey.a, true), geometryEpoch: epoch),
        isFalse,
      );
      expect(api.argsOf('control'), isEmpty);
      expect(store.session.value.controlling, isTrue);

      // A move refused so is kept, and goes with the next fresh picture:
      // the host always gets the last position.
      api.failNext('input', 'NOT_FRESH');
      expect(
        store.send(DesktopInput.move(7, 7), geometryEpoch: epoch),
        isFalse,
      );
      api.viewValue = const DesktopView(
        generation: 3,
        presentedFrameId: 2,
        displayWidth: 1920,
        displayHeight: 1080,
        state: 2,
        controlling: true,
        controlAllowed: true,
        fresh: true,
      );
      await tester.pump(const Duration(milliseconds: 16));
      expect(api.inputs.where((e) => e['x'] == 7), hasLength(2));
      expect(api.inputs.last, {'kind': 'move', 'x': 7, 'y': 7});

      // So is a tap's: the refusal releases what it would hold, and the next
      // fresh picture carries its move alone.
      api.failNext('input', 'NOT_FRESH');
      expect(
        store.sendSequence([
          DesktopInput.move(9, 9),
          DesktopInput.button(RemoteButton.left, true),
          DesktopInput.button(RemoteButton.left, false),
        ], geometryEpoch: epoch),
        isFalse,
      );
      expect(api.ops.last, 'release');
      api.viewValue = const DesktopView(
        generation: 3,
        presentedFrameId: 3,
        displayWidth: 1920,
        displayHeight: 1080,
        state: 2,
        controlling: true,
        controlAllowed: true,
        fresh: true,
      );
      await tester.pump(const Duration(milliseconds: 16));
      expect(api.argsOf('input').last['inputs'], [
        {'kind': 'move', 'x': 9, 'y': 9},
      ]);

      // Lost control ends it locally at once.
      api.failNext('input', 'NOT_CONTROLLING');
      expect(
        store.send(DesktopInput.key(RemoteKey.a, true), geometryEpoch: epoch),
        isFalse,
      );
      expect(api.argsOf('control'), [
        {'enabled': false},
      ]);
      expect(store.session.value.inputLive, isFalse);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets('polling runs only on demand', (tester) async {
    final api = FakeDesktopApi();
    final store = DesktopStore(api);
    int count(String op) => api.ops.where((o) => o == op).length;

    // Idle: no timer runs.
    await tester.pump(const Duration(seconds: 10));
    expect(api.calls, isEmpty);

    // The page reads the host once. With no viewer, only this app's own
    // actions and its host's change events change what it says, so the
    // page reads it again only as a 30 s backstop, and not after it leaves.
    final page = store.demand(DesktopDemand.page);
    await tester.pump();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 29));
    expect(api.calls, isEmpty);
    await tester.pump(const Duration(seconds: 1));
    expect(api.ops, ['host_status']);
    _lifecycle(tester, [AppLifecycleState.inactive, AppLifecycleState.hidden]);
    api.calls.clear();
    await tester.pump(const Duration(seconds: 60));
    expect(api.calls, isEmpty);
    _lifecycle(tester, [AppLifecycleState.inactive, AppLifecycleState.resumed]);
    await tester.pump(const Duration(seconds: 30));
    expect(count('host_status'), 1);
    page.dispose();
    await tester.pump();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 60));
    expect(api.calls, isEmpty);

    // Sharing with no viewer connected: only the backstop, without a page
    // too, since the host announces a viewer's arrival.
    await store.enableHost(
      displayId: 1,
      allowControl: true,
      ordinaryOnly: true,
    );
    api.calls.clear();
    await tester.pump(const Duration(seconds: 30));
    expect(api.ops, ['host_status']);

    // A viewer arrives: read at once on the event, and then every two
    // seconds while it is connected, for what its session shows.
    final local = api.hostSnapshot.host!;
    DesktopHostSnapshot viewed(DesktopHostViewer? viewer) =>
        DesktopHostSnapshot(
          host: DesktopHostStatus(
            state: viewer == null ? 'waiting' : 'streaming',
            controlAllowed: local.controlAllowed,
            viewer: viewer,
            display: local.display,
            service: local.service,
            invitation: local.invitation,
          ),
        );
    api.hostSnapshot = viewed(DesktopHostViewer(since: DateTime(2026, 11)));
    api.calls.clear();
    api.announceHostChange();
    await tester.pump();
    expect(api.ops, ['host_status']);
    expect(store.hostView.value.viewer, isNotNull);
    api.calls.clear();
    await tester.pump(const Duration(seconds: 4));
    expect(api.ops, ['host_status', 'host_status']);

    // It leaves: the event, then the backstop again.
    api.hostSnapshot = viewed(null);
    api.announceHostChange();
    await tester.pump();
    expect(store.hostView.value.viewer, isNull);
    api.calls.clear();
    await tester.pump(const Duration(seconds: 29));
    expect(api.calls, isEmpty);

    // Stopping ends it.
    await store.stopHost();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 60));
    expect(api.calls, isEmpty);

    // A session without a viewer on screen: its status each second, and
    // never the per-frame view.
    await store.connect(_target);
    api.viewerStatusValue = liveStatus();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 2));
    expect(count('viewer_status'), 2);
    expect(count('view'), 0);

    // A viewer on screen: status every 500 ms, and the view leaf.
    final viewer = store.demand(DesktopDemand.viewer);
    await tester.pump();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 1));
    expect(count('viewer_status'), 2);
    expect(count('view'), greaterThan(0));
    viewer.dispose();
    await tester.pump();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 2));
    expect(count('viewer_status'), 2);
    expect(count('view'), 0);

    // Disconnected: idle again.
    await store.disconnect();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 10));
    expect(api.calls, isEmpty);
    store.dispose();

    // A device that cannot host loads once, then polls nothing, on a timer
    // or on the next visit.
    final phoneApi = FakeDesktopApi()
      ..available = const DesktopCapabilities(host: false);
    final phone = DesktopStore(phoneApi);
    var phonePage = phone.demand(DesktopDemand.page);
    await tester.pump();
    expect(phone.hostView.value.loaded, isTrue);
    expect(phone.hostView.value.supported, isFalse);
    phoneApi.calls.clear();
    await tester.pump(const Duration(seconds: 10));
    phonePage.dispose();
    phonePage = phone.demand(DesktopDemand.page);
    await tester.pump(const Duration(seconds: 2));
    expect(phoneApi.calls, isEmpty);
    phonePage.dispose();
    phone.dispose();
  });

  testWidgets(
    "the host's change event reads it at once, a burst during a read once "
    "more after it, and a read a lifecycle call held up again until it is not",
    (tester) async {
      final api = _HeldStatusApi();
      final store = DesktopStore(api);
      await store.enableHost(
        displayId: 1,
        allowControl: true,
        ordinaryOnly: true,
      );
      int reads() => api.ops.where((o) => o == 'host_status').length;
      final local = api.hostSnapshot;
      final viewed = DesktopHostSnapshot(
        host: DesktopHostStatus(
          state: 'streaming',
          controlAllowed: true,
          service: local.host!.service,
          invitation: local.host!.invitation,
          viewer: DesktopHostViewer(since: DateTime(2026, 11)),
        ),
      );

      // Three events while the first read is out: one read more, not three.
      api.calls.clear();
      api.held = Completer<void>();
      api.announceHostChange();
      api.announceHostChange();
      api.announceHostChange();
      await tester.pump();
      expect(reads(), 1);
      api.hostSnapshot = viewed;
      api.held!.complete();
      api.held = null;
      await tester.pump();
      expect(reads(), 2);
      expect(store.hostView.value.viewer, isNotNull);

      // It leaves.
      api.hostSnapshot = local;
      api.announceHostChange();
      await tester.pump();
      expect(store.hostView.value.viewer, isNull);

      // One arrives while a lifecycle call holds the host: read again each
      // second until a read is not held up, then only on the viewer's clock.
      api.hostSnapshot = const DesktopHostSnapshot(busy: true);
      api.calls.clear();
      api.announceHostChange();
      await tester.pump();
      expect(reads(), 1);
      await tester.pump(const Duration(seconds: 1));
      expect(reads(), 2);
      expect(store.hostView.value.viewer, isNull);
      api.hostSnapshot = viewed;
      await tester.pump(const Duration(seconds: 1));
      expect(reads(), 3);
      expect(store.hostView.value.viewer, isNotNull);
      api.calls.clear();
      await tester.pump(const Duration(seconds: 2));
      expect(reads(), 1);

      store.dispose();
      // Nothing is read for an event after that.
      api.calls.clear();
      api.announceHostChange();
      await tester.pump();
      expect(reads(), 0);
    },
  );

  testWidgets('a display attached after a headless start can be shared', (
    tester,
  ) async {
    final api = FakeDesktopApi();
    final withDisplay = api.available;
    api.available = const DesktopCapabilities(reason: 'no_display');
    final store = DesktopStore(api);
    final page = store.demand(DesktopDemand.page);
    await tester.pump();
    expect(store.hostView.value.supported, isFalse);
    api.available = withDisplay;
    await tester.pump(const Duration(seconds: 10));
    expect(store.hostView.value.supported, isTrue);
    expect(store.hostView.value.displays, withDisplay.displays);
    // With a display, nothing is read again on a clock.
    api.calls.clear();
    await tester.pump(const Duration(seconds: 20));
    expect(api.ops, isNot(contains('capabilities')));
    page.dispose();
    store.dispose();
  });

  testWidgets(
    "a missing permission is read again on return to the app, and on a slow "
    "clock only while the page asks for it",
    (tester) async {
      const granted = DesktopHostPermissions(screenCapture: true, input: true);
      const viewOnly = DesktopHostPermissions(
        screenCapture: true,
        input: false,
      );
      DesktopCapabilities withPermissions(DesktopHostPermissions p) =>
          DesktopCapabilities(
            host: true,
            permissions: p,
            displays: const [DesktopDisplay(1, 'Display 1', 1920, 1080)],
          );
      final api = FakeDesktopApi()..available = withPermissions(viewOnly);
      final store = DesktopStore(api);
      int count(String op) => api.ops.where((o) => o == op).length;
      var page = store.demand(DesktopDemand.page);
      await tester.pump();
      expect(store.hostView.value.permissions, viewOnly);

      // Missing, with the card on screen: every 10 s, not every second.
      api.calls.clear();
      await tester.pump(const Duration(seconds: 9));
      expect(count('capabilities'), 0);
      await tester.pump(const Duration(seconds: 1));
      expect(count('capabilities'), 1);

      // Granted in System Settings: coming back reads it at once.
      api.available = withPermissions(granted);
      _lifecycle(tester, [AppLifecycleState.inactive]);
      _lifecycle(tester, [AppLifecycleState.resumed]);
      await tester.pump();
      expect(store.hostView.value.permissions, granted);

      // Complete: nothing more on a clock.
      api.calls.clear();
      await tester.pump(const Duration(seconds: 60));
      expect(count('capabilities'), 0);

      // Missing again, but the card is not on screen: nothing either.
      api.available = withPermissions(viewOnly);
      _lifecycle(tester, [AppLifecycleState.inactive]);
      _lifecycle(tester, [AppLifecycleState.resumed]);
      await tester.pump();
      page.dispose();
      await tester.pump();
      api.calls.clear();
      await tester.pump(const Duration(seconds: 60));
      expect(count('capabilities'), 0);
      page = store.demand(DesktopDemand.page);
      await tester.pump();
      page.dispose();
      store.dispose();
    },
  );

  testWidgets('a sharing or configured host always has its invitation', (
    tester,
  ) async {
    void holds(DesktopStore store) {
      void check() {
        final view = store.hostView.value;
        if (view.sharing || view.configured) {
          expect(view.invitation, isNotNull, reason: '$view');
        }
      }

      store.hostView.addListener(check);
    }

    final api = FakeDesktopApi()..trust = const DesktopTrust(supported: true);
    final store = DesktopStore(api);
    holds(store);
    final page = store.demand(DesktopDemand.page);
    await tester.pump();
    DesktopHostView view() => store.hostView.value;
    expect(view().loaded, isTrue);
    expect(view().configured, isFalse);
    expect(view().invitation, isNull);

    // Local sharing, then stopped with its grant remembered: paused.
    await store.enableHost(
      displayId: 1,
      allowControl: true,
      ordinaryOnly: true,
    );
    expect(view().sharing, isTrue);
    expect(view().invitation, FakeDesktopApi.localInvitation);
    // A poll while a lifecycle call holds the host keeps it; the next read
    // that is not busy says what became of it.
    api.hostSnapshot = const DesktopHostSnapshot(busy: true);
    await tester.pump(const Duration(seconds: 1));
    expect(view().sharing, isTrue);
    expect(view().invitation, FakeDesktopApi.localInvitation);
    await store.stopHost();
    expect(view().sharing, isFalse);
    expect(view().configured, isTrue);
    expect(view().invitation, FakeDesktopApi.localInvitation);

    // Background sharing, also once stopped.
    await store.enableHost(displayId: 1, allowControl: true);
    expect(view().sharing, isTrue);
    expect(view().background, isTrue);
    expect(view().invitation, FakeDesktopApi.backgroundInvitation);
    await store.stopHost();
    expect(view().sharing, isFalse);
    expect(view().configured, isTrue);
    expect(view().invitation, FakeDesktopApi.backgroundInvitation);
    await store.revokeTrust();
    expect(view().configured, isFalse);
    expect(view().invitation, isNull);
    page.dispose();
    store.dispose();

    // After a restart, a grant remembered for this relay is paused with its
    // invitation; one remembered under another relay is not enabled at all.
    for (final matches in [true, false]) {
      final restarted = FakeDesktopApi()
        ..trust = DesktopTrust(
          supported: true,
          matchesRelay: matches,
          saved: DesktopTrustedHost(
            relay: 'test',
            service: 'desktop:here',
            displayId: 1,
            invitation: matches ? FakeDesktopApi.localInvitation : null,
          ),
        );
      final next = DesktopStore(restarted);
      holds(next);
      await next.restoreOnStartup();
      expect(next.hostView.value.loaded, isTrue);
      expect(next.hostView.value.sharing, isFalse);
      expect(next.hostView.value.configured, matches);
      expect(
        next.hostView.value.invitation,
        matches ? FakeDesktopApi.localInvitation : isNull,
      );
      next.dispose();
    }
  });

  testWidgets(
    'a host on another relay says where, is not resumed by startup, and '
    'moves to the current relay with a new invitation',
    (tester) async {
      // The background service's host, saved before Settings changed relay.
      final api = FakeDesktopApi()
        ..trust = const DesktopTrust(supported: true)
        ..hostSnapshot = const DesktopHostSnapshot(
          background: DesktopBackground(
            configured: true,
            service: 'desktop:here',
            server: 'old-relay:7666',
            invitation: FakeDesktopApi.backgroundInvitation,
            displayId: 1,
            matchesRelay: false,
          ),
        );
      final store = DesktopStore(api);
      DesktopHostView view() => store.hostView.value;
      await store.restoreOnStartup();
      expect(view().configured, isTrue);
      expect(view().otherRelay, 'old-relay:7666');
      expect(api.argsOf('background_action'), [
        {'action': 'status'},
      ]);

      // A plain Start never moves it: the refusal (Rust's guard, which
      // resume_options' test covers) changes nothing and is not an error.
      await store.resumeHost();
      expect(api.argsOf('background_action').last, {
        'action': 'start',
        'move_relay': false,
      });
      expect(view().sharing, isFalse);
      expect(view().error, isNull);
      expect(view().otherRelay, 'old-relay:7666');

      await store.shareOnCurrentRelay();
      expect(api.argsOf('background_action').last, {
        'action': 'start',
        'move_relay': true,
      });
      expect(view().sharing, isTrue);
      expect(view().otherRelay, isNull);
      expect(view().invitation, FakeDesktopApi.movedInvitation);
      store.dispose();

      // A running local host starts again on the current relay.
      final local = FakeDesktopApi()
        ..trust = const DesktopTrust(supported: true)
        ..hostSnapshot = const DesktopHostSnapshot(
          host: DesktopHostStatus(
            controlAllowed: true,
            display: DesktopDisplay(1, 'Display 1', 1920, 1080),
            service: 'desktop:here',
            invitation: FakeDesktopApi.localInvitation,
            server: 'old-relay:7666',
            matchesRelay: false,
          ),
        );
      final next = DesktopStore(local);
      await next.restoreOnStartup();
      expect(next.hostView.value.otherRelay, 'old-relay:7666');
      await next.shareOnCurrentRelay();
      expect(local.ops, containsAllInOrder(['host_stop', 'host_start']));
      expect(local.argsOf('host_start').single, containsPair('remember', true));
      expect(next.hostView.value.sharing, isTrue);
      expect(next.hostView.value.otherRelay, isNull);
      next.dispose();
    },
  );

  testWidgets('quitting stops only this app\'s host and session, bounded', (
    tester,
  ) async {
    final api = _WedgedStopApi();
    final store = DesktopStore(api);
    final viewer = store.demand(DesktopDemand.viewer);
    await _live(tester, store, api);
    var done = false;
    unawaited(store.shutdown().then((_) => done = true));
    await tester.pump(const Duration(milliseconds: 1500));
    expect(done, isTrue);
    expect(api.argsOf('host_stop'), [
      {'on_exit': true},
    ]);
    expect(api.ops, contains('disconnect'));
    // The closed viewer is not taken for a lost one while the app exits.
    await tester.pump(const Duration(seconds: 5));
    expect(api.argsOf('connect'), hasLength(1));
    api.wedge.complete();
    viewer.dispose();
    store.dispose();
  });

  testWidgets('a second connect during an attempt connects the new target', (
    tester,
  ) async {
    const second = DesktopTarget(invitation: 'pbd1.second', name: 'Second');
    final api = FakeDesktopApi();
    final store = DesktopStore(api);
    final viewer = store.demand(DesktopDemand.viewer);
    List<Object?> connects() => [
      for (final args in api.argsOf('connect')) args['invitation'],
    ];

    // The first target fails once and waits to retry; the retry blocks on
    // the network.
    api.failNext('connect', 'IO');
    await store.connect(_target);
    expect(store.session.value.phase, DesktopPhase.reconnecting);
    final gate = Completer<void>();
    api.connectGate = gate;
    await tester.pump(const Duration(seconds: 1));
    expect(store.session.value.phase, DesktopPhase.connecting);
    expect(store.session.value.reconnectAttempt, 1);

    // The new target cancels the blocked attempt in Rust first.
    final retried = api.calls.length;
    await store.connect(second);
    expect(connects(), [_target.invitation, _target.invitation, 'pbd1.second']);
    expect(
      api.ops.sublist(retried),
      containsAllInOrder(['disconnect', 'connect']),
    );
    // The cancelled attempt's late answer neither replaces the new session
    // nor schedules a retry of the old target.
    gate.complete();
    api.viewerStatusValue = liveStatus();
    await tester.pump(const Duration(milliseconds: 500));
    final session = store.session.value;
    expect(session.target, second);
    expect(session.phase, DesktopPhase.live);
    expect(session.reconnectAttempt, 0);
    expect(session.failure, isNull);
    await tester.pump(const Duration(seconds: 30));
    expect(connects(), hasLength(3));

    viewer.dispose();
    store.dispose();
  });

  testWidgets(
    'hidden viewer pauses once, never reconnects or requests keyframes; visible resumes',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      var viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api);
      int count(String op) => api.ops.where((o) => o == op).length;

      // Hidden twice over: the app, then the viewer leaves the screen.
      _lifecycle(tester, [
        AppLifecycleState.inactive,
        AppLifecycleState.hidden,
      ]);
      viewer.dispose();
      await tester.pump();
      expect(count('pause'), 1);
      expect(store.session.value.phase, DesktopPhase.paused);
      expect(store.session.value.paused, isTrue);

      // The frozen session is closed by the host meanwhile.
      api.viewerStatusValue = stoppedStatus('transport');
      store.requestKeyframe();
      final views = count('view');
      await tester.pump(const Duration(seconds: 30));
      expect(count('connect'), 1);
      expect(count('keyframe'), 0);
      expect(count('view'), views);

      // Back on screen: resumed, and the closed session reconnects at once.
      viewer = store.demand(DesktopDemand.viewer);
      _lifecycle(tester, [
        AppLifecycleState.inactive,
        AppLifecycleState.resumed,
      ]);
      await tester.pump();
      expect(count('resume'), 1);
      expect(count('pause'), 1);
      expect(count('connect'), 2);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.phase, DesktopPhase.live);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets('quality change is live', (tester) async {
    final api = FakeDesktopApi();
    final store = DesktopStore(api);
    final viewer = store.demand(DesktopDemand.viewer);
    await _live(tester, store, api);
    api.calls.clear();
    store.setQuality(DesktopQuality.smooth);
    await tester.pump(const Duration(seconds: 2));
    expect(api.argsOf('quality'), [
      {'preset': 'smooth'},
    ]);
    expect(api.ops, isNot(contains('connect')));
    expect(api.ops, isNot(contains('disconnect')));
    expect(store.quality.value, DesktopQuality.smooth);
    expect(store.session.value.phase, DesktopPhase.live);
    viewer.dispose();
    store.dispose();
  });

  test("a chosen frame rate replaces the preset's ceiling, Quality's too; a "
      "JPEG session's ceiling is the host's JPEG rate whatever is asked", () {
    const quality = DesktopStats(
      codec: 'h265',
      preset: DesktopQuality.quality,
      maxFps: 120,
    );
    const jpeg = DesktopStats(
      codec: 'jpeg',
      preset: DesktopQuality.smooth,
      maxFps: 120,
    );
    expect(quality.ceilingFps(), 30);
    expect(quality.ceilingFps(chosen: 120), 120);
    expect(jpeg.ceilingFps(chosen: 60), DesktopStats.jpegMaxFps);
  });

  test("the probe's answer decides what is offered and what fits; the "
      'limits fit the decoder offered first; no answer is H.264 alone on '
      'Android and no native decoder elsewhere', () {
    final decoders = DesktopDecoders.fromJson({
      'h264': {'max_width': 3840, 'max_height': 2160},
      'h265': {'max_width': 1080, 'max_height': 1920},
      'display_max_hz': 120.0,
    });
    DesktopTarget at(DesktopResolution? r, [DesktopCodecPreference? codec]) =>
        _target.copyWith(resolution: (to: r), codec: codec);
    // H.265 wherever it decodes, its limits fitted inside its decoder,
    // which was probed upright.
    expect(decoders.offersH265(at(null)), isTrue);
    expect(decoders.limitsFor(null), (1920, 1080));
    expect(
      decoders.offersH265(
        at(DesktopResolution.p720, DesktopCodecPreference.h264),
      ),
      isFalse,
    );
    expect(maxFpsChoicesFor(decoders.displayMaxHz!), kMaxFpsChoices);
    expect(maxFpsChoicesFor(59.94), [30, 60]);
    expect(maxFpsChoicesFor(59), [30, 60]);

    // A phone whose AVC decoder stops at 1080p and whose HEVC one does not:
    // 1440p fits only while H.265 may be used — not once it failed here,
    // nor once the computer answered it with H.264. What fits is the
    // remote display's picture once that is known.
    final phone = DesktopDecoders.fromJson({
      'h264': {'max_width': 1920, 'max_height': 1080},
      'h265': {'max_width': 2560, 'max_height': 1440},
    });
    const auto = DesktopCodecPreference.auto,
        h264 = DesktopCodecPreference.h264;
    expect(phone.decodes(DesktopResolution.p1440, auto), isTrue);
    expect(phone.decodes(DesktopResolution.p1440, h264), isFalse);
    expect(phone.decodes(DesktopResolution.native, auto), isFalse);
    expect(
      phone.decodes(DesktopResolution.native, auto, display: (2560, 1440)),
      isTrue,
    );
    expect(phone.withoutH265().decodes(DesktopResolution.p1440, auto), isFalse);
    expect(
      phone.decodes(DesktopResolution.p1440, auto, h265Declined: true),
      isFalse,
    );

    // Garbage offers no H.265. A probe that failed offers nothing but on
    // Android, which always decodes H.264, at 1080p as before the probe;
    // there a probe without a hardware AVC decoder keeps its H.265.
    final none = DesktopDecoders.fromJson({'h265': 'yes'});
    expect(none.h265, isNull);
    // Only a runner that says it reads the window's display is asked it.
    expect(none.windowRefresh, isFalse);
    expect(
      DesktopDecoders.fromJson({'window_refresh': true}).windowRefresh,
      isTrue,
    );
    expect(decodersFromProbe(null, android: false), const DesktopDecoders());
    final failed = decodersFromProbe(null, android: true);
    expect(failed, DesktopDecoders.h264Only);
    expect(failed.decodes(DesktopResolution.p1080, auto), isTrue);
    expect(failed.decodes(DesktopResolution.p1440, auto), isFalse);
    expect(
      decodersFromProbe({
        'h264': null,
        'h265': {'max_width': 3840, 'max_height': 2160},
        'display_max_hz': 120.0,
      }, android: true),
      const DesktopDecoders(
        h264: DesktopDecoderLimit.p1080,
        h265: DesktopDecoderLimit.largest,
        displayMaxHz: 120,
      ),
    );
  });

  test('what an install would add follows the probe: the HEVC Video '
      "Extensions where Windows' GPU decodes H.265 without them, GStreamer's "
      'decoders or a hardware H.265 one on Linux; hinted only while the '
      'session goes without it, and H.265 only on Auto', () {
    const auto = DesktopCodecPreference.auto,
        h264 = DesktopCodecPreference.h264;
    const limit = {'max_width': 3840, 'max_height': 2160};
    final windows = decodersFromProbe({
      'h264': limit,
      'h265': null,
      'installable': ['hevc', 7],
    }, android: false);
    expect(windows.installable, {'hevc'});
    expect(windows.missing, isEmpty);
    expect(windows.installHint('h264', auto), DecoderInstallHint.hevcExtension);
    expect(windows.installHint('h264', h264), isNull);
    expect(windows.installHint('h265', auto), isNull);
    expect(windows.installHint(null, auto), isNull);
    expect(windows, isNot(DesktopDecoders(h264: windows.h264)));

    // Linux without GStreamer's decoders: JPEG, which they would make
    // H.264 under either preference.
    final bare = decodersFromProbe(
      {'h264': null, 'h265': null},
      android: false,
      linux: true,
    );
    expect(bare.missing, {'h264', 'h265'});
    expect(bare.installHint('jpeg', auto), DecoderInstallHint.gstreamer);
    expect(bare.installHint('jpeg', h264), DecoderInstallHint.gstreamer);
    // H.264 in software only: a GPU's driver would add H.265.
    final software = decodersFromProbe(
      {
        'h264': {'max_width': 1920, 'max_height': 1080},
        'h265': null,
      },
      android: false,
      linux: true,
    );
    expect(software.installHint('h264', auto), DecoderInstallHint.gpuDriver);
    expect(software.installHint('h264', h264), isNull);
    // JPEG from a host that encodes no H.264: nothing to install here.
    expect(software.installHint('jpeg', auto), isNull);

    // A Linux H.265 decoder that failed here is not missing, and a probe
    // that failed, or another platform's, says nothing is.
    final hardware = decodersFromProbe(
      {'h264': limit, 'h265': limit},
      android: false,
      linux: true,
    );
    expect(hardware.withoutH265().installHint('h264', auto), isNull);
    expect(
      decodersFromProbe(null, android: false, linux: true).missing,
      isEmpty,
    );
    expect(decodersFromProbe({'h264': null}, android: false).missing, isEmpty);
    expect(bare.withoutH265(), bare);
  });

  testWidgets(
    'a viewer whose runner reads its window display, as on Windows and '
    'Linux, asks for exact display rates, not whole hertz: it connects at '
    'the fastest display, tells the session its window display once '
    'connected, and again whenever the runner says the window moved, '
    'Flutter reports another rate or the app comes back; a runner silent '
    'for 200 ms leaves Flutter\'s rate',
    (tester) async {
      const channel = MethodChannel('pb_mapper/desktop_video');
      final messenger = tester.binding.defaultBinaryMessenger;
      var reads = 0;
      Future<Object?> Function() answer = () async => 59.94;
      messenger.setMockMethodCallHandler(channel, (call) {
        expect(call.method, 'refresh_hz');
        reads++;
        return answer();
      });
      addTearDown(() => messenger.setMockMethodCallHandler(channel, null));
      addTearDown(tester.view.reset);
      addTearDown(tester.view.display.reset);
      Future<void> runnerSaysChanged() async {
        await messenger.handlePlatformMessage(
          channel.name,
          channel.codec.encodeMethodCall(const MethodCall('display_changed')),
          (_) {},
        );
        await tester.pump();
      }

      final api = FakeDesktopApi()
        ..decodersValue = const DesktopDecoders(
          displayMaxHz: 119.88,
          windowRefresh: true,
        );
      final store = DesktopStore(
        api,
        displayRefresh: const DisplayRefresh(follows: true),
      );
      final viewer = store.demand(DesktopDemand.viewer);
      List<Object?> told() => [
        for (final args in api.argsOf('display_refresh')) args['hz'],
      ];

      await _live(tester, store, api);
      expect(api.argsOf('connect').single['refresh_hz'], 119.88);
      expect(told(), [59.94]);

      // Moved to the faster monitor, then back to the same rate: once.
      answer = () async => 119.88;
      await runnerSaysChanged();
      expect(told(), [59.94, 119.88]);
      await runnerSaysChanged();
      expect(told(), [59.94, 119.88]);

      // A resize leaves Flutter's rate: nothing is read.
      final before = reads;
      tester.view.physicalSize = const Size(1000, 700);
      await tester.pump();
      expect(reads, before);

      // A display Flutter reports at a new whole hertz: the runner's exact
      // rate goes.
      answer = () async => 59.94;
      tester.view.display.refreshRate = 59;
      await tester.pump();
      expect(told(), [59.94, 119.88, 59.94]);

      // Back in front after the mode changed meanwhile.
      answer = () async => 119.88;
      _lifecycle(tester, [
        AppLifecycleState.inactive,
        AppLifecycleState.resumed,
      ]);
      await tester.pump();
      expect(told(), [59.94, 119.88, 59.94, 119.88]);

      // A runner that does not answer: Flutter's rate after 200 ms.
      final never = Completer<Object?>();
      answer = () => never.future;
      tester.view.display.refreshRate = 60;
      await tester.pump(const Duration(milliseconds: 199));
      expect(told(), hasLength(4));
      await tester.pump(const Duration(milliseconds: 1));
      expect(told(), [59.94, 119.88, 59.94, 119.88, 60.0]);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    "a Mac viewer follows Flutter's rate for its window display and never "
    'asks a runner that did not say it reads it; Android and iOS connect at '
    'it and never send one',
    (tester) async {
      const channel = MethodChannel('pb_mapper/desktop_video');
      final messenger = tester.binding.defaultBinaryMessenger;
      final asked = <String>[];
      messenger.setMockMethodCallHandler(channel, (call) async {
        asked.add(call.method);
        return 30.0;
      });
      addTearDown(() => messenger.setMockMethodCallHandler(channel, null));
      addTearDown(tester.view.display.reset);
      tester.view.display.refreshRate = 60;
      final api = FakeDesktopApi()
        ..decodersValue = const DesktopDecoders(displayMaxHz: 120);
      final store = DesktopStore(
        api,
        displayRefresh: const DisplayRefresh(follows: true),
      );
      final viewer = store.demand(DesktopDemand.viewer);
      List<Object?> told() => [
        for (final args in api.argsOf('display_refresh')) args['hz'],
      ];
      await _live(tester, store, api);
      expect(api.argsOf('connect').single['refresh_hz'], 120);
      expect(told(), [60.0]);
      tester.view.display.refreshRate = 120;
      await tester.pump();
      expect(told(), [60.0, 120.0]);

      // A reconnect starts over: the connect's rate, then the window's.
      tester.view.display.refreshRate = 60;
      await tester.pump();
      api.viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 500));
      await tester.pump(const Duration(seconds: 1));
      expect(api.argsOf('connect'), hasLength(2));
      expect(api.argsOf('connect').last['refresh_hz'], 120);
      expect(told(), [60.0, 120.0, 60.0, 60.0]);
      _lifecycle(tester, [
        AppLifecycleState.inactive,
        AppLifecycleState.resumed,
      ]);
      await tester.pump();
      expect(asked, isEmpty);
      viewer.dispose();
      store.dispose();

      final phoneApi = FakeDesktopApi()
        ..decodersValue = const DesktopDecoders(displayMaxHz: 120);
      final phone = DesktopStore(
        phoneApi,
        displayRefresh: const DisplayRefresh(follows: false),
      );
      final phoneViewer = phone.demand(DesktopDemand.viewer);
      await _live(tester, phone, phoneApi);
      expect(phoneApi.argsOf('connect').single['refresh_hz'], 60);
      tester.view.display.refreshRate = 120;
      await tester.pump();
      expect(phoneApi.argsOf('display_refresh'), isEmpty);
      phoneViewer.dispose();
      phone.dispose();
    },
  );

  testWidgets(
    'a window display that changes while the connect runs reaches the '
    'session once connected: read from Flutter again, or from the runner '
    'again when it said so meanwhile',
    (tester) async {
      const channel = MethodChannel('pb_mapper/desktop_video');
      final messenger = tester.binding.defaultBinaryMessenger;
      var runnerHz = 120.0;
      messenger.setMockMethodCallHandler(channel, (call) async => runnerHz);
      addTearDown(() => messenger.setMockMethodCallHandler(channel, null));
      addTearDown(tester.view.display.reset);

      for (final runner in [false, true]) {
        tester.view.display.refreshRate = 120;
        runnerHz = 120;
        final gate = Completer<void>();
        final api = FakeDesktopApi()
          ..decodersValue = DesktopDecoders(
            displayMaxHz: 120,
            windowRefresh: runner,
          )
          ..connectGate = gate;
        final store = DesktopStore(
          api,
          displayRefresh: const DisplayRefresh(follows: true),
        );
        final viewer = store.demand(DesktopDemand.viewer);
        final connecting = store.connect(_target);
        await tester.pump();
        expect(
          api.argsOf('connect').single['refresh_hz'],
          120,
          reason: '$runner',
        );

        // The window moves to a 60 Hz display before the computer answers.
        tester.view.display.refreshRate = 60;
        runnerHz = 60;
        if (runner) {
          await messenger.handlePlatformMessage(
            channel.name,
            channel.codec.encodeMethodCall(const MethodCall('display_changed')),
            (_) {},
          );
        }
        await tester.pump();
        expect(api.argsOf('display_refresh'), isEmpty, reason: '$runner');

        gate.complete();
        await connecting;
        api.viewerStatusValue = liveStatus();
        await tester.pump(const Duration(milliseconds: 500));
        expect(store.session.value.phase, DesktopPhase.live);
        expect(
          [for (final args in api.argsOf('display_refresh')) args['hz']],
          [60.0],
          reason: '$runner',
        );
        viewer.dispose();
        store.dispose();
      }
    },
  );

  testWidgets(
    'resolution, frame rate and codec connect again at once, keeping the '
    'choice to view only; H.265 is offered where it decodes, a decoder '
    'failure on it connects again on H.264 for the rest of the run, and '
    'only a failure there ends the session',
    (tester) async {
      final api = FakeDesktopApi()
        ..codec = 'h264'
        ..hostH265 = true
        ..decodersValue = const DesktopDecoders(
          h264: DesktopDecoderLimit.largest,
          h265: DesktopDecoderLimit(1920, 1080),
        );
      final textures = FakeTextures();
      final store = DesktopStore(api, textures: textures.create);
      final viewer = store.demand(DesktopDemand.viewer);
      Map<String, Object?> last() => api.argsOf('connect').last;
      Future<void> live() async {
        api.viewerStatusValue = liveStatus();
        await tester.pump(const Duration(milliseconds: 500));
        expect(store.session.value.phase, DesktopPhase.live);
      }

      // The default, the display's own size, within the H.265 decoder.
      await _live(tester, store, api);
      expect(
        last(),
        allOf(
          containsPair('h265', true),
          containsPair('limits', (1920, 1080)),
          isNot(contains('resolution')),
          containsPair('max_fps', 0),
        ),
      );
      expect(store.picture.value?.codec, 'h265');

      // A chosen size stays on H.265, fitted inside its decoder.
      store.control(false);
      store.setResolution(DesktopResolution.native);
      await tester.pump();
      expect(
        last(),
        allOf(
          containsPair('resolution', 'native'),
          containsPair('h265', true),
          containsPair('limits', (1920, 1080)),
        ),
      );
      await live();
      expect(store.session.value.controlWanted, isFalse);
      store.setMaxFps(120);
      await tester.pump();
      expect(last(), containsPair('max_fps', 120));
      await live();

      // Back to the default; the codec preference takes H.265 away.
      store.setResolution(null);
      await tester.pump();
      expect(last(), isNot(contains('resolution')));
      await live();
      store.setCodec(DesktopCodecPreference.h264);
      await tester.pump();
      expect(last(), containsPair('h265', false));
      await live();
      store.setCodec(DesktopCodecPreference.auto);
      await tester.pump();
      expect(last(), containsPair('h265', true));
      await live();
      expect(api.argsOf('connect'), hasLength(6));

      // This device cannot decode it after all: connected again at once,
      // on H.264, with nothing for the user to see.
      api.viewerStatusValue = stoppedStatus('decoder_failed');
      await tester.pump(const Duration(milliseconds: 500));
      expect(last(), containsPair('h265', false));
      await live();
      expect(store.session.value.failure, isNull);
      expect(store.picture.value?.codec, 'h264');
      expect(store.decoders.value?.h265, isNull);
      expect(api.decodersProbed, 1);

      // Nothing to change: no reconnect. A failure on H.264 ends it.
      store.setCodec(DesktopCodecPreference.h264);
      await tester.pump();
      expect(api.argsOf('connect'), hasLength(7));
      api.viewerStatusValue = stoppedStatus('decoder_failed');
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.end, DesktopEndReason.decoderFailed);
      expect(api.argsOf('connect'), hasLength(7));

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'a computer that answers H.265 with H.264 is not offered it again, and '
    'limits only H.265 decodes here connect again at once within the H.264 '
    "decoder's, the choice kept; what fits follows the remote display",
    (tester) async {
      final api = FakeDesktopApi()
        ..codec = 'h264'
        ..decodersValue = const DesktopDecoders(
          h264: DesktopDecoderLimit(1920, 1080),
          h265: DesktopDecoderLimit(2560, 1440),
        );
      final store = DesktopStore(api, textures: FakeTextures().create);
      final viewer = store.demand(DesktopDemand.viewer);
      List<(Object?, Object?)> asked() => [
        for (final c in api.argsOf('connect')) (c['h265'], c['limits']),
      ];

      await store.connect(
        _target.copyWith(resolution: (to: DesktopResolution.p1440)),
      );
      await tester.pump();
      expect(asked(), [(true, (2560, 1440)), (false, (1920, 1080))]);
      expect(store.session.value.target?.resolution, DesktopResolution.p1440);
      // Before the remote display is known, its bounds decide.
      expect(store.decodes(DesktopResolution.p1440), isFalse);
      expect(store.decodes(DesktopResolution.p1080), isTrue);
      // A 1080p display is sent at 1080p whatever larger size is chosen.
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.phase, DesktopPhase.live);
      expect(store.decodes(DesktopResolution.p1440), isTrue);

      // A reconnect asks for what this session decodes straight away.
      api.viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 500));
      await tester.pump(const Duration(seconds: 1));
      expect(asked().last, (false, (1920, 1080)));

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'the remote resolution changes live, never a reconnect, and a computer '
    'that keeps Fit local asks for it again each time control is gained',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api, textures: FakeTextures().create);
      final viewer = store.demand(DesktopDemand.viewer);
      const modes = DisplayModeList(
        current: DisplayMode(2560, 1440),
        original: DisplayMode(2560, 1440),
        supported: [DisplayMode(2560, 1440), DisplayMode(1920, 1080)],
        fitLocal: DisplayMode(1920, 1080),
      );
      List<Object?> asked() => [
        for (final call in api.argsOf('display_mode')) call['target'],
      ];
      Future<void> report({required bool controlling}) async {
        api.viewerStatusValue = liveStatus(
          controlling: controlling,
          modes: modes,
        );
        await tester.pump(const Duration(milliseconds: 500));
      }

      await store.connect(_target.copyWith(fitLocal: true));
      await tester.pump();
      await report(controlling: false);
      expect(asked(), isEmpty);
      // Control gained: Fit local, once.
      await report(controlling: true);
      await report(controlling: true);
      expect(asked(), [
        {'fit_local': store.localDisplay!.toJson()},
      ]);

      // Another size: live, with no reconnect, and Fit local no longer
      // kept, so a later gain asks nothing.
      store.setDisplayMode(
        const DisplayModeTarget.size(DisplayMode(2560, 1440)),
      );
      expect(asked().last, {
        'size': {'width': 2560, 'height': 1440},
      });
      expect(api.argsOf('connect'), hasLength(1));
      expect(store.session.value.target?.fitLocal, isFalse);
      await report(controlling: false);
      await report(controlling: true);
      expect(asked(), hasLength(2));

      // Fit local picked in control: sent once, never again by a poll that
      // still shows the old size while the host switches.
      store.setDisplayMode(DisplayModeTarget.fitLocal(store.localDisplay!));
      await report(controlling: true);
      await report(controlling: true);
      expect(asked(), hasLength(3));

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'a decoder probe that does not answer holds the first connect only so '
    'long, with no native decoder off Android (JPEG), and none after it; '
    'its late answer is kept',
    (tester) async {
      final api = FakeDesktopApi()
        ..codec = 'h264'
        ..hostH265 = true
        ..decodersGate = Completer()
        ..decodersValue = const DesktopDecoders(
          h264: DesktopDecoderLimit.largest,
          h265: DesktopDecoderLimit.largest,
        );
      final textures = FakeTextures();
      final store = DesktopStore(api, textures: textures.create);
      final viewer = store.demand(DesktopDemand.viewer);

      unawaited(store.connect(_target));
      await tester.pump(const Duration(milliseconds: 1900));
      expect(api.argsOf('connect'), isEmpty);
      await tester.pump(const Duration(milliseconds: 100));
      expect(
        api.argsOf('connect').single,
        allOf(
          containsPair('h265', false),
          containsPair('limits', (3840, 2160)),
        ),
      );
      expect(api.argsOf('jpeg_frames'), hasLength(1));
      store.setMaxFps(60);
      await tester.pump();
      expect(api.argsOf('connect'), hasLength(2));

      api.decodersGate!.complete();
      await tester.pump();
      store.setMaxFps(0);
      await tester.pump();
      expect(api.argsOf('connect').last, containsPair('h265', true));
      expect(api.decodersProbed, 1);
      // The late answer decodes natively.
      expect(textures.created, hasLength(1));

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    "the decoder's figures are read only while shown: a listener swapped in "
    'one frame keeps them, the last one leaving stops the timings, and a '
    'read answered after its timer stopped is dropped',
    (tester) async {
      final api = FakeDesktopApi()..codec = 'h264';
      final textures = FakeTextures();
      final store = DesktopStore(
        api,
        textures: textures.create,
        now: () => tester.binding.clock.now(),
      );
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api);
      final texture = textures.created.single;
      var decoded = 0;
      Future<Map<String, Object?>> counting() async => {
        'decoded': decoded += 60,
      };
      texture.statsAnswer = counting;
      final figures = store.decoderStats;
      void shown() {}

      // The first listener reads at once; the next read gives a rate.
      figures.addListener(shown);
      await tester.pump();
      expect(figures.value, isNull);
      await tester.pump(const Duration(seconds: 1));
      expect(figures.value?.decodedPerSecond, 60);

      // A rebuild takes its listener out and puts it back: nothing restarts.
      final reads = texture.statsTimings.length;
      figures.removeListener(shown);
      figures.addListener(shown);
      await tester.pump();
      expect(figures.value?.decodedPerSecond, 60);
      expect(texture.statsTimings, hasLength(reads));

      // The last listener leaves while a read is out: the timings stop, the
      // figures go, and nothing reads meanwhile.
      final slow = Completer<Map<String, Object?>>();
      texture.statsAnswer = () => slow.future;
      await tester.pump(const Duration(seconds: 1));
      figures.removeListener(shown);
      await tester.pump();
      expect(texture.statsTimings.last, isFalse);
      expect(figures.value, isNull);
      final stopped = texture.statsTimings.length;
      await tester.pump(const Duration(seconds: 2));
      expect(texture.statsTimings, hasLength(stopped));

      // Shown again: the slow read's answer is not measured against the new
      // reads.
      texture.statsAnswer = counting;
      figures.addListener(shown);
      await tester.pump(const Duration(milliseconds: 100));
      slow.complete({'decoded': decoded + 600});
      await tester.pump();
      expect(figures.value, isNull);
      await tester.pump(const Duration(milliseconds: 900));
      expect(figures.value?.decodedPerSecond, 60);

      figures.removeListener(shown);
      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'metrics do not notify session or hostView listeners, and AppStore is never notified',
    (tester) async {
      final api = FakePbMapperApi();
      final app = AppStore(api: api, changes: const Stream.empty());
      var appNotified = 0;
      app.addListener(() => appNotified++);
      final store = app.desktop;
      final viewer = store.demand(DesktopDemand.viewer);
      await store.enableHost(
        displayId: 1,
        allowControl: true,
        ordinaryOnly: true,
      );
      await _live(tester, store, api.desktop, controlling: true);

      var sessions = 0, hosts = 0;
      final stats = <DesktopStats>[];
      store.session.addListener(() => sessions++);
      store.hostView.addListener(() => hosts++);
      void sampled() => stats.add(store.stats.value);
      store.stats.addListener(sampled);
      for (var i = 0; i < 6; i++) {
        api.desktop.viewerStatusValue = liveStatus(
          controlling: true,
          stats: DesktopStats(
            path: 'direct',
            codec: 'h264',
            presentedFps: 50.0 + i,
            rttMs: 20 + i,
          ),
        );
        await tester.pump(const Duration(milliseconds: 500));
      }
      expect(sessions, 0);
      expect(hosts, 0);
      expect(appNotified, 0);
      // At most once a second over three seconds, plus the first sample.
      expect(stats.length, inInclusiveRange(2, 4));

      store.stats.removeListener(sampled);
      viewer.dispose();
      app.dispose();
    },
  );

  testWidgets(
    'reconnect only on transport loss or 5 s without video; identity, version and denied are terminal',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final viewer = store.demand(DesktopDemand.viewer);
      int connects() => api.argsOf('connect').length;
      DesktopSessionView session() => store.session.value;
      await _live(tester, store, api);

      // A stale picture while video flows recovers in Rust, and a host
      // desktop that cannot be captured is waited for: no reconnect.
      api.viewerStatusValue = liveStatus(fresh: false, lastVideoAgeMs: 100);
      await tester.pump(const Duration(seconds: 1));
      expect(session().phase, DesktopPhase.recovering);
      api.viewerStatusValue = DesktopViewerStatus(
        state: 'capture_unavailable',
        reason: 'secure_desktop',
        generation: 1,
        display: const DesktopDisplay(1, 'Display 1', 1920, 1080),
        presentedFrameId: 1,
        lastVideoAgeMs: 10000,
      );
      await tester.pump(const Duration(seconds: 10));
      expect(session().phase, DesktopPhase.waitingForDesktop);
      expect(connects(), 1);

      // Transport loss reconnects after the first backoff step.
      api.viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 500));
      expect(session().phase, DesktopPhase.reconnecting);
      await tester.pump(const Duration(seconds: 1));
      expect(connects(), 2);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(session().reconnectAttempt, 0);

      // So do five seconds without video.
      api.viewerStatusValue = liveStatus(lastVideoAgeMs: 5000);
      await tester.pump(const Duration(milliseconds: 1500));
      expect(connects(), 3);

      // A busy host is retried, and says so while it waits.
      api.failNext('connect', 'HOST_BUSY');
      await store.connect(_target);
      expect(session().phase, DesktopPhase.reconnecting);
      expect(session().failure?.code, 'HOST_BUSY');
      await tester.pump(const Duration(seconds: 1));
      expect(connects(), 5);
      expect(session().reconnectAttempt, 1);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(session().phase, DesktopPhase.live);

      // A protocol failure is retried three times in a row, not a fourth.
      for (var stop = 1; stop <= 4; stop++) {
        api.viewerStatusValue = stoppedStatus('protocol');
        await tester.pump(const Duration(seconds: 20));
      }
      expect(connects(), 8);
      expect(session().phase, DesktopPhase.ended);
      expect(session().failure?.code, 'PROTOCOL');

      // Terminal answers end the session without a retry.
      for (final (code, end) in [
        ('VERSION_MISMATCH', DesktopEndReason.versionMismatch),
        ('ACCESS_DENIED', DesktopEndReason.denied),
        ('NOT_FOUND', DesktopEndReason.failed),
        ('VAULT_UNAVAILABLE', DesktopEndReason.failed),
      ]) {
        final before = connects();
        api.failNext('connect', code);
        await store.connect(
          const DesktopTarget(computerId: 'computer-1', name: 'Remembered'),
        );
        await tester.pump(const Duration(seconds: 30));
        expect(connects(), before + 1, reason: code);
        expect(session().phase, DesktopPhase.ended, reason: code);
        expect(session().end, end, reason: code);
        expect(session().failure?.code, code);
      }
      final before = connects();
      api.failNext('connect', 'IDENTITY_CHANGED');
      await store.connect(_target);
      store.retryNow();
      await tester.pump(const Duration(seconds: 30));
      expect(connects(), before + 1);
      expect(session().phase, DesktopPhase.identityChanged);
      expect(session().target, _target);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'texture survives reconnect (attach the new source); snapshot is best effort and time-boxed; a late texture after a busy disconnect is disposed',
    (tester) async {
      final api = FakeDesktopApi()..codec = 'h264';
      final textures = FakeTextures();
      final store = DesktopStore(api, textures: textures.create);
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api);
      final texture = textures.created.single;
      expect(texture.attached, [1]);
      expect(store.picture.value?.textureId, texture.textureId);

      // A reconnect attaches its new source to the same texture.
      api.viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 1500));
      expect(textures.created, hasLength(1));
      expect(texture.attached, [1, 2]);
      expect(store.picture.value?.textureId, texture.textureId);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.fresh, isTrue);

      // A snapshot that never answers holds the disconnect 300 ms at most.
      texture.snapshotAnswer = () => Completer<bool>().future;
      var closed = false;
      unawaited(
        store.disconnect(snapshotPath: 'thumb').then((_) => closed = true),
      );
      await tester.pump(const Duration(milliseconds: 250));
      expect(closed, isFalse);
      await tester.pump(const Duration(milliseconds: 100));
      expect(closed, isTrue);
      expect(texture.snapshots, ['thumb']);
      expect(texture.disposed, isTrue);
      expect(store.picture.value, isNull);

      // A texture created after its session was disconnected is disposed.
      final creating = Completer<void>();
      textures.gate = creating;
      unawaited(store.connect(_target));
      await tester.pump();
      await store.disconnect();
      creating.complete();
      await tester.pump();
      expect(textures.created, hasLength(2));
      expect(textures.created.last.disposed, isTrue);
      expect(store.picture.value, isNull);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    "a moving peer pointer is read at every frame, the timer's read asking "
    'for the first, and a still one on the timer only',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api);
      int reads() => api.ops.where((op) => op == 'view').length;
      double? peerX() => store.peerCursors.value.singleOrNull?.position.dx;
      void peerAt(int x, int seq) => api.viewValue = DesktopView(
        generation: 1,
        presentedFrameId: 1,
        displayWidth: 1920,
        displayHeight: 1080,
        videoWidth: 1920,
        videoHeight: 1080,
        state: 2,
        controlAllowed: true,
        fresh: true,
        peerCursorSeq: seq,
        peerCursors: [DesktopPeerCursor(color: 1, x: x, y: 540)],
      );
      // A peer pointer shows: the timer's read asks for a frame, whose read
      // finds it still and asks for none.
      peerAt(192, 1);
      await tester.binding.delayed(const Duration(milliseconds: 16));
      expect(peerX(), 0.1);
      expect(tester.binding.hasScheduledFrame, isTrue);
      var before = reads();
      await tester.pump();
      expect(reads() - before, 1);
      expect(tester.binding.hasScheduledFrame, isFalse);

      // Still: only the timer reads it, and asks for no frame.
      before = reads();
      await tester.binding.delayed(const Duration(milliseconds: 160));
      expect(reads() - before, 10);
      expect(tester.binding.hasScheduledFrame, isFalse);

      // It moves: the timer's read asks for a frame, whose read finds it
      // moved again and asks for the next, at the display's rate.
      peerAt(384, 2);
      await tester.binding.delayed(const Duration(milliseconds: 16));
      expect(peerX(), 0.2);
      expect(tester.binding.hasScheduledFrame, isTrue);
      for (var seq = 3; seq < 8; seq++) {
        peerAt(192 * seq, seq);
        before = reads();
        await tester.pump();
        expect(reads() - before, 1);
        expect(peerX(), closeTo(seq / 10, 1e-9));
        expect(tester.binding.hasScheduledFrame, isTrue);
      }
      // At rest again: the next frame's read asks for none.
      await tester.pump();
      expect(tester.binding.hasScheduledFrame, isFalse);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'while the pointer moves, the view is read as each 4 ms move gate '
    'closes, so a shape named after a move is taken within 4 ms, with no '
    'frame and before the timer',
    (tester) async {
      final api = FakeDesktopApi();
      api.cursorShapes[1] = DesktopCursorShape(
        kind: DesktopCursorKind.hand,
        width: 1,
        height: 1,
        hotX: 0,
        hotY: 0,
        rgba: Uint8List(4),
      );
      final store = DesktopStore(api);
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api, controlling: true);
      final epoch = store.geometry.value!.epoch;
      int reads() => api.ops.where((op) => op == 'view').length;
      void named(int shape) => api.viewValue = DesktopView(
        generation: 1,
        presentedFrameId: 1,
        displayWidth: 1920,
        displayHeight: 1080,
        videoWidth: 1920,
        videoHeight: 1080,
        cursorShape: shape,
        state: 2,
        controlling: true,
        controlAllowed: true,
        fresh: true,
      );
      named(0);
      await tester.pump();
      // Just after the timer's read: the next is 16 ms away.
      var before = reads();
      while (reads() == before) {
        await tester.binding.delayed(const Duration(milliseconds: 1));
      }

      expect(
        store.send(const DesktopInput.move(10, 10), geometryEpoch: epoch),
        isTrue,
      );
      named(1);
      await tester.binding.delayed(const Duration(milliseconds: 3));
      expect(store.cursor.shape, isNull);
      await tester.binding.delayed(const Duration(milliseconds: 1));
      expect(store.cursor.shape?.definition.kind, DesktopCursorKind.hand);
      expect(tester.binding.hasScheduledFrame, isFalse);

      // Moves every millisecond: one goes each 4 ms, and the view is read
      // as each gate closes, besides the timer.
      await tester.binding.delayed(const Duration(milliseconds: 12));
      before = reads();
      for (var x = 0; x < 32; x++) {
        store.send(DesktopInput.move(x, 10), geometryEpoch: epoch);
        await tester.binding.delayed(const Duration(milliseconds: 1));
      }
      expect(reads() - before, 8 + 2);
      // At rest: the last gate's read, then the timer alone.
      before = reads();
      await tester.binding.delayed(const Duration(milliseconds: 64));
      expect(reads() - before, 1 + 4);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'a picture the runner presents itself names its platform view and is '
    'greyed there; a Flutter texture names none and takes no grey',
    (tester) async {
      for (final viewType in [null, 'pb_mapper/desktop_video_view']) {
        final api = FakeDesktopApi()..codec = 'h264';
        final textures = FakeTextures()..platformViewType = viewType;
        final store = DesktopStore(api, textures: textures.create);
        final viewer = store.demand(DesktopDemand.viewer);
        await _live(tester, store, api);
        final texture = textures.created.single;
        expect(store.picture.value?.textureId, texture.textureId);
        expect(store.picture.value?.platformViewType, viewType);

        await store.greyPicture(true);
        await store.greyPicture(false);
        expect(texture.greys, viewType == null ? isEmpty : [true, false]);

        // Gone with its texture: nothing left to grey.
        await store.disconnect();
        await store.greyPicture(true);
        expect(texture.greys, hasLength(viewType == null ? 0 : 2));

        viewer.dispose();
        store.dispose();
      }
    },
  );

  testWidgets('a picture the runner can sharpen says so, and only its own '
      'texture takes a level; a Flutter texture is never asked', (
    tester,
  ) async {
    for (final viewType in [null, 'pb_mapper/desktop_video_view']) {
      final api = FakeDesktopApi()..codec = 'h264';
      final textures = FakeTextures()..platformViewType = viewType;
      final store = DesktopStore(api, textures: textures.create);
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api);
      final texture = textures.created.single;
      expect(store.picture.value?.enhances, viewType != null);

      await store.enhancePicture(texture.textureId, PictureEnhancement.high);
      // A picture another texture showed changes nothing.
      await store.enhancePicture(texture.textureId + 1, PictureEnhancement.low);
      expect(
        texture.enhancements,
        viewType == null ? isEmpty : [PictureEnhancement.high],
      );

      // Gone with its texture.
      await store.disconnect();
      await store.enhancePicture(texture.textureId, PictureEnhancement.off);
      expect(texture.enhancements, hasLength(viewType == null ? 0 : 1));

      viewer.dispose();
      store.dispose();
    }
  });

  testWidgets('a picture the runner draws on its plane says so, and the plane '
      'is placed by its texture\'s id and greyed; a texture takes neither', (
    tester,
  ) async {
    const placement = VideoPlacement(
      visible: true,
      picture: Rect.fromLTWH(0, 40, 1920, 1080),
      clip: Rect.fromLTWH(0, 40, 1600, 900),
    );
    for (final plane in [false, true]) {
      final api = FakeDesktopApi()..codec = 'h264';
      final textures = FakeTextures()..plane = plane;
      final store = DesktopStore(api, textures: textures.create);
      final viewer = store.demand(DesktopDemand.viewer);
      await _live(tester, store, api);
      final texture = textures.created.single;
      expect(store.picture.value?.textureId, texture.textureId);
      expect(store.picture.value?.plane, plane);

      await store.placePicture(texture.textureId, placement);
      // A hole another texture left places nothing.
      await store.placePicture(texture.textureId + 1, VideoPlacement.hidden);
      await store.greyPicture(true);
      expect(texture.placements, plane ? [placement] : isEmpty);
      expect(texture.greys, plane ? [true] : isEmpty);

      // Gone with its texture: nothing left to place.
      await store.disconnect();
      await store.placePicture(texture.textureId, VideoPlacement.hidden);
      expect(texture.placements, hasLength(plane ? 1 : 0));

      viewer.dispose();
      store.dispose();
    }
  });
}
