import 'dart:ui';
import 'dart:async' show unawaited;
import 'dart:io' show Platform, exit;
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart' show rootBundle;
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/views/client_connection_view.dart';
import 'package:pb_mapper_ui/src/views/main_landing_view.dart';
import 'package:pb_mapper_ui/src/views/service_registration_view.dart';
import 'package:pb_mapper_ui/src/views/setup_wizard_view.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/views/configuration_view.dart';
import 'package:pb_mapper_ui/src/views/log_view_page.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/app_typography.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/tray/tray_service.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:window_manager/window_manager.dart';

/// Noto Sans SC is bundled under the SIL Open Font License, which asks that
/// its notice be distributed with the font. Registering it puts the text in
/// the standard Flutter licence listing alongside every package's.
void _registerFontLicenses() {
  LicenseRegistry.addLicense(() async* {
    yield LicenseEntryWithLineBreaks(<String>[
      'NotoSansSC',
    ], await rootBundle.loadString('assets/fonts/OFL.txt'));
  });
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  _registerFontLicenses();
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
  final PbMapperApi _api = PbMapperApi();
  bool _allowExit = false;

  AppSection _section = AppSection.home;
  OpsTab _opsTab = OpsTab.status;
  WizardMode _wizardMode = WizardMode.firstRun;

  /// Which half of a workspace is showing, and how many items its list holds.
  /// Both workspaces share this: entering one always starts on its form.
  WorkspacePane _pane = WorkspacePane.form;
  int _registerCount = 0;
  int _connectCount = 0;

  static const String _lastSectionKey = 'last_section';

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

    unawaited(_restoreLastSection());
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

  bool get _isWorkspace =>
      _section == AppSection.register || _section == AppSection.connect;

  bool _isDesktop() {
    return !kIsWeb &&
        (Platform.isWindows || Platform.isLinux || Platform.isMacOS);
  }

  void _goTo(AppSection section) {
    setState(() {
      // A workspace is entered to do something, so start on the form; the list
      // is one click away in the sidebar.
      if (section != _section) _pane = WorkspacePane.form;
      _section = section;
    });
    unawaited(_persistSection(section));
  }

  /// Opens the guided server step. Used by "Configure" and by the pages that
  /// notice the server is not set yet.
  void _openServerSetup() {
    setState(() {
      _wizardMode = WizardMode.serverOnly;
      _section = AppSection.setup;
    });
  }

  void _goToOpsTab(OpsTab tab) {
    setState(() {
      _section = AppSection.ops;
      _opsTab = tab;
    });
    unawaited(_persistSection(AppSection.ops));
  }

  /// Remember which zone the user was in, so a daily user does not pick a role
  /// every launch. First run has nothing stored and opens home, where the guide
  /// is useful.
  Future<void> _persistSection(AppSection section) async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_lastSectionKey, section.name);
    } catch (_) {
      // A missing preference only costs the restored zone, so ignore it.
    }
  }

  Future<void> _restoreLastSection() async {
    // Nothing configured yet means nothing to restore: walk the user through
    // setup rather than showing a page of choices they cannot judge.
    if (await SetupState.needsSetup(_api)) {
      if (mounted) {
        setState(() {
          _wizardMode = WizardMode.firstRun;
          _section = AppSection.setup;
        });
      }
      return;
    }
    try {
      final prefs = await SharedPreferences.getInstance();
      final name = prefs.getString(_lastSectionKey);
      if (name == null || !mounted) return;
      final restored = AppSection.values.firstWhere(
        (section) => section.name == name,
        orElse: () => AppSection.home,
      );
      // Setup is never restored: reaching it is decided by the checks above.
      if (restored == AppSection.setup) return;
      setState(() => _section = restored);
    } catch (_) {
      // Fall through to home.
    }
  }

  /// The pages that still navigate by calling into the old global manager.
  void _navigateToPage(int page) {
    switch (page) {
      case 1:
        _goTo(AppSection.register);
      case 2:
        _goTo(AppSection.connect);
      case 3:
        _goToOpsTab(OpsTab.status);
      case 4:
        // These links appear when a page notices the server is unset, which is
        // a question the wizard answers better than a settings form.
        _openServerSetup();
      case 5:
        _goToOpsTab(OpsTab.logs);
      default:
        _goTo(AppSection.home);
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
    switch (_section) {
      case AppSection.register:
        return ServiceRegistrationView(
          pane: _pane,
          onCount: (count) {
            if (count != _registerCount) {
              setState(() => _registerCount = count);
            }
          },
        );
      case AppSection.connect:
        return ClientConnectionView(
          pane: _pane,
          onCount: (count) {
            if (count != _connectCount) {
              setState(() => _connectCount = count);
            }
          },
        );
      case AppSection.ops:
        switch (_opsTab) {
          case OpsTab.status:
            return const StatusMonitoringView();
          case OpsTab.config:
            return const ConfigurationView();
          case OpsTab.logs:
            return const LogViewPage(showScaffold: false);
        }
      case AppSection.setup:
        return SetupWizardView(
          mode: _wizardMode,
          // The wizard decides where to land, since it knows whether anything
          // was set up and which end of the tunnel it was.
          onFinished: _goTo,
          onSkip: () => _goTo(AppSection.home),
        );
      case AppSection.home:
        return MainLandingView(
          // "Configure" is a question, not a place: walk the user through it
          // rather than dropping them on a settings page to work out.
          onConfiguration: _openServerSetup,
          onServiceRegistration: () => _goTo(AppSection.register),
          onClientConnection: () => _goTo(AppSection.connect),
          onOperations: () => _goToOpsTab(OpsTab.status),
          onToggleTheme: toggleTheme,
        );
    }
  }

  /// The two themes differ only in brightness. The CJK fallback is where the
  /// platform differences live — see [AppTypography].
  static ThemeData _buildTheme(Brightness brightness) {
    return ThemeData(
      useMaterial3: true,
      colorScheme: ColorScheme.fromSeed(
        seedColor: Colors.indigo,
        brightness: brightness,
      ),
      fontFamilyFallback: AppTypography.uiFallback,
      textTheme: const TextTheme(
        titleLarge: TextStyle(fontWeight: FontWeight.bold, fontSize: 20),
      ),
    );
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
      theme: _buildTheme(Brightness.light),
      darkTheme: _buildTheme(Brightness.dark),
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
    // Home and setup are full-screen on mobile: neither wants a back arrow,
    // since setup has its own Skip and home is already the top level.
    if (_section == AppSection.home || _section == AppSection.setup) {
      return _getCurrentPageContent(context);
    }

    // Mobile mirrors the desktop zones: back out to home, and inside ops the
    // three tabs sit at the bottom. A workspace has nothing to switch between.
    return Scaffold(
      appBar: AppBar(
        title: Text(_getPageTitle(context) ?? context.l10n.appTitle),
        leading: IconButton(
          icon: const Icon(Icons.arrow_back),
          tooltip: context.l10n.home,
          onPressed: () => _goTo(AppSection.home),
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
      bottomNavigationBar: _isWorkspace
          // Mobile has no sidebar, so the two workspace panes live here or the
          // list would have no way in.
          ? BottomNavigationBar(
              type: BottomNavigationBarType.fixed,
              selectedItemColor: Theme.of(context).colorScheme.primary,
              unselectedItemColor: Colors.grey,
              currentIndex: _pane.index,
              onTap: (index) =>
                  setState(() => _pane = WorkspacePane.values[index]),
              items: [
                BottomNavigationBarItem(
                  icon: const Icon(Icons.add),
                  label: _section == AppSection.register
                      ? context.l10n.navNewRegister
                      : context.l10n.navNewConnect,
                ),
                BottomNavigationBarItem(
                  icon: Icon(
                    _section == AppSection.register ? Icons.dns : Icons.cable,
                  ),
                  label: _section == AppSection.register
                      ? context.l10n.navRegisteredList
                      : context.l10n.navConnectionList,
                ),
              ],
            )
          : _section == AppSection.ops
          ? BottomNavigationBar(
              type: BottomNavigationBarType.fixed,
              selectedItemColor: Theme.of(context).colorScheme.primary,
              unselectedItemColor: Colors.grey,
              currentIndex: _opsTab.index,
              onTap: (index) => _goToOpsTab(OpsTab.values[index]),
              items: [
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
            )
          : null,
    );
  }

  Widget _buildDesktopApp(BuildContext context) {
    final l10n = context.l10n;
    return DesktopLayout(
      section: _section,
      opsTab: _opsTab,
      onHome: () => _goTo(AppSection.home),
      onOps: () => _goToOpsTab(_opsTab),
      onOpsTab: _goToOpsTab,
      pane: _pane,
      onPane: (pane) => setState(() => _pane = pane),
      itemCount: _section == AppSection.register
          ? _registerCount
          : _connectCount,
      // The title bar names the window, not the page: it sits beside the app
      // mark, and the content already carries its own heading.
      title: l10n.appTitle,
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
    switch (_section) {
      case AppSection.register:
        return l10n.pageRegister;
      case AppSection.connect:
        return l10n.pageConnect;
      case AppSection.ops:
        return switch (_opsTab) {
          OpsTab.status => l10n.pageStatus,
          OpsTab.config => l10n.pageConfig,
          OpsTab.logs => l10n.pageLogs,
        };
      case AppSection.setup:
      case AppSection.home:
        return null;
    }
  }
}
