import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/app_destination.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';
import 'package:pb_mapper_ui/src/models/client_config.dart';
import 'package:pb_mapper_ui/src/models/service_config.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';
import 'package:pb_mapper_ui/src/views/setup_wizard_view.dart';
import 'package:pb_mapper_ui/src/widgets/app_bottom_nav.dart';
import 'package:pb_mapper_ui/src/widgets/client_card.dart';
import 'package:pb_mapper_ui/src/widgets/service_card.dart';

/// The views read their strings from Localizations, so tests need the delegates
/// installed. Pin a locale so assertions do not depend on the host language.
///
/// Pin the platform too. Widget tests report android, which the shell reads as
/// a phone and answers with bottom navigation however wide the window is — so
/// a test of the desktop shell that did not say otherwise would be testing the
/// wrong layout. The phone-layout tests drive that from width instead, and so
/// do not care what this says.
Widget _wrap(
  Widget child, {
  Locale locale = const Locale('en'),
  TargetPlatform platform = TargetPlatform.windows,
}) {
  return MaterialApp(
    theme: ThemeData(platform: platform),
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

/// A window wide enough for the side rail. The platform comes from [_wrap].
void _desktopWindow(WidgetTester tester) {
  tester.view.physicalSize = const Size(1600, 1200);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
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
    _desktopWindow(tester);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'Register',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(tester.takeException(), isNull);
    expect(find.text('Register'), findsWidgets);
  });

  testWidgets('a workspace hides the other role', (tester) async {
    _desktopWindow(tester);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'Register',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    // The point of the split: registering must not mix the connect
    // workspace's own entries or Status into the sidebar. Swapping role is
    // still one click, but it lives behind the switcher rather than sitting
    // alongside this workspace's destinations.
    expect(find.text('New Connection'), findsNothing);
    expect(find.text('Connections'), findsNothing);
    expect(find.text('Status'), findsNothing);
    // Logs are the exception, and deliberately so: why a registration did not
    // come up is a question you have without leaving the workspace.
    expect(find.text('Logs'), findsOneWidget);
    // Ops stays available. Home does not: it moved to the app mark, so the
    // sidebar no longer spends its top slot on a destination.
    expect(find.text('Operations'), findsOneWidget);
    expect(find.text('Home'), findsNothing);
    // And the bottom bar is not in the tree at all while the rail is up.
    expect(find.byType(NavigationBar), findsNothing);
  });

  testWidgets('crossing the breakpoint moves the navigation', (tester) async {
    _desktopWindow(tester);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'pb-mapper',
          child: const Text('body'),
        ),
      ),
    );

    // Wide: rail up, no bar.
    expect(find.text('Operations'), findsOneWidget);
    expect(find.byType(NavigationBar), findsNothing);

    tester.view.physicalSize = const Size(900, 700);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 120));

    // Mid-flight both are in the tree — one shrinking, one arriving. That is
    // the difference between a transition and the frame-one swap this
    // replaced, where two separate trees were picked by width.
    expect(find.text('Operations'), findsOneWidget);
    expect(find.byType(NavigationBar), findsOneWidget);

    await tester.pumpAndSettle();

    // Settled compact: bar only, and the rail is gone rather than merely
    // clipped, so its destinations are not read out twice.
    expect(find.text('Operations'), findsNothing);
    expect(find.byType(NavigationBar), findsOneWidget);
  });

  testWidgets('a short page starts at the top of the panel', (tester) async {
    _desktopWindow(tester);

    // Regression: the desktop body was wrapped in Center, which pinned a short
    // page vertically as well as horizontally. A one-row list ended up floating
    // in the middle of the window with empty space above it.
    await tester.pumpWidget(
      _wrap(
        Builder(
          builder: (context) => ResponsiveLayout.wrapWithMaxWidth(
            context: context,
            child: const SizedBox(height: 80, child: Text('row')),
          ),
        ),
      ),
    );

    expect(tester.getTopLeft(find.text('row')).dy, 0);
  });

  testWidgets('a workspace offers its form and its list', (tester) async {
    _desktopWindow(tester);

    var picked = <WorkspacePane>[];
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.connect,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: picked.add,
          itemCount: 2,
          title: 'Connect',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    // The list used to be below the form, reachable only by scrolling. Both
    // halves are sidebar destinations now, and the count says what is there.
    expect(find.text('New Connection'), findsOneWidget);
    expect(find.text('Connections (2)'), findsOneWidget);

    await tester.tap(find.text('Connections (2)'));
    expect(picked, [WorkspacePane.list]);
  });

  testWidgets('a workspace reaches the logs without leaving', (tester) async {
    _desktopWindow(tester);

    // Logs lived only under ops, so reading why a registration failed meant
    // leaving the workspace. They are a destination in both workspaces now —
    // the same view ops shows, reached without the detour.
    var picked = <WorkspacePane>[];
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: picked.add,
          title: 'pb-mapper',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    await tester.tap(find.text('Logs'));
    expect(picked, [WorkspacePane.logs]);
  });

  testWidgets('the compact bar is Material 3 and shares the rail list', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(375, 812);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    var picked = <WorkspacePane>[];
    await tester.pumpWidget(
      _wrap(
        Builder(
          builder: (context) => Scaffold(
            bottomNavigationBar: AppBottomNav(
              destinations: destinationsFor(
                context,
                section: AppSection.register,
                opsTab: OpsTab.status,
                pane: WorkspacePane.form,
                itemCount: 7,
                onPane: picked.add,
                onOpsTab: (_) {},
              ),
            ),
          ),
        ),
        platform: TargetPlatform.android,
      ),
    );

    // Material 3's bar, with its pill indicator and label, rather than the
    // Material 2 one it replaced — that drew a bare tinted icon and painted
    // everything unselected a hardcoded grey the theme could not reach.
    expect(find.byType(NavigationBar), findsOneWidget);
    expect(find.byType(BottomNavigationBar), findsNothing);

    // The destinations come from the same list the rail draws, so the count
    // reaches the bar. It used to be spelled out for the rail alone.
    expect(find.text('New Service'), findsOneWidget);
    expect(find.text('Registered (7)'), findsOneWidget);
    expect(find.text('Logs'), findsOneWidget);

    await tester.tap(find.text('Logs'));
    expect(picked, [WorkspacePane.logs]);
  });

  testWidgets('a wide touch screen still navigates from the bottom', (
    tester,
  ) async {
    // A phone on its side measures 800px and a tablet more than that, so width
    // alone cannot tell one from a small desktop window — which is how the rail
    // ended up running down the left edge of a phone. No touch platform gets a
    // rail, however wide it measures.
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'pb-mapper',
          child: const Text('body'),
        ),
        platform: TargetPlatform.android,
      ),
    );

    // Navigation is along the bottom, and the rail is not in the tree at all.
    // "Operations" is the tell: it is a rail-only entry, since it leaves for
    // another zone rather than switching what this one shows.
    expect(find.text('body'), findsOneWidget);
    expect(find.byType(NavigationBar), findsOneWidget);
    expect(find.text('Operations'), findsNothing);
  });

  testWidgets('a narrow desktop window drops the rail too', (tester) async {
    // Below the tablet breakpoint the rail had no room for its labels and
    // shrank to a 76px strip of unlabelled icons. A labelled bar along the
    // bottom is the better answer at that width.
    tester.view.physicalSize = const Size(900, 700);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'pb-mapper',
          child: const Text('body'),
        ),
      ),
    );

    expect(find.text('body'), findsOneWidget);
    expect(find.byType(NavigationBar), findsOneWidget);
    expect(find.text('Operations'), findsNothing);
  });

  testWidgets('a workspace swaps roles from its top slot', (tester) async {
    _desktopWindow(tester);

    // The slot used to say "Home", so swapping role meant leaving to the
    // landing page and picking again. The two workspaces are peers, so the
    // slot names the one you are in and swaps straight to the other.
    var swapped = <AppSection>[];
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.register,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: swapped.add,
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'pb-mapper',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    // Closed, it names the current workspace and nothing else: the other role
    // is behind the menu rather than sitting in the sidebar.
    expect(find.text('Connect'), findsNothing);

    await tester.tap(find.text('Register'));
    await tester.pumpAndSettle();

    expect(find.text('Connect'), findsOneWidget);
    await tester.tap(find.text('Connect'));
    await tester.pumpAndSettle();

    expect(swapped, [AppSection.connect]);
  });

  testWidgets('ops backs out instead of going home', (tester) async {
    _desktopWindow(tester);

    // Ops is reached from a workspace far more often than from home, so the
    // way out returns to wherever the detour started.
    var backs = 0;
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.ops,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () => backs++,
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'pb-mapper',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(find.text('Home'), findsNothing);
    expect(find.text('Back'), findsOneWidget);

    await tester.tap(find.text('Back'));
    expect(backs, 1);
  });

  testWidgets('the app mark is the way home', (tester) async {
    _desktopWindow(tester);

    // Home stopped being a sidebar entry, so it needs a place that does not
    // move between zones. The mark is the only such spot.
    var homes = 0;
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.ops,
          opsTab: OpsTab.status,
          onHome: () => homes++,
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'pb-mapper',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    await tester.tap(find.text('pb-mapper'));
    await tester.pump();
    expect(homes, 1);
  });

  testWidgets('ops shows its three tabs and no roles', (tester) async {
    _desktopWindow(tester);

    var picked = <OpsTab>[];
    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.ops,
          opsTab: OpsTab.config,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: picked.add,
          pane: WorkspacePane.form,
          onPane: (_) {},
          title: 'Configuration',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(find.text('Status'), findsOneWidget);
    expect(find.text('Config'), findsOneWidget);
    expect(find.text('New Service'), findsNothing);
    expect(find.text('New Connection'), findsNothing);
    // Logs moved into the workspaces. Keeping a copy here would be a second
    // door onto the same view, one zone away from where the question is asked.
    expect(find.text('Logs'), findsNothing);

    await tester.tap(find.text('Status'));
    expect(picked, [OpsTab.status]);
  });

  testWidgets('home has no sidebar', (tester) async {
    _desktopWindow(tester);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.home,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(find.text('Home'), findsNothing);
    expect(find.text('Operations'), findsNothing);
  });

  testWidgets('setup has no sidebar', (tester) async {
    _desktopWindow(tester);

    await tester.pumpWidget(
      _wrap(
        DesktopLayout(
          section: AppSection.setup,
          opsTab: OpsTab.status,
          onHome: () {},
          onBack: () {},
          onSwitchWorkspace: (_) {},
          onOps: () {},
          onOpsTab: (_) {},
          pane: WorkspacePane.form,
          onPane: (_) {},
          child: const SizedBox.shrink(),
        ),
      ),
    );

    // A guided flow must not offer a way to wander off mid-setup.
    expect(find.text('Home'), findsNothing);
    expect(find.text('Operations'), findsNothing);
  });

  /// A register row with every fact turned on, which is the widest it gets.
  Widget serviceRow() => ServiceCard(
    config: ServiceConfig(
      serviceKey: 'home-ubuntu',
      localAddress: '127.0.0.1:8080',
      protocol: 'TCP',
      enableEncryption: true,
      enableKeepAlive: true,
      status: ServiceStatus.running,
      updatedAt: DateTime.now().subtract(const Duration(hours: 3)),
      createdAt: DateTime.now().subtract(const Duration(days: 2)),
    ),
  );

  Widget clientRow() => ClientCard(
    config: ClientConfig(
      serviceKey: 'codex-remote',
      localAddress: '127.0.0.1:9090',
      protocol: 'TCP',
      enableKeepAlive: true,
      status: ClientStatus.running,
      updatedAt: DateTime.now().subtract(const Duration(hours: 3)),
      createdAt: DateTime.now().subtract(const Duration(days: 2)),
    ),
  );

  testWidgets('list rows fit a phone', (tester) async {
    // The row was drawn against a 1200px content panel: a fixed 88px action,
    // three icon buttons, and a row of facts that are sized to their text and
    // cannot shrink. On a phone the facts had under 100px to sit in and the row
    // overflowed by 317px — the striped bar, on the first screen a phone user
    // sees after registering anything.
    tester.view.physicalSize = const Size(375, 812);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    for (final row in [serviceRow(), clientRow()]) {
      await tester.pumpWidget(_wrap(Scaffold(body: row)));
      expect(tester.takeException(), isNull);
    }
  });

  testWidgets('a phone row puts its actions under the name', (tester) async {
    tester.view.physicalSize = const Size(375, 812);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    // In a scroll view, as the list pane renders it: the height is unbounded
    // there, and a row that only behaves under a bounded parent is not tested.
    await tester.pumpWidget(
      _wrap(Scaffold(body: SingleChildScrollView(child: serviceRow()))),
    );

    // Stacked, not merely un-overflowed: the name keeps a readable width
    // because the actions moved to their own line beneath it.
    final name = tester.getRect(find.text('home-ubuntu'));
    final action = tester.getRect(find.text('Stop'));
    expect(action.top, greaterThan(name.bottom));
    expect(name.width, greaterThan(120));
  });

  testWidgets('a wide row keeps its actions on one line', (tester) async {
    _desktopWindow(tester);

    // The stacking is driven by the width the row is given, so the desktop
    // layout has to be checked too or the fix would quietly cost a line there.
    // In a scroll view, as the list pane renders it: the height is unbounded
    // there, and a row that only behaves under a bounded parent is not tested.
    await tester.pumpWidget(
      _wrap(Scaffold(body: SingleChildScrollView(child: serviceRow()))),
    );

    final name = tester.getRect(find.text('home-ubuntu'));
    final action = tester.getRect(find.text('Stop'));
    expect(action.left, greaterThan(name.right));
    expect(action.top, lessThan(name.bottom));
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
