import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';
import 'package:pb_mapper_ui/src/views/setup_wizard_view.dart';

/// The views read their strings from Localizations, so tests need the delegates
/// installed. Pin a locale so assertions do not depend on the host language.
Widget _wrap(Widget child, {Locale locale = const Locale('en')}) {
  return MaterialApp(
    locale: locale,
    supportedLocales: LocaleController.supported,
    localizationsDelegates: const [
      AppLocalizations.delegate,
      GlobalMaterialLocalizations.delegate,
      GlobalWidgetsLocalizations.delegate,
      GlobalCupertinoLocalizations.delegate,
    ],
    home: child,
  );
}

void main() {
  testWidgets('main landing offers the guide and both roles', (tester) async {
    await tester.pumpWidget(
      _wrap(
        MainLandingView(
          onConfiguration: () {},
          onServiceRegistration: () {},
          onClientConnection: () {},
          onOperations: () {},
          onToggleTheme: () {},
        ),
      ),
    );

    expect(find.text('Quick Start'), findsOneWidget);
    expect(find.text('Register'), findsOneWidget);
    expect(find.text('Connect'), findsOneWidget);
    // Status, Config and Logs belong to the sidebar; repeating them here is
    // what made this page a second navigation surface.
    expect(find.text('Status'), findsNothing);
    expect(find.text('Logs'), findsNothing);
  });

  testWidgets('main landing asks for a star', (tester) async {
    await tester.pumpWidget(
      _wrap(
        MainLandingView(
          onConfiguration: () {},
          onServiceRegistration: () {},
          onClientConnection: () {},
          onOperations: () {},
          onToggleTheme: () {},
        ),
      ),
    );

    expect(find.text('Star on GitHub'), findsOneWidget);
  });

  testWidgets('role cards and the config step navigate', (tester) async {
    var register = 0;
    var connect = 0;
    var config = 0;

    await tester.pumpWidget(
      _wrap(
        MainLandingView(
          onConfiguration: () => config++,
          onServiceRegistration: () => register++,
          onClientConnection: () => connect++,
          onOperations: () {},
          onToggleTheme: () {},
        ),
      ),
    );

    await tester.tap(find.text('Register'));
    await tester.tap(find.text('Connect'));
    await tester.tap(find.text('Configure'));
    await tester.pump();

    expect(register, 1);
    expect(connect, 1);
    expect(config, 1);
  });

  testWidgets('landing page renders in Chinese', (tester) async {
    await tester.pumpWidget(
      _wrap(
        MainLandingView(
          onConfiguration: () {},
          onServiceRegistration: () {},
          onClientConnection: () {},
          onOperations: () {},
          onToggleTheme: () {},
        ),
        locale: const Locale('zh'),
      ),
    );

    expect(find.text('快速开始'), findsOneWidget);
    expect(find.text('注册服务'), findsOneWidget);
    expect(find.text('连接服务'), findsOneWidget);
    expect(find.text('Quick Start'), findsNothing);
  });

  testWidgets('desktop shell builds its localized chrome', (tester) async {
    // Regression: the sidebar and title bar read strings during build. Doing
    // that from the State that builds MaterialApp looked up above the
    // Localizations scope, threw, and left an empty grey window. Analysis
    // cannot catch it, so build the shell and assert nothing threw.
    tester.view.physicalSize = const Size(1600, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onOps: () {},
          onOpsTab: (_) {},
          title: 'Register',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(tester.takeException(), isNull);
    expect(find.text('Register'), findsWidgets);
  });

  testWidgets('a workspace hides the other role', (tester) async {
    tester.view.physicalSize = const Size(1600, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onOps: () {},
          onOpsTab: (_) {},
          title: 'Register',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    // The whole point of the split: registering must not put Connect, Status or
    // Logs within reach.
    expect(find.text('Connect'), findsNothing);
    expect(find.text('Status'), findsNothing);
    expect(find.text('Logs'), findsNothing);
    // Ops and the way home stay available.
    expect(find.text('Operations'), findsOneWidget);
    expect(find.text('Home'), findsOneWidget);
  });

  testWidgets('ops shows its three tabs and no roles', (tester) async {
    tester.view.physicalSize = const Size(1600, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    var picked = <OpsTab>[];
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.ops,
          opsTab: OpsTab.config,
          onHome: () {},
          onOps: () {},
          onOpsTab: picked.add,
          title: 'Configuration',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(find.text('Status'), findsOneWidget);
    expect(find.text('Config'), findsOneWidget);
    expect(find.text('Logs'), findsOneWidget);
    expect(find.text('Register'), findsNothing);
    expect(find.text('Connect'), findsNothing);

    await tester.tap(find.text('Logs'));
    expect(picked, [OpsTab.logs]);
  });

  testWidgets('home has no sidebar', (tester) async {
    tester.view.physicalSize = const Size(1600, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.home,
          opsTab: OpsTab.status,
          onHome: () {},
          onOps: () {},
          onOpsTab: (_) {},
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(find.text('Home'), findsNothing);
    expect(find.text('Operations'), findsNothing);
  });

  testWidgets('setup has no sidebar', (tester) async {
    tester.view.physicalSize = const Size(1600, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.setup,
          opsTab: OpsTab.status,
          onHome: () {},
          onOps: () {},
          onOpsTab: (_) {},
          child: const SizedBox.shrink(),
        ),
      ),
    );

    // A guided flow must not offer a way to wander off mid-setup.
    expect(find.text('Home'), findsNothing);
    expect(find.text('Operations'), findsNothing);
  });

  testWidgets('wizard opens on step 1 and can be skipped', (tester) async {
    var skipped = 0;
    await tester.pumpWidget(
      _wrap(SetupWizardView(onFinished: (_) {}, onSkip: () => skipped++)),
    );

    expect(find.text('Step 1 of 3'), findsOneWidget);
    expect(find.text('Where is your server?'), findsOneWidget);

    await tester.tap(find.text('Skip'));
    await tester.pump();
    expect(skipped, 1);
  });

  testWidgets('wizard rejects an address without a port', (tester) async {
    await tester.pumpWidget(
      _wrap(SetupWizardView(onFinished: (_) {}, onSkip: () {})),
    );

    await tester.enterText(find.byType(TextField).first, 'example.com');
    await tester.tap(find.text('Next'));
    await tester.pump();

    // Still on step 1, with the reason shown, rather than saving nonsense.
    expect(find.text('Use host:port'), findsOneWidget);
    expect(find.text('Step 1 of 3'), findsOneWidget);
  });

  testWidgets('wizard rejects a key of the wrong length', (tester) async {
    await tester.pumpWidget(
      _wrap(SetupWizardView(onFinished: (_) {}, onSkip: () {})),
    );

    final fields = find.byType(TextField);
    await tester.enterText(fields.at(0), 'example.com:7666');
    await tester.enterText(fields.at(1), 'too-short');
    await tester.tap(find.text('Next'));
    await tester.pump();

    expect(find.text('Must be exactly 32 characters'), findsOneWidget);
  });

  testWidgets('server-only mode starts at the server question', (tester) async {
    await tester.pumpWidget(
      _wrap(
        SetupWizardView(
          mode: WizardMode.serverOnly,
          onFinished: (_) {},
          onSkip: () {},
        ),
      ),
    );

    // It opens on the server question. The counter says 3, because the hub
    // afterwards lets this visit carry on into setting up a service.
    expect(find.text('Step 1 of 3'), findsOneWidget);
    expect(find.text('Where is your server?'), findsOneWidget);
  });

  testWidgets('service key field offers registered services and free input', (
    tester,
  ) async {
    final controller = TextEditingController();
    addTearDown(controller.dispose);

    await tester.pumpWidget(
      _wrap(
        Scaffold(
          body: SetupServiceKeyField(
            controller: controller,
            availableServices: const ['web', 'ssh', 'web', ''],
            labelText: 'Service Key',
            helperText: 'Choose or type a key',
          ),
        ),
      ),
    );

    expect(find.byType(DropdownMenu<String>), findsOneWidget);

    await tester.tap(find.byIcon(Icons.arrow_drop_down));
    await tester.pumpAndSettle();
    expect(find.text('ssh'), findsOneWidget);
    expect(find.text('web'), findsOneWidget);

    await tester.tap(find.text('ssh'));
    await tester.pumpAndSettle();
    expect(controller.text, 'ssh');

    await tester.enterText(find.byType(TextField), 'custom-key');
    expect(controller.text, 'custom-key');
  });

  testWidgets('service key field stays editable without suggestions', (
    tester,
  ) async {
    final controller = TextEditingController();
    addTearDown(controller.dispose);

    await tester.pumpWidget(
      _wrap(
        Scaffold(
          body: SetupServiceKeyField(
            controller: controller,
            availableServices: const [],
            labelText: 'Service Key',
            helperText: 'Type a key',
          ),
        ),
      ),
    );

    expect(find.byType(DropdownMenu<String>), findsOneWidget);

    await tester.enterText(find.byType(TextField), 'manual-key');
    expect(controller.text, 'manual-key');
  });

  testWidgets('service key field rebuilds when suggestions change', (
    tester,
  ) async {
    final controller = TextEditingController();
    addTearDown(controller.dispose);

    Future<void> pumpField(List<String> services) {
      return tester.pumpWidget(
        _wrap(
          Scaffold(
            body: SetupServiceKeyField(
              controller: controller,
              availableServices: services,
              labelText: 'Service Key',
              helperText: 'Choose or type a key',
            ),
          ),
        ),
      );
    }

    await pumpField(const ['web']);
    final firstKey = tester
        .widget<DropdownMenu<String>>(find.byType(DropdownMenu<String>))
        .key;

    await pumpField(const ['ssh']);
    final refreshedMenu = tester.widget<DropdownMenu<String>>(
      find.byType(DropdownMenu<String>),
    );

    expect(refreshedMenu.key, isNot(firstKey));
    expect(refreshedMenu.dropdownMenuEntries.map((entry) => entry.value), [
      'ssh',
    ]);
  });
  testWidgets('a wizard that set nothing up finishes at home', (tester) async {
    AppSection? landed;
    await tester.pumpWidget(
      _wrap(
        SetupWizardView(
          mode: WizardMode.serverOnly,
          onFinished: (section) => landed = section,
          onSkip: () {},
        ),
      ),
    );

    // Skipping is not setting something up, so there is no workspace to open.
    await tester.tap(find.text('Skip'));
    await tester.pump();
    expect(landed, isNull);
  });

  group('SetupState', () {
    test('the default address alone is not a configured server', () {
      expect(SetupState.isServerConfigured('localhost:7666'), isFalse);
      expect(SetupState.isServerConfigured(''), isFalse);
      expect(SetupState.isServerConfigured('   '), isFalse);
    });

    test('any other address counts as configured', () {
      expect(SetupState.isServerConfigured('example.com:7666'), isTrue);
      expect(SetupState.isServerConfigured('10.0.0.2:9000'), isTrue);
      // Same host, different port: still a deliberate choice.
      expect(SetupState.isServerConfigured('localhost:9000'), isTrue);
    });
  });

  group('LocaleController', () {
    test('cycles between the supported languages', () {
      const system = Locale('en');
      // Following the system resolves first, so the first press moves away
      // from what the user is actually seeing.
      expect(LocaleController.next(null, system).languageCode, 'zh');
      expect(
        LocaleController.next(const Locale('zh'), system).languageCode,
        'en',
      );
      expect(
        LocaleController.next(const Locale('en'), system).languageCode,
        'zh',
      );
    });

    test('labels the active language, following the system when unset', () {
      expect(LocaleController.labelFor(null, const Locale('zh')), '中');
      expect(LocaleController.labelFor(null, const Locale('en')), 'EN');
      expect(
        LocaleController.labelFor(const Locale('en'), const Locale('zh')),
        'EN',
      );
    });

    test('falls back to English for an unsupported system language', () {
      expect(LocaleController.resolve(const Locale('fr')).languageCode, 'en');
    });
  });
}
