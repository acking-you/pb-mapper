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
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/app_typography.dart';
import 'package:pb_mapper_ui/src/common/desktop_layout.dart';
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

  /// Where the user has been, oldest first, so the sidebar's back entry can
  /// return them to it. Only zone changes are recorded: switching a tab or a
  /// pane stays on the same screen, and burying the way out under a stack of
  /// tab changes is what a back button is expected not to do.
  final List<_Location> _history = [];

  /// Deep enough for any real path through five zones, shallow enough that a
  /// user clicking between zones all afternoon cannot grow it without bound.
  static const int _maxHistory = 16;

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

  /// Records where the user is so [_goBack] can return them to it.
  ///
  /// Home is the root rather than a stop along the way: arriving there means
  /// the trail behind is spent, and keeping it would let back walk into zones
  /// the user had already left.
  void _pushHistory() {
    if (_section == AppSection.home || _section == AppSection.setup) return;
    final here = _Location(_section, _opsTab, _pane);
    if (_history.isNotEmpty && _history.last.section == here.section) {
      _history.removeLast();
    }
    _history.add(here);
    if (_history.length > _maxHistory) _history.removeAt(0);
  }

  void _goTo(AppSection section) {
    if (section == _section) return;
    if (section == AppSection.home) {
      _history.clear();
    } else {
      _pushHistory();
    }
    setState(() {
      // A workspace is entered to do something, so start on the form; the list
      // is one click away in the sidebar.
      _pane = WorkspacePane.form;
      _section = section;
    });
    unawaited(_persistSection(section));
  }

  /// Returns to the previous zone, or home once the trail runs out.
  void _goBack() {
    if (_history.isEmpty) {
      _goTo(AppSection.home);
      return;
    }
    final previous = _history.removeLast();
    setState(() {
      _section = previous.section;
      _opsTab = previous.opsTab;
      _pane = previous.pane;
    });
    unawaited(_persistSection(previous.section));
  }

  /// Swaps between registering and connecting.
  ///
  /// A swap, not a trip: the two are peers, so this replaces the current
  /// workspace rather than stacking on top of it. Backing out of ops afterwards
  /// lands on the workspace the user actually left, not on the one they
  /// happened to open first.
  void _switchWorkspace(AppSection section) {
    if (section == _section || !section.isWorkspace) return;
    setState(() {
      _pane = WorkspacePane.form;
      _section = section;
    });
    unawaited(_persistSection(section));
  }

  /// Opens the guided server step. Used by "Configure" and by the pages that
  /// notice the server is not set yet.
  void _openServerSetup() {
    _pushHistory();
    setState(() {
      _wizardMode = WizardMode.serverOnly;
      _section = AppSection.setup;
    });
  }

  void _goToOpsTab(OpsTab tab) {
    // Changing tab inside ops is not a trip: only the way in is recorded, so
    // backing out returns to the workspace rather than to the previous tab.
    if (_section != AppSection.ops) _pushHistory();
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
        // Logs left ops for the workspaces. Show the ones for the workspace
        // the user is already in, and pick register if they are in neither.
        if (!_isWorkspace) _goTo(AppSection.register);
        setState(() => _pane = WorkspacePane.logs);
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
      // Every colour in the tree is lerped between the two themes over this,
      // so switching does not repaint the window in one frame. The stock 200ms
      // is quick enough to read as a flash on a full-window surface change.
      themeAnimationDuration: const Duration(milliseconds: 350),
      themeAnimationCurve: Curves.easeInOutCubic,
      // Pass this context down: the State's own context sits above MaterialApp
      // and so outside the Localizations scope it installs.
      // One shell for both layouts. They used to be two separate trees picked
      // by width, which is why crossing the breakpoint swapped one for the
      // other in a single frame instead of moving between them.
      home: Builder(builder: _buildApp),
    );
  }

  /// Language and theme. Both layouts show them, so they are built once.
  List<Widget> _chromeActions(BuildContext context) {
    final l10n = context.l10n;
    return [
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
    ];
  }

  Widget _buildApp(BuildContext context) {
    final l10n = context.l10n;
    return DesktopLayout(
      section: _section,
      opsTab: _opsTab,
      // The app mark goes home from anywhere; the sidebar's top slot either
      // swaps workspaces or backs out of the zone.
      onHome: () => _goTo(AppSection.home),
      onBack: _goBack,
      onSwitchWorkspace: _switchWorkspace,
      onOps: () => _goToOpsTab(_opsTab),
      onOpsTab: _goToOpsTab,
      pane: _pane,
      onPane: (pane) => setState(() => _pane = pane),
      itemCount: _section == AppSection.register
          ? _registerCount
          : _connectCount,
      // The title bar names the window, not the page: it sits beside the app
      // mark, and the content already carries its own heading. The compact
      // toolbar is the one that names the page.
      title: l10n.appTitle,
      pageTitle: _getPageTitle(context),
      titleBarActions: _chromeActions(context),
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
        };
      case AppSection.setup:
      case AppSection.home:
        return null;
    }
  }
}


/// A place the user has been, complete enough to put them back on it.
///
/// The zone alone would drop them on the default tab or the form, which reads
/// as a different screen from the one they left — so the tab and the pane
/// travel with it.
@immutable
class _Location {
  const _Location(this.section, this.opsTab, this.pane);

  final AppSection section;
  final OpsTab opsTab;
  final WorkspacePane pane;
}
