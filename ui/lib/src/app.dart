import 'dart:async';
import 'dart:io' show Directory, exit;
import 'dart:ui' show AppExitResponse;

import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/common/tray/tray_service.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/ffi/remote_computer_vault.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/pages/logs_page.dart';
import 'package:pb_mapper_ui/src/pages/overview_page.dart';
import 'package:pb_mapper_ui/src/pages/relay_page.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_screen.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_title_bar.dart';
import 'package:pb_mapper_ui/src/pages/settings_page.dart';
import 'package:pb_mapper_ui/src/pages/setup_flow.dart';
import 'package:pb_mapper_ui/src/pages/tunnels_page.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/app_shell.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/shell/sharing_indicator.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';
import 'package:pb_mapper_ui/src/updates/update_schedule.dart';
import 'package:toastification/toastification.dart' show ToastificationWrapper;
import 'package:window_manager/window_manager.dart';

/// The app. Owns the data store, the preferences, the galaxy's state and the
/// remote desktop, and decides between first-run setup, the six pages and
/// the viewer.
class PbMapperApp extends StatefulWidget {
  const PbMapperApp({
    super.key,
    required this.api,
    this.changes,
    this.prefs,
    this.viewerPrefs,
    this.vault,
    this.thumbnailDir,
    this.needsSetup,
    this.initialPage,
    this.desktop = false,
    this.updates,
    this.checkForUpdates = false,
  });

  final PbMapperApiClient api;

  /// Defaults to the Rust change stream; tests pass their own.
  final Stream<StateChange>? changes;
  final AppPrefs? prefs;
  final ViewerPrefs? viewerPrefs;

  /// Where remembered computers are kept; tests pass their own.
  final RemoteComputerVault? vault;

  /// The directory their last pictures go in: one under the app's support
  /// directory unless a test passes its own.
  final Future<Directory> Function()? thumbnailDir;

  /// Null decides from the saved config and tunnels whether this install was
  /// ever set up.
  final bool? needsSetup;

  /// The page to open on, over the one remembered from last time: Remote
  /// desktop after the helper's restart hand-off (`--desktop`).
  final AppPage? initialPage;

  /// Run the window's desktop life: the tray, and closing to it.
  final bool desktop;

  /// Defaults to one that updates from GitHub; tests pass their own.
  final UpdateController? updates;

  /// Look for a newer release in the background, while the user lets it.
  /// Off in tests, which must not reach GitHub.
  final bool checkForUpdates;

  @override
  State<PbMapperApp> createState() => _PbMapperAppState();
}

class _PbMapperAppState extends State<PbMapperApp> with WindowListener {
  late final AppStore _store = AppStore(
    api: widget.api,
    changes: widget.changes,
    onRestartReady: () async => _quit(),
    vault: widget.vault,
    thumbnailDir: widget.thumbnailDir,
  );
  late final RelayStore _relay = RelayStore(app: _store);
  late final AppPrefs _prefs = widget.prefs ?? AppPrefs();
  late final ViewerPrefs _viewerPrefs = widget.viewerPrefs ?? ViewerPrefs();
  late final ViewerHost _viewer = ViewerHost(_store.desktop);
  late final RemoteDesktop _remote = RemoteDesktop(
    desktop: _store.desktop,
    computers: _store.computers,
    viewer: _viewer,
    prefs: _viewerPrefs,
    relayAddress: () => _store.relayAddress,
    credentialKeyId: () => _store.credentialKeyId ?? 0,
  );
  final SharingChrome _sharing = SharingChrome();
  late final UpdateController _updates =
      widget.updates ?? UpdateController(quit: () async => _quit());
  UpdateSchedule? _updateSchedule;
  final GalaxyView _galaxy = GalaxyView();
  AppLifecycleListener? _lifecycle;
  late AppPage _page = widget.initialPage ?? AppPage.overview;

  /// Null while deciding, so a returning user never sees setup flash by.
  bool? _setup;
  bool _allowExit = false;

  /// The viewer session the window was last brought back for.
  DateTime? _shownForViewer;

  @override
  void initState() {
    super.initState();
    _prefs.addListener(_onPrefs);
    _viewer.addListener(_onViewer);
    unawaited(_boot());
    if (widget.desktop) _startDesktop();
  }

