import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';

void main() {
  testWidgets('main landing exposes core client actions', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: MainLandingView(
          onConfiguration: () {},
          onServiceRegistration: () {},
          onClientConnection: () {},
          onStatusMonitoring: () {},
          onLogs: () {},
          onToggleTheme: () {},
        ),
      ),
    );

    expect(find.text('Quick Start'), findsOneWidget);
    expect(find.text('Configuration'), findsWidgets);
    expect(find.text('Register'), findsWidgets);
    expect(find.text('Connect'), findsWidgets);
    expect(find.text('Logs'), findsWidgets);
  });
}
