import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
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

    // The page polls the host each second, and not after it leaves.
    final page = store.demand(DesktopDemand.page);
    await tester.pump();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 3));
    expect(api.ops, ['host_status', 'host_status', 'host_status']);
    page.dispose();
    await tester.pump();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 10));
    expect(api.calls, isEmpty);

    // Sharing polls every two seconds without a page, so a viewer's
    // arrival is seen; stopping ends it.
    await store.enableHost(
      displayId: 1,
      allowControl: true,
      ordinaryOnly: true,
    );
    api.calls.clear();
    await tester.pump(const Duration(seconds: 4));
    expect(api.ops, ['host_status', 'host_status']);
    await store.stopHost();
    api.calls.clear();
    await tester.pump(const Duration(seconds: 10));
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

  test("a JPEG session's ceiling is the host's JPEG rate, whatever the "
      'preset', () {
    const h264 = DesktopStats(
      codec: 'h264',
      preset: DesktopQuality.balanced,
      maxFps: 60,
    );
    const jpeg = DesktopStats(
      codec: 'jpeg',
      preset: DesktopQuality.smooth,
      maxFps: 120,
    );
    expect(h264.ceilingFps, 60);
    expect(jpeg.ceilingFps, DesktopStats.jpegMaxFps);
  });

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
}
