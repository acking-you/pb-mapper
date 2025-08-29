import 'dart:ui';
import 'package:flutter/material.dart';
import 'package:rinf/rinf.dart';
import 'package:ui/src/views/client_connection_view.dart';
import 'package:ui/src/views/main_landing_view.dart';
import 'package:ui/src/views/server_management_view.dart';
import 'package:ui/src/views/service_registration_view.dart';
import 'package:ui/src/views/status_monitoring_view.dart';
import 'package:ui/src/views/configuration_view.dart';
import 'package:ui/src/common/log_manager.dart';
import 'package:ui/src/common/desktop_layout.dart';
import 'package:ui/src/common/responsive_layout.dart';
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
  int _currentPage =
      0; // 0 = landing, 1 = server, 2 = register, 3 = connect, 4 = status, 5 = config

  @override
  void initState() {
    super.initState();

    // Initialize the global log manager
    LogManager().initialize();

    _listener = AppLifecycleListener(
      onExitRequested: () async {
        LogManager().dispose(); // Clean up log manager
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

  Widget _getCurrentPageContent() {
    switch (_currentPage) {
      case 1:
        return const ServerManagementView();
      case 2:
        return const ServiceRegistrationView();
      case 3:
        return const ClientConnectionView();
      case 4:
        return const StatusMonitoringView();
      case 5:
        return const ConfigurationView();
      default:
        return MainLandingView(
          onServerManagement: () => _navigateToPage(1),
          onServiceRegistration: () => _navigateToPage(2),
          onClientConnection: () => _navigateToPage(3),
          onStatusMonitoring: () => _navigateToPage(4),
          onToggleTheme: toggleTheme,
        );
    }
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'pb-mapper UI',
      theme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
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
      home: Builder(
        builder: (context) => ResponsiveLayout.isMobile(context)
            ? _buildMobileApp()
            : _buildDesktopApp(),
      ),
    );
  }

  Widget _buildMobileApp() {
    if (_currentPage == 0) {
      return _getCurrentPageContent();
    }

    return Scaffold(
      appBar: AppBar(
        title: Text(_getPageTitle() ?? 'pb-mapper UI'),
        leading: IconButton(
          icon: const Icon(Icons.arrow_back),
          onPressed: () => _navigateToPage(0),
        ),
        actions: [
          IconButton(
            icon: Icon(
              _themeMode == ThemeMode.dark ? Icons.light_mode : Icons.dark_mode,
            ),
            onPressed: toggleTheme,
          ),
        ],
      ),
      body: _getCurrentPageContent(),
      bottomNavigationBar: _currentPage != 0
          ? BottomNavigationBar(
              type: BottomNavigationBarType.fixed,
              selectedItemColor: Theme.of(context).colorScheme.primary,
              unselectedItemColor: Colors.grey,
              currentIndex: _currentPage - 1,
              onTap: (index) => _navigateToPage(index + 1),
              items: const [
                BottomNavigationBarItem(icon: Icon(Icons.dns), label: 'Server'),
                BottomNavigationBarItem(
                  icon: Icon(Icons.app_registration),
                  label: 'Register',
                ),
                BottomNavigationBarItem(
                  icon: Icon(Icons.cable),
                  label: 'Connect',
                ),
                BottomNavigationBarItem(
                  icon: Icon(Icons.monitor),
                  label: 'Status',
                ),
              ],
            )
          : null,
    );
  }

  Widget _buildDesktopApp() {
    return DesktopLayout(
      selectedIndex: _currentPage,
      onNavigationChanged: _navigateToPage,
      child: ResponsiveScaffold(
        title: _getPageTitle(),
        body: _getCurrentPageContent(),
        actions: _currentPage == 0
            ? [
                IconButton(
                  icon: Icon(
                    _themeMode == ThemeMode.dark
                        ? Icons.light_mode
                        : Icons.dark_mode,
                  ),
                  onPressed: toggleTheme,
                ),
              ]
            : null,
      ),
    );
  }

  String? _getPageTitle() {
    switch (_currentPage) {
      case 0:
        return null;
      case 1:
        return 'Server Management';
      case 2:
        return 'Service Registration';
      case 3:
        return 'Client Connection';
      case 4:
        return 'Status Monitoring';
      case 5:
        return 'Configuration';
      default:
        return null;
    }
  }
}