  Future<void> _boot() async {
    final started = _store.start();
    // Setup seeds its fields from the store once, so the saved config has to
    // be in it first: shown any earlier, setup would offer a blank credential
    // and saving the relay would overwrite the real one with it.
    final (_, read) = await (
      Future.wait([_prefs.load(), _viewerPrefs.load()]),
      _store.ready,
    ).wait;
    // Whether this install needs setup comes from that same read. Reading
    // again would queue behind the relay check the store makes next, which
    // holds the native state lock until the relay answers. A read that failed
    // is not evidence of a new install, and dropping an existing user into
    // setup is the worse mistake.
    final setup =
        widget.needsSetup ??
        (read &&
            SetupState.needsSetup(
              serverAddress: _store.relayAddress,
              hasTunnels: _store.tunnels.isNotEmpty,
            ));
    if (!mounted) return;
    setState(() {
      _setup = setup;
      if (widget.initialPage == null) {
        _page = AppPage.parse(_prefs.lastPage) ?? AppPage.overview;
      }
    });
    await started;
    if (widget.checkForUpdates && updatesSupported && mounted) {
      unawaited(_reportUpdate());
      _updateSchedule = UpdateSchedule(
        updates: _updates,
        prefs: _prefs,
        announce: (version) => showToast(
          null,
          lookupAppLocalizations(
            _prefs.activeLocale,
          ).updateAnnounce('$version'),
          kind: ToastKind.info,
        ),
      )..start();
    }
  }

  /// Say what became of the update handed off before the last quit. The
  /// installer reopens the app from the background, where Windows lets no
  /// window take the front, so it comes forward here too: otherwise the app
  /// seems to have vanished, and the update to have failed.
  Future<void> _reportUpdate() async {
    final outcome = await _updates.takeOutcome();
    if (outcome == null || !mounted) return;
    if (widget.desktop) {
      try {
        await windowManager.show();
        await windowManager.focus();
      } catch (error) {
        debugPrint('Could not bring the window forward: $error');
      }
    }
    final l10n = lookupAppLocalizations(_prefs.activeLocale);
    final version = '${outcome.to}';
    if (outcome.installed) {
      showToast(null, l10n.updateDone(version), kind: ToastKind.success);
    } else {
      showToast(
        null,
        l10n.updateNotDone(version),
        kind: ToastKind.error,
        description: l10n.updateNotDoneHint,
      );
    }
  }

  void _onPrefs() {
    if (!mounted) return;
    setState(() {});
    if (widget.desktop) {
      unawaited(TrayService.instance.updateStrings(_trayStrings()));
    }
  }

  void _onViewer() {
    if (mounted) setState(() {});
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
    // The tray reads the stores rather than asking the Rust side on a timer
    // of its own, so it changes the moment the pages do.
    _store.addListener(_refreshTray);
    _store.desktop.hostView.addListener(_onHostView);
  }

  Future<void> _startTray() async {
    try {
      await windowManager.setPreventClose(true);
      await TrayService.instance.initialize(
        statusProvider: () async => _trayStatus(),
        showApp: TrayService.instance.showFromTray,
        quitApp: () => unawaited(_quitFromTray()),
        onDisconnectViewer: () => unawaited(_store.desktop.endViewerSession()),
        onStopSharing: () => unawaited(_store.desktop.stopHost()),
        strings: _trayStrings(),
      );
    } catch (e) {
      debugPrint('Tray initialization failed: $e');
    }
  }

  void _refreshTray() => unawaited(TrayService.instance.refreshStatus());

  /// A sharing computer's window usually waits hidden in the tray, where the
  /// title bar's controls are out of sight. When a viewer arrives, the window
  /// comes back without taking the focus, once per viewer: closing it again
  /// is respected (D15, owner decision OD-1).
  void _onHostView() {
    _refreshTray();
    final viewer = _store.desktop.hostView.value.viewer;
    if (viewer == null || viewer.since == _shownForViewer) return;
    _shownForViewer = viewer.since;
    unawaited(_showForViewer());
  }

