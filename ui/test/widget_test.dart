import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';

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
          selectedIndex: 1,
          onNavigationChanged: (_) {},
          title: 'Register',
          child: const SizedBox.shrink(),
        ),
      ),
    );

    expect(tester.takeException(), isNull);
    expect(find.text('Register'), findsWidgets);
    expect(find.text('Connect'), findsOneWidget);
    expect(find.text('Logs'), findsOneWidget);
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
