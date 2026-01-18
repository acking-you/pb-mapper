import 'dart:ui';
import 'dart:io' show Platform;
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/views/client_connection_view.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';
import 'package:pb_mapper_ui/src/views/server_management_view.dart';
import 'package:pb_mapper_ui/src/views/service_registration_view.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/views/configuration_view.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  PbMapperService().initLogging();
  await createActors();
  runApp(MyApp());
}

Future<void> createActors() async {
  // Send app directory path to Rust for mobile platforms
  if (Platform.isAndroid || Platform.isIOS) {
    try {
      final appDocumentsDir = await getApplicationDocumentsDirectory();
      await PbMapperService().setAppDirectoryPath(appDocumentsDir.path);
      if (kDebugMode) {
        print('App directory path sent to Rust: ${appDocumentsDir.path}');
      }
    } catch (e) {
      if (kDebugMode) {
        print('Failed to get app directory path: $e');
      }
      // Send empty path as fallback to ensure Rust doesn't get stuck waiting
      await PbMapperService().setAppDirectoryPath('');
      if (kDebugMode) {
        print('Sent empty path to Rust as fallback');
      }
    }
  } else {
    // For desktop platforms, send empty path to indicate no mobile directory
    await PbMapperService().setAppDirectoryPath('');
    if (kDebugMode) {
      print('Desktop platform: sent empty path to Rust');
    }
  }
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

    // Set up global navigation manager
    AppNavigationManager.setNavigationFunction(_navigateToPage);

    _listener = AppLifecycleListener(
      onExitRequested: () async {
        LogManager().dispose(); // Clean up log manager
        PbMapperService().dispose();
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
          onConfiguration: () => _navigateToPage(5),
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
          icon: const Icon(Icons.home),
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
      bottomNavigationBar: BottomNavigationBar(
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
          BottomNavigationBarItem(icon: Icon(Icons.cable), label: 'Connect'),
          BottomNavigationBarItem(icon: Icon(Icons.monitor), label: 'Status'),
          BottomNavigationBarItem(icon: Icon(Icons.settings), label: 'Config'),
        ],
      ),
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
            : [
                IconButton(
                  icon: Icon(
                    _themeMode == ThemeMode.dark
                        ? Icons.light_mode
                        : Icons.dark_mode,
                  ),
                  onPressed: toggleTheme,
                ),
              ],
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
