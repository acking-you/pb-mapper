import 'dart:ui';
import 'dart:async' show unawaited;
import 'dart:io' show Platform, exit;
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/views/client_connection_view.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';
import 'package:pb_mapper_ui/src/views/service_registration_view.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/views/configuration_view.dart';
import 'package:pb_mapper_ui/src/views/log_view_page.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/tray/tray_service.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:window_manager/window_manager.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (!kIsWeb && (Platform.isWindows || Platform.isLinux || Platform.isMacOS)) {
    await windowManager.ensureInitialized();

    // Hide the native title bar so the app draws one continuous surface: the
    // grey strip above the content is otherwise a separate window frame that
    // no theme can reach. macOS keeps its traffic lights, which sit inside the
    // surface; Windows and Linux get caption buttons from window_manager.
    final windowOptions = WindowOptions(
      size: const Size(1200, 800),
      minimumSize: const Size(720, 560),
      center: true,
      backgroundColor: Colors.transparent,
      title: 'pb-mapper',
      titleBarStyle: TitleBarStyle.hidden,
      windowButtonVisibility: Platform.isMacOS,
    );
    unawaited(
      windowManager.waitUntilReadyToShow(windowOptions, () async {
        await windowManager.show();
        await windowManager.focus();
      }),
    );
  }
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

class _MyAppState extends State<MyApp> with WindowListener {
  /// This `AppLifecycleListener` is responsible for the
  /// graceful shutdown of the async runtime in Rust.
  /// If you don't care about
  /// properly dropping Rust objects before shutdown,
  /// creating this listener is not necessary.
  late final AppLifecycleListener _listener;
  ThemeMode _themeMode = ThemeMode.system;
  int _currentPage =
      0; // 0 = landing, 1 = register, 2 = connect, 3 = status, 4 = config, 5 = logs
  final PbMapperApi _api = PbMapperApi();
  bool _allowExit = false;

  static const String _lastPageKey = 'last_visited_page';

  /// null follows the system language.
  Locale? _locale;

  @override
  void initState() {
    super.initState();

    // Initialize the global log manager
    LogManager().initialize();

    // Set up global navigation manager
    AppNavigationManager.setNavigationFunction(_navigateToPage);

    _listener = AppLifecycleListener(
      onExitRequested: () async {
        if (_isDesktop() && !_allowExit) {
          await TrayService.instance.hideToTray();
          return AppExitResponse.cancel;
        }
        LogManager().dispose(); // Clean up log manager
        PbMapperService().dispose();
        return AppExitResponse.exit;
      },
    );

    unawaited(_restoreLastPage());
    unawaited(_restoreLocale());
    unawaited(_initTray());
    if (!kIsWeb &&
        (Platform.isWindows || Platform.isLinux || Platform.isMacOS)) {
      windowManager.addListener(this);
    }
  }

  @override
  void dispose() {
    TrayService.instance.dispose();
    if (!kIsWeb &&
        (Platform.isWindows || Platform.isLinux || Platform.isMacOS)) {
      windowManager.removeListener(this);
    }
    _listener.dispose();
    super.dispose();
  }

  @override
  void onWindowClose() async {
    if (_isDesktop()) {
      await TrayService.instance.hideToTray();
    }
  }

  Future<void> _initTray() async {
    if (!kIsWeb &&
        (Platform.isWindows || Platform.isLinux || Platform.isMacOS)) {
      await windowManager.setPreventClose(true);
    }
    try {
      await TrayService.instance.initialize(
        statusProvider: _fetchTrayStatus,
        showApp: _showFromTray,
        quitApp: _quitFromTray,
        strings: _trayStrings(),
      );
    } catch (e) {
      debugPrint('Tray initialization failed: $e');
    }
  }

  Future<TrayStatus> _fetchTrayStatus() async {
    try {
      final serverStatus = await _api.getServerStatusDetail();
      final serviceConfigs = await _api.getServiceConfigs();
      final clientConfigs = await _api.getClientConfigs();

      final runningServices = serviceConfigs.where((config) {
        final status = config.status.toLowerCase();
        return status == 'running' || status == 'retrying';
      }).length;

      final runningClients = clientConfigs.where((config) {
        final status = config.status.toLowerCase();
        return status == 'running' || status == 'retrying';
      }).length;

      final registeredServices = serverStatus.serverAvailable
          ? serverStatus.registeredServices.length
          : runningServices;

      return TrayStatus(
        serverAvailable: serverStatus.serverAvailable,
        activeConnections: 0,
        registeredServices: registeredServices,
        connectedClients: runningClients,
      );
    } catch (_) {
      return const TrayStatus(
        serverAvailable: false,
        activeConnections: 0,
        registeredServices: 0,
        connectedClients: 0,
      );
    }
  }

  void _showFromTray() {
    TrayService.instance.showFromTray();
  }

  void _quitFromTray() {
    _allowExit = true;
    TrayService.instance.dispose();
    PbMapperService().dispose();
    LogManager().dispose();
    exit(0);
  }

  bool _isDesktop() {
    return !kIsWeb &&
        (Platform.isWindows || Platform.isLinux || Platform.isMacOS);
  }

  void _navigateToPage(int page) {
    setState(() {
      _currentPage = page;
    });
    unawaited(_persistPage(page));
  }

