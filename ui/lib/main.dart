import 'dart:ui';
import 'package:flutter/material.dart';
import 'package:rinf/rinf.dart';
import 'package:ui/src/views/client_connection_page.dart';
import 'package:ui/src/views/main_landing_view.dart';
import 'package:ui/src/views/server_management_page.dart';
import 'package:ui/src/views/service_registration_page.dart';
import 'src/bindings/bindings.dart';

Future<void> main() async {
  await initializeRust(assignRustSignal);
  createActors();
  runApp(MyApp());
}

void createActors() {
  CreateActors().sendSignalToRust();
}

class MyApp extends StatefulWidget {
  const MyApp({super.key});
  @override
  State<MyApp> createState() => _MyAppState();
}

class _MyAppState extends State<MyApp> {
  /// This `AppLifecycleListener` is responsible for the
  /// graceful shutdown of the async runtime in Rust.
  /// If you don't care about
  /// properly dropping Rust objects before shutdown,
  /// creating this listener is not necessary.
  late final AppLifecycleListener _listener;
  ThemeMode _themeMode = ThemeMode.system;
  int _currentPage = 0; // 0 = landing, 1 = server, 2 = register, 3 = connect

  @override
  void initState() {
    super.initState();
    _listener = AppLifecycleListener(
      onExitRequested: () async {
        finalizeRust(); // This line shuts down the async Rust runtime.
        return AppExitResponse.exit;
      },
    );
  }

  @override
  void dispose() {
    _listener.dispose();
    super.dispose();
  }

  void _navigateToPage(int page) {
    setState(() {
      _currentPage = page;
    });
  }

  void _goBack() {
    setState(() {
      _currentPage = 0;
    });
  }

  void toggleTheme() {
    final brightness = MediaQuery.platformBrightnessOf(context);
    setState(() {
      if (_themeMode == ThemeMode.system) {
        _themeMode = brightness == Brightness.light
            ? ThemeMode.dark
            : ThemeMode.light;
      } else if (_themeMode == ThemeMode.light) {
        _themeMode = ThemeMode.dark;
      } else {
        _themeMode = ThemeMode.light;
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    Widget homeWidget;

    switch (_currentPage) {
      case 1:
        homeWidget = ServerManagementPage(
          onBack: _goBack,
          onToggleTheme: toggleTheme,
        );
        break;
      case 2:
        homeWidget = ServiceRegistrationPage(
          onBack: _goBack,
          onToggleTheme: toggleTheme,
        );
        break;
      case 3:
        homeWidget = ClientConnectionPage(
          onBack: _goBack,
          onToggleTheme: toggleTheme,
        );
        break;
      default:
        homeWidget = MainLandingView(
          onServerManagement: () => _navigateToPage(1),
          onServiceRegistration: () => _navigateToPage(2),
          onClientConnection: () => _navigateToPage(3),
          onToggleTheme: toggleTheme,
        );
    }

    return MaterialApp(
      title: 'pb-mapper UI',
      theme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
        // Improve overall text and icon visibility
        textTheme: const TextTheme(
          titleLarge: TextStyle(fontWeight: FontWeight.bold, fontSize: 20),
        ),
      ),
      darkTheme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.indigo,
          brightness: Brightness.dark,
        ),
        textTheme: const TextTheme(
          titleLarge: TextStyle(fontWeight: FontWeight.bold, fontSize: 20),
        ),
      ),
      themeMode: _themeMode,
      home: homeWidget,
    );
  }
}
