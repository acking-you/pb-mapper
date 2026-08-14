import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';
import 'package:pb_mapper_ui/src/views/client_connection_view.dart';
import 'package:pb_mapper_ui/src/views/service_registration_view.dart';

import 'fake_pb_mapper_api.dart';

/// The views, which had no tests at all until they took their API as a
/// parameter. Everything here would previously have needed the native library
/// loaded, so none of it could run.
Widget _wrap(Widget child) => MaterialApp(
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
      _wrap(
        ServiceRegistrationView(pane: WorkspacePane.list, api: api),
      ),
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