  /// Remember where the user was, so a daily user does not land on the guide
  /// every launch. First run has nothing stored and falls back to the landing
  /// page, which is where the guide is useful.
  Future<void> _persistPage(int page) async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setInt(_lastPageKey, page);
    } catch (_) {
      // A missing preference only costs the restored page, so ignore it.
    }
  }

  Future<void> _restoreLastPage() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final page = prefs.getInt(_lastPageKey);
      if (page != null && page > 0 && page <= 5 && mounted) {
        setState(() => _currentPage = page);
      }
    } catch (_) {
      // Fall through to the landing page.
    }
  }

  Future<void> _restoreLocale() async {
    final locale = await LocaleController.load();
    if (locale != null && mounted) {
      setState(() => _locale = locale);
    }
    await TrayService.instance.updateStrings(_trayStrings());
  }

  /// The tray is not a widget, so its strings are looked up from the active
  /// locale rather than a BuildContext.
  TrayStrings _trayStrings() {
    final active =
        _locale ??
        LocaleController.resolve(PlatformDispatcher.instance.locale);
    final l10n = lookupAppLocalizations(active);
    return TrayStrings(
      offline: l10n.trayStatusOffline,
      connections: l10n.trayStatusConnections,
      services: l10n.trayStatusServices,
      onlineIdle: l10n.trayStatusOnlineIdle,
      open: l10n.trayOpen,
      refresh: l10n.trayRefresh,
      quit: l10n.trayQuit,
    );
  }

  void toggleLanguage() {
    final system = PlatformDispatcher.instance.locale;
    final next = LocaleController.next(_locale, system);
    setState(() => _locale = next);
    unawaited(LocaleController.save(next));
    // The tray is outside the widget tree, so it needs the new strings pushed.
    unawaited(TrayService.instance.updateStrings(_trayStrings()));
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

  Widget _getCurrentPageContent(BuildContext context) {
    switch (_currentPage) {
      case 1:
        return const ServiceRegistrationView();
      case 2:
        return const ClientConnectionView();
      case 3:
        return const StatusMonitoringView();
      case 4:
        return const ConfigurationView();
      case 5:
        return const LogViewPage(showScaffold: false);
      default:
        return MainLandingView(
          onConfiguration: () => _navigateToPage(4),
          onServiceRegistration: () => _navigateToPage(1),
          onClientConnection: () => _navigateToPage(2),
          onToggleTheme: toggleTheme,
        );
    }
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'pb-mapper UI',
      locale: _locale,
      supportedLocales: LocaleController.supported,
      localizationsDelegates: const [
        AppLocalizations.delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
      ],
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
      // Pass this context down: the State's own context sits above MaterialApp
      // and so outside the Localizations scope it installs.
      home: Builder(
        builder: (context) => ResponsiveLayout.isMobile(context)
            ? _buildMobileApp(context)
            : _buildDesktopApp(context),
      ),
    );
  }

  Widget _buildMobileApp(BuildContext context) {
    if (_currentPage == 0) {
      return _getCurrentPageContent(context);
    }

    return Scaffold(
      appBar: AppBar(
        title: Text(
          _getPageTitle(context) ?? context.l10n.appTitle,
        ),
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
      body: _getCurrentPageContent(context),
      bottomNavigationBar: BottomNavigationBar(
        type: BottomNavigationBarType.fixed,
        selectedItemColor: Theme.of(context).colorScheme.primary,
        unselectedItemColor: Colors.grey,
        currentIndex: _currentPage - 1,
        onTap: (index) => _navigateToPage(index + 1),
        items: [
          BottomNavigationBarItem(
            icon: const Icon(Icons.app_registration),
            label: context.l10n.navRegister,
          ),
          BottomNavigationBarItem(
            icon: const Icon(Icons.cable),
            label: context.l10n.navConnect,
          ),
          BottomNavigationBarItem(
            icon: const Icon(Icons.monitor),
            label: context.l10n.navStatus,
          ),
          BottomNavigationBarItem(
            icon: const Icon(Icons.settings),
            label: context.l10n.navConfig,
          ),
          BottomNavigationBarItem(
            icon: const Icon(Icons.terminal),
            label: context.l10n.navLogs,
          ),
        ],
      ),
    );
  }

  Widget _buildDesktopApp(BuildContext context) {
    final l10n = context.l10n;
    return DesktopLayout(
      selectedIndex: _currentPage,
      onNavigationChanged: _navigateToPage,
      title: _getPageTitle(context) ?? l10n.appTitle,
      titleBarActions: [
        IconButton(
          icon: Text(
            LocaleController.labelFor(
              _locale,
              PlatformDispatcher.instance.locale,
            ),
            style: const TextStyle(fontSize: 12, fontWeight: FontWeight.w700),
          ),
          iconSize: 18,
          tooltip: l10n.toggleLanguage,
          onPressed: toggleLanguage,
        ),
        IconButton(
          icon: Icon(
            _themeMode == ThemeMode.dark ? Icons.light_mode : Icons.dark_mode,
          ),
          iconSize: 18,
          tooltip: l10n.toggleTheme,
          onPressed: toggleTheme,
        ),
      ],
      child: ResponsiveScaffold(body: _getCurrentPageContent(context)),
    );
  }

  String? _getPageTitle(BuildContext context) {
    final l10n = context.l10n;
    switch (_currentPage) {
      case 1:
        return l10n.pageRegister;
      case 2:
        return l10n.pageConnect;
      case 3:
        return l10n.pageStatus;
      case 4:
        return l10n.pageConfig;
      case 5:
        return l10n.pageLogs;
      default:
        return null;
    }
  }
}
