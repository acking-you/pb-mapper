import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';

import 'fake_pb_mapper_api.dart';

Future<AppStore> _started(
  FakePbMapperApi api, [
  StreamController<StateChange>? changes,
]) async {
  final store = AppStore(
    api: api,
    changes: (changes ?? StreamController<StateChange>.broadcast()).stream,
  );
  addTearDown(store.dispose);
  await store.start();
  return store;
}

void main() {
  test(
    'what there is to connect to leaves out what this computer shares',
    () async {
      final store = await _started(
        FakePbMapperApi(
            services: [fakeService(serviceKey: 'web')],
            serverServices: ['web', 'zeta', 'alpha'],
          )
          // Credential #3's `web` is not this computer's share of that name.
          ..namespacedServices = [(3, 'web')],
      );
      expect(
        [for (final s in store.relayServices) s.id],
        ['web', 'zeta', 'alpha', '3/web'],
      );
      expect(
        [for (final s in store.remoteServices) s.id],
        ['alpha', '3/web', 'zeta'],
      );
    },
  );

  test(
    'a connection stays in the namespace it started in after the credential changes',
    () async {
      // Started under credential #3; this computer now holds the
      // administrator key, whose own `cam` is a different service.
      final store = await _started(
        FakePbMapperApi(
          clients: [fakeClient(serviceKey: 'cam', routedNamespace: 3)],
          serverServices: ['cam'],
        )..namespacedServices = [(3, 'cam')],
      );
      final [own, theirs] = store.relayServices;
      expect(store.connectionTo(own), isNull);
      expect(store.connectionTo(theirs)?.key, 'cam');
    },
  );

  test('a name that reads like another namespace keeps its own id', () {
    // The administrator's `3/cache` and credential #3's `cache`.
    const own = RemoteService(name: '3/cache');
    const theirs = RemoteService(name: 'cache', namespace: 3);
    expect(own.id, '0/3/cache');
    expect(theirs.id, '3/cache');
  });

  test(
    'the relay is unset without an address, and offline when down',
    () async {
      final api = FakePbMapperApi(
        config: const ConfigStatus(
          serverAddress: '',
          keepAliveEnabled: true,
          msgHeaderKey: '',
        ),
      );
      final store = await _started(api);
      expect(store.relayState, RelayState.unset);

      api.relayUp = false;
      await store.saveRelay(
        address: 'relay.example:7666',
        credential: '',
        keepAlive: true,
      );
      expect(store.relayState, RelayState.offline);
      expect(store.relayServices, isEmpty);
    },
  );

  test(
    'a renamed share registers the new key before dropping the old',
    () async {
      final api = FakePbMapperApi(services: [fakeService(serviceKey: 'old')]);
      final store = await _started(api);
      final old = store.shares.single;

      final result = await store.update(
        old,
        const TunnelDraft(
          kind: TunnelKind.share,
          key: 'new',
          localAddress: '127.0.0.1:8081',
        ),
      );

      expect(result.success, isTrue);
      expect(api.calls, ['registerService(new)', 'deleteServiceConfig(old)']);
      expect(store.shares.map((t) => t.key), ['new']);
    },
  );

  test('a connection edit that is refused puts the old one back', () async {
    final api = FakePbMapperApi(clients: [fakeClient(serviceKey: 'db')]);
    final store = await _started(api);
    final old = store.connections.single;

    api.failNext = 'address in use';
    final result = await store.update(
      old,
      const TunnelDraft(
        kind: TunnelKind.connect,
        key: 'db',
        localAddress: '127.0.0.1:5432',
      ),
    );

    expect(result.success, isFalse);
    expect(api.calls, [
      'disconnectService(db)',
      'connectService(db)',
      'connectService(db)',
    ]);
    expect(store.connections.single.localAddress, '127.0.0.1:9090');
    expect(store.connections.single.health, TunnelHealth.running);
  });

  test('a share edit that is refused leaves the running share alone', () async {
    final api = FakePbMapperApi(services: [fakeService(serviceKey: 'web')]);
    final store = await _started(api);
    final old = store.shares.single;

    api.failNext = 'relay unreachable';
    final result = await store.update(
      old,
      const TunnelDraft(
        kind: TunnelKind.share,
        key: 'web',
        localAddress: '127.0.0.1:8081',
      ),
    );

    // Refused before the old share was touched, so it is not started twice.
    expect(result.success, isFalse);
    expect(api.calls, ['registerService(web)']);
    expect(store.shares.single.localAddress, '127.0.0.1:8080');
    expect(store.shares.single.health, TunnelHealth.running);
  });

  test('a change made elsewhere is read back', () async {
    final api = FakePbMapperApi();
    final changes = StreamController<StateChange>.broadcast();
    final store = await _started(api, changes);
    expect(store.shares, isEmpty);

    api.services = [fakeService(serviceKey: 'from-terminal')];
    changes.add(
      const StateChange(
        kind: StateChangeKind.services,
        origin: ChangeOrigin.cli,
        seq: 1,
      ),
    );
    // The subscription coalesces a burst over 150 ms.
    await Future<void>.delayed(const Duration(milliseconds: 250));
    expect(store.shares.map((t) => t.key), ['from-terminal']);
  });

  test('a burst that touches both lists reads both again', () async {
    final api = FakePbMapperApi();
    final changes = StreamController<StateChange>.broadcast();
    final store = await _started(api, changes);

    api.services = [fakeService(serviceKey: 'web')];
    api.clients = [fakeClient(serviceKey: 'db')];
    for (final (seq, kind) in [
      (1, StateChangeKind.services),
      (2, StateChangeKind.clients),
    ]) {
      changes.add(
        StateChange(kind: kind, origin: ChangeOrigin.internal, seq: seq),
      );
    }
    await Future<void>.delayed(const Duration(milliseconds: 250));
    expect(store.shares.map((t) => t.key), ['web']);
    expect(store.connections.map((t) => t.key), ['db']);
  });

  test(
    'a tunnel that fails after it settled is seen without an event',
    () async {
      final api = FakePbMapperApi(services: [fakeService(serviceKey: 'web')]);
      final store = AppStore(
        api: api,
        changes: const Stream.empty(),
        tunnelInterval: const Duration(milliseconds: 50),
      );
      addTearDown(store.dispose);
      await store.start();
      expect(store.shares.single.health, TunnelHealth.running);

      // What the Rust side reports once a read has made it probe again.
      api.services = [fakeService(serviceKey: 'web', status: 'failed')];
      await Future<void>.delayed(const Duration(milliseconds: 120));
      expect(store.shares.single.health, TunnelHealth.failed);
    },
  );

  test(
    'settling waits out the retry loop and reports where it landed',
    () async {
      final api = FakePbMapperApi(
        services: [fakeService(serviceKey: 'web', status: 'failed')],
      );
      final store = await _started(api);
      final settled = await store.settle(TunnelKind.share, 'web');
      expect(settled?.health, TunnelHealth.failed);
    },
  );

  group('validation', () {
    test('host:port', () {
      expect(looksLikeHostPort('127.0.0.1:8080'), isTrue);
      expect(looksLikeHostPort('example.com:7666'), isTrue);
      expect(looksLikeHostPort('[::1]:22'), isTrue);
      expect(looksLikeHostPort('example.com'), isFalse);
      expect(looksLikeHostPort(':8080'), isFalse);
      expect(looksLikeHostPort('host:70000'), isFalse);
      expect(looksLikeHostPort('::1:22'), isFalse);
    });

    test('credential', () {
      expect(looksLikeCredential(''), isTrue);
      expect(looksLikeCredential('a' * 32), isTrue);
      expect(looksLikeCredential('pbmt1_abc'), isTrue);
      expect(looksLikeCredential('short'), isFalse);
    });
  });
}