  Future<void> _showForViewer() async {
    try {
      if (await windowManager.isVisible() &&
          !await windowManager.isMinimized()) {
        return;
      }
      await windowManager.show(inactive: true);
    } catch (error) {
      debugPrint('Could not show the window for a viewer: $error');
    }
  }

  TrayStatus _trayStatus() {
    final online = _store.relayState == RelayState.online;
    final host = _store.desktop.hostView.value;
    int running(List<Tunnel> tunnels) =>
        tunnels.where((t) => t.health == TunnelHealth.running).length;
    return TrayStatus(
      serverAvailable: online,
      activeConnections: 0,
      registeredServices: online
          ? _store.relayServices.length
          : running(_store.shares),
      connectedClients: running(_store.connections),
      host: host.sharing ? host : null,
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
      hostSharing: l10n.trayHostSharing,
      hostViewed: l10n.trayHostViewed,
      hostControlled: l10n.trayHostControlled,
      hostPaused: l10n.trayHostPaused,
      disconnectViewer: l10n.hostDisconnectViewer,
      stopSharing: l10n.hostStop,
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

  /// Quit from the tray ends sharing and viewing first, so the other
  /// computer is told why instead of losing the connection. An update's or
  /// the helper hand-off's quit is a restart and keeps [_quit].
  Future<void> _quitFromTray() async {
    if (_allowExit) return;
    _allowExit = true;
    await _store.desktop.shutdown();
    _quit();
  }

  @override
  void dispose() {
    _prefs.removeListener(_onPrefs);
    _viewer.removeListener(_onViewer);
    _store.removeListener(_refreshTray);
    if (widget.desktop) {
      _store.desktop.hostView.removeListener(_onHostView);
      windowManager.removeListener(this);
      TrayService.instance.dispose();
    }
    _lifecycle?.dispose();
    _updateSchedule?.dispose();
    if (widget.updates == null) _updates.dispose();
    _remote.dispose();
    _viewer.dispose();
    _sharing.dispose();
    _relay.dispose();
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
      // A dialog's barrier hides the title bar's capsule: the observer tells
      // the fallback layer to step in.
      navigatorObservers: [_sharing.observer],
      // Inside MaterialApp, so a toast raised without a context still wears
      // the theme and speaks the language. The fallback layer sits above the
      // navigator, so above every route, dialog and the viewer (D15), and
      // the caption buttons above that: one set for the app's life, which no
      // page transition moves and no barrier covers. Their tooltips need an
      // overlay up here too.
      builder: (context, child) => ToastificationWrapper(
        child: Overlay.wrap(
          child: Stack(
            children: [
              Positioned.fill(child: child ?? const SizedBox.shrink()),
              SharingFallbackLayer(desktop: _store.desktop, chrome: _sharing),
              if (drawsCaptionButtons)
                Positioned.fill(child: _windowControls(context)),
            ],
          ),
        ),
      ),
      home: Builder(builder: _home),
    );
  }

  /// The caption buttons as tall as the title bar on screen, in its tone.
  WindowControls _windowControls(BuildContext context) => switch (_setup) {
    false when _viewer.showing => const WindowControls(
      height: kViewerTitleBarHeight,
      tone: CaptionTone.viewer,
    ),
    false => WindowControls(
      height: usesRail(context) ? kPageBarHeight : kCompactPageBarHeight,
    ),
    // Loading and first-run setup have no bar: Windows' own height.
    _ => const WindowControls(height: kCaptionButtonHeight),
  };

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
      relay: _relay,
      prefs: _prefs,
      updates: _updates,
      galaxy: _galaxy,
      remote: _remote,
      sharing: _sharing,
      page: _page,
      go: _go,
      // The viewer is a mode of the app, not a route: while it shows, the
      // shell is not built at all, so there is only ever one viewer.
      child: _viewer.showing
          ? const RemoteViewerScreen()
          : AppShell(
              page: _page,
              onSelect: _go,
              child: switch (_page) {
                AppPage.overview => const OverviewPage(),
                AppPage.tunnels => const TunnelsPage(),
                AppPage.remote => const RemotePage(),
                AppPage.relay => const RelayPage(),
                AppPage.logs => const LogsPage(),
                AppPage.settings => const SettingsPage(),
              },
            ),
    );
  }
}
