import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';

void main() {
  testWidgets('main landing offers the guide and both roles', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: MainLandingView(
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

  testWidgets('role cards and the config step navigate', (
    WidgetTester tester,
  ) async {
    var register = 0;
    var connect = 0;
    var config = 0;

    await tester.pumpWidget(
      MaterialApp(
        home: MainLandingView(
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
}
