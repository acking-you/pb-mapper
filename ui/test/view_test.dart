import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';
import 'package:pb_mapper_ui/src/views/client_connection_view.dart';
import 'package:pb_mapper_ui/src/views/registered_services_view.dart';
import 'package:pb_mapper_ui/src/views/service_registration_view.dart';
import 'package:pb_mapper_ui/src/widgets/connection_view.dart';
import 'package:toastification/toastification.dart';

import 'fake_pb_mapper_api.dart';

/// The views, which had no tests at all until they took their API as a
/// parameter. Everything here would previously have needed the native library
/// loaded, so none of it could run.
/// Mirrors main.dart, including the toast overlay: messages render there
/// rather than into a Scaffold, so a test without it sees nothing.
Widget _wrap(Widget child) => ToastificationWrapper(
  child: MaterialApp(
    locale: const Locale('en'),
    supportedLocales: LocaleController.supported,
    localizationsDelegates: const [
      AppLocalizations.delegate,
      GlobalMaterialLocalizations.delegate,
      GlobalWidgetsLocalizations.delegate,
      GlobalCupertinoLocalizations.delegate,
    ],
    theme: ThemeData(platform: TargetPlatform.windows),
    home: Scaffold(body: child),
  ),
);

void main() {
  testWidgets('the register list renders what the API returned', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final api = FakePbMapperApi(
      services: [
        fakeService(serviceKey: 'home-ubuntu', localAddress: '127.0.0.1:8080'),
        fakeService(
          serviceKey: 'my-mac',
          localAddress: '127.0.0.1:5900',
          status: 'stopped',
        ),
      ],
    );

    await tester.pumpWidget(
      _wrap(ServiceRegistrationView(pane: WorkspacePane.list, api: api)),
    );
    await tester.pumpAndSettle();

    expect(find.text('home-ubuntu'), findsOneWidget);
    expect(find.text('my-mac'), findsOneWidget);
    expect(find.text('127.0.0.1:8080'), findsOneWidget);
    expect(find.text('127.0.0.1:5900'), findsOneWidget);
    // Status is per row, not shared: one is up, one is not.
    expect(find.text('Running'), findsOneWidget);
    expect(find.text('Stopped'), findsOneWidget);
  });

  testWidgets('an empty register list says so', (tester) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        ServiceRegistrationView(
          pane: WorkspacePane.list,
          api: FakePbMapperApi(),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // The empty state was added when the list became its own destination: it
    // used to render nothing at all, which reads as a broken page.
    expect(find.text('No services registered'), findsOneWidget);
  });

  testWidgets('the register form reports its count to the shell', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    // The sidebar entry shows this count, and it comes from the view rather
    // than a second fetch. Worth pinning: a silent break here shows up as an
    // entry that permanently reads zero.
    var reported = <int>[];
    await tester.pumpWidget(
      _wrap(
        ServiceRegistrationView(
          onCount: reported.add,
          api: FakePbMapperApi(
            services: [
              fakeService(serviceKey: 'a'),
              fakeService(serviceKey: 'b'),
              fakeService(serviceKey: 'c'),
            ],
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(reported, contains(3));
  });

  testWidgets('registering without a key is refused before any FFI call', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final api = FakePbMapperApi();
    await tester.pumpWidget(_wrap(ServiceRegistrationView(api: api)));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Register & Start'));
    await tester.pumpAndSettle();

    // The guard is the point: an empty key must not reach the Rust side.
    expect(api.calls, isEmpty);
    expect(find.text('Service key is required'), findsOneWidget);

    // Toasts close themselves on a timer, and the test binding fails a test
    // that ends with one still pending. Run it out.
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });

  testWidgets('the services page shows what the server holds, structured', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    // The status page used to render `serverMap`, which is a Rust Debug dump
    // of the whole map. These are the same facts, from the protocol's own
    // structured query — and it can say whether a connection is healthy,
    // which the dump could not.
    final api = FakePbMapperApi(
      serverServices: ['codex-remote'],
      conns: {
        'codex-remote': [
          fakeConn(connId: 1042, lastRxAge: const Duration(milliseconds: 400)),
          fakeConn(connId: 1043, healthy: false),
        ],
      },
    );

    await tester.pumpWidget(_wrap(RegisteredServicesView(api: api)));
    await tester.pumpAndSettle();

    expect(find.text('codex-remote'), findsOneWidget);
    // Connections are only asked for once the service is expanded, so nothing
    // is fetched for a list the user never opens.
    expect(find.text('#1042'), findsNothing);

    // The chevron owns the expansion; the key itself stays selectable, which
    // it cannot be if tapping it does something else.
    await tester.tap(find.byIcon(Icons.expand_more_rounded));
    await tester.pumpAndSettle();

    expect(find.text('#1042'), findsOneWidget);
    expect(find.text('#1043'), findsOneWidget);
    expect(find.text('Unhealthy'), findsOneWidget);
  });

  testWidgets('connection ids become chips, and survive an odd string', (
    tester,
  ) async {
    // The server hand-formats these as `count:… max:… list:[…]`. Parsed, they
    // are chips you can copy; unparsed, the raw line still has to show rather
    // than the section silently going blank.
    expect(ConnectionIdChips.parseIds('count:3, max:9, list:[1, 4, 9]'), [
      1,
      4,
      9,
    ]);
    expect(ConnectionIdChips.parseIds('list:[]'), isEmpty);
    expect(ConnectionIdChips.parseIds('something else entirely'), isNull);
    expect(ConnectionIdChips.parseIds('list:[1, oops]'), isNull);
  });

  testWidgets('a failing client can still be stopped', (tester) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    // `failed` means the status probe could not reach the server — the client's
    // retry loop is still running and still dialling. The row used to offer
    // Connect there, which left it retrying forever with nothing in the UI to
    // stop it. Only `stopped` means there is nothing running.
    final api = FakePbMapperApi(
      clients: [
        fakeClient(serviceKey: 'TEST', status: 'failed'),
        fakeClient(serviceKey: 'idle', status: 'stopped'),
      ],
    );

    await tester.pumpWidget(
      _wrap(ClientConnectionView(pane: WorkspacePane.list, api: api)),
    );
    await tester.pumpAndSettle();

    expect(find.text('Disconnect'), findsOneWidget);
    expect(find.text('Connect'), findsOneWidget);

    // And pressing it disconnects. The label and the action used to be decided
    // by two separate copies of the same rule, so fixing the label alone left
    // a button that said Disconnect and reconnected.
    await tester.tap(find.text('Disconnect'));
    await tester.pumpAndSettle();
    expect(api.calls, contains('disconnectService(TEST)'));
    expect(api.calls.where((c) => c.startsWith('connectService')), isEmpty);

    // The card also arms a 10s fallback to clear its operating state, and
    // the toast a 4s close. Both have to run out or the binding fails the
    // test for a pending timer.
    await tester.pump(const Duration(seconds: 12));
    await tester.pumpAndSettle();
  });

  testWidgets('a failing service can still be stopped', (tester) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final api = FakePbMapperApi(
      services: [
        fakeService(serviceKey: 'TEST', status: 'failed'),
        fakeService(
          serviceKey: 'idle',
          localAddress: '127.0.0.1:1',
          status: 'stopped',
        ),
      ],
    );

    await tester.pumpWidget(
      _wrap(ServiceRegistrationView(pane: WorkspacePane.list, api: api)),
    );
    await tester.pumpAndSettle();

    expect(find.text('Stop'), findsOneWidget);
    expect(find.text('Start'), findsOneWidget);

    await tester.tap(find.text('Stop'));
    await tester.pumpAndSettle();
    expect(api.calls, contains('unregisterService(TEST)'));
    expect(api.calls.where((c) => c.startsWith('registerService')), isEmpty);

    await tester.pump(const Duration(seconds: 12));
    await tester.pumpAndSettle();
  });

  testWidgets('the connect list renders what the API returned', (tester) async {
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final api = FakePbMapperApi(
      clients: [fakeClient(serviceKey: 'codex-remote')],
    );

    await tester.pumpWidget(
      _wrap(ClientConnectionView(pane: WorkspacePane.list, api: api)),
    );
    await tester.pumpAndSettle();

    expect(find.text('codex-remote'), findsOneWidget);
  });
}
