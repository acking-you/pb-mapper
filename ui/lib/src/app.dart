import 'dart:async';
import 'dart:io' show exit;
import 'dart:ui' show AppExitResponse;

import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/common/tray/tray_service.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/pages/logs_page.dart';
import 'package:pb_mapper_ui/src/pages/overview_page.dart';
import 'package:pb_mapper_ui/src/pages/settings_page.dart';
import 'package:pb_mapper_ui/src/pages/setup_flow.dart';
import 'package:pb_mapper_ui/src/pages/tunnels_page.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/app_shell.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:toastification/toastification.dart' show ToastificationWrapper;
import 'package:window_manager/window_manager.dart';

/// The app. Owns the data store, the preferences and the galaxy's state, and
/// decides between first-run setup and the four pages.
class PbMapperApp extends StatefulWidget {
  const PbMapperApp({
    super.key,
    required this.api,
    this.changes,
    this.prefs,
    this.needsSetup,
    this.initialPage,
    this.desktop = false,
  });

  final PbMapperApiClient api;

  /// Defaults to the Rust change stream; tests pass their own.
  final Stream<StateChange>? changes;
  final AppPrefs? prefs;

  /// Null asks the API whether this install was ever set up.
  final bool? needsSetup;

  /// The page to open on, over the one remembered from last time.
  final AppPage? initialPage;

  /// Run the window's desktop life: the tray, and closing to it.
  final bool desktop;

  @override
  State<PbMapperApp> createState() => _PbMapperAppState();
}

class _PbMapperAppState extends State<PbMapperApp> with WindowListener {
  late final AppStore _store = AppStore(
    api: widget.api,
    changes: widget.changes,
  );
  late final AppPrefs _prefs = widget.prefs ?? AppPrefs();
  final GalaxyView _galaxy = GalaxyView();
  AppLifecycleListener? _lifecycle;
  late AppPage _page = widget.initialPage ?? AppPage.overview;

  /// Null while deciding, so a returning user never sees setup flash by.
  bool? _setup;
  bool _allowExit = false;

  @override
  void initState() {
    super.initState();
    _prefs.addListener(_onPrefs);
    unawaited(_boot());
    if (widget.desktop) _startDesktop();
  }

  Future<void> _boot() async {
    await _prefs.load();
    final setup = widget.needsSetup ?? await SetupState.needsSetup(widget.api);
    if (!mounted) return;
    setState(() {
      _setup = setup;
      if (widget.initialPage == null) {
        _page = AppPage.parse(_prefs.lastPage) ?? AppPage.overview;
      }
    });
    await _store.start();
  }

  void _onPrefs() {
    if (!mounted) return;
    setState(() {});
    if (widget.desktop) {
      unawaited(TrayService.instance.updateStrings(_trayStrings()));
    }
  }

  void _go(AppPage page) {
    if (page == _page) return;
    setState(() => _page = page);
    unawaited(_prefs.setLastPage(page.name));
  }

  void _finishSetup() => setState(() {
    _setup = false;
    _page = AppPage.overview;
  });

  // --- Desktop: the tray, and closing to it rather than quitting.

  void _startDesktop() {
    _lifecycle = AppLifecycleListener(
      onExitRequested: () async {
        if (!_allowExit) {
          await TrayService.instance.hideToTray();
          return AppExitResponse.cancel;
        }
        _shutdown();
        return AppExitResponse.exit;
      },
    );
    windowManager.addListener(this);
    unawaited(_startTray());
    // The tray reads the store rather than asking the Rust side on a timer of
    // its own, so it changes the moment the pages do.
    _store.addListener(_refreshTray);
  }

  Future<void> _startTray() async {
    try {
      await windowManager.setPreventClose(true);
      await TrayService.instance.initialize(
        statusProvider: () async => _trayStatus(),
        showApp: TrayService.instance.showFromTray,
        quitApp: _quit,
        strings: _trayStrings(),
      );
    } catch (e) {
      debugPrint('Tray initialization failed: $e');
    }
  }

  void _refreshTray() => unawaited(TrayService.instance.refreshStatus());

  TrayStatus _trayStatus() {
    final online = _store.relayState == RelayState.online;
    int running(List<Tunnel> tunnels) =>
        tunnels.where((t) => t.health == TunnelHealth.running).length;
    return TrayStatus(
      serverAvailable: online,
      activeConnections: 0,
      registeredServices: online
          ? _store.relayServices.length
          : running(_store.shares),
      connectedClients: running(_store.connections),
    );
  }

  /// The tray is not a widget, so its strings come from the active locale.
  TrayStrings _trayStrings() {
    final l10n = lookupAppLocalizations(_prefs.activeLocale);
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

  @override
  void onWindowClose() {
    unawaited(TrayService.instance.hideToTray());
  }

  void _shutdown() {
    TrayService.instance.dispose();
    LogManager().dispose();
    PbMapperService().dispose();
  }

  void _quit() {
    _allowExit = true;
    _shutdown();
    exit(0);
  }

  @override
  void dispose() {
    _prefs.removeListener(_onPrefs);
    _store.removeListener(_refreshTray);
    if (widget.desktop) {
      windowManager.removeListener(this);
      TrayService.instance.dispose();
    }
    _lifecycle?.dispose();
    _store.dispose();
    _galaxy.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      onGenerateTitle: (context) => AppLocalizations.of(context)!.appTitle,
      debugShowCheckedModeBanner: false,
      locale: _prefs.locale,
      supportedLocales: LocaleController.supported,
      localizationsDelegates: const [
        AppLocalizations.delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
      ],
      theme: buildAppTheme(Brightness.light),
      darkTheme: buildAppTheme(Brightness.dark),
      themeMode: _prefs.themeMode,
      // Every colour is lerped between the themes over this, so switching does
      // not repaint the whole window in one frame.
      themeAnimationDuration: const Duration(milliseconds: 320),
      themeAnimationCurve: Curves.easeInOutCubic,
      // Inside MaterialApp, so a toast raised without a context still wears
      // the theme and speaks the language.
      builder: (context, child) =>
          ToastificationWrapper(child: child ?? const SizedBox.shrink()),
      home: Builder(builder: _home),
    );
  }

  Widget _home(BuildContext context) {
    final setup = _setup;
    if (setup == null) {
      return const Scaffold(body: AppBackdrop(child: SizedBox.expand()));
    }
    if (setup) {
      return SetupFlow(store: _store, onDone: _finishSetup);
    }
    return AppScope(
      store: _store,
      prefs: _prefs,
      galaxy: _galaxy,
      page: _page,
      go: _go,
      child: AppShell(
        page: _page,
        onSelect: _go,
        child: switch (_page) {
          AppPage.overview => const OverviewPage(),
          AppPage.tunnels => const TunnelsPage(),
          AppPage.logs => const LogsPage(),
          AppPage.settings => const SettingsPage(),
        },
      ),
    );
  }
}
