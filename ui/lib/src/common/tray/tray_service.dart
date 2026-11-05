import 'dart:async' show Completer, Timer, unawaited;
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tray_manager/tray_manager.dart';
import 'package:window_manager/window_manager.dart';

class TrayStatus {
  final bool serverAvailable;
  final int activeConnections;
  final int registeredServices;
  final int connectedClients;

  const TrayStatus({
    required this.serverAvailable,
    required this.activeConnections,
    required this.registeredServices,
    required this.connectedClients,
  });

  bool get hasConnections =>
      activeConnections > 0 || registeredServices > 0 || connectedClients > 0;

  int get connectionCount => activeConnections + connectedClients;

  String displayText(TrayStrings strings) {
    if (!serverAvailable) {
      return strings.offline;
    }
    if (activeConnections > 0 || connectedClients > 0) {
      return strings.connections(connectionCount);
    }
    if (registeredServices > 0) {
      return strings.services(registeredServices);
    }
    return strings.onlineIdle;
  }
}

/// The tray lives outside the widget tree, so it cannot read localizations from
/// a BuildContext. The app hands it the current strings instead, and hands them
/// again whenever the language changes.
class TrayStrings {
  const TrayStrings({
    required this.offline,
    required this.connections,
    required this.services,
    required this.onlineIdle,
    required this.open,
    required this.refresh,
    required this.quit,
  });

  final String offline;
  final String Function(int) connections;
  final String Function(int) services;
  final String onlineIdle;
  final String open;
  final String refresh;
  final String quit;
}

class TrayService with TrayListener {
  TrayService._();
  static final TrayService instance = TrayService._();

  final TrayManager _trayManager = TrayManager.instance;
  final WindowManager _windowManager = WindowManager.instance;

  bool _initialized = false;
  TrayStatus _status = const TrayStatus(
    serverAvailable: false,
    activeConnections: 0,
    registeredServices: 0,
    connectedClients: 0,
  );
  Timer? _statusTimer;
  Future<TrayStatus> Function()? _statusProvider;
  VoidCallback? _showApp;
  VoidCallback? _quitApp;
  bool _iconSupported = true;
  bool _toolTipSupported = true;
  bool _contextMenuSupported = true;
  TrayStrings? _strings;
  final _statusItem = MenuItem(key: 'status', disabled: true);
  final _openItem = MenuItem(key: 'open');
  final _refreshItem = MenuItem(key: 'refresh');
  final _quitItem = MenuItem(key: 'quit');
  // Windows can deliver WM_COMMAND after the popup has returned. Retain IDs
  // across renders so that a closing click still resolves to the same action.
  late final _menu = Menu(
    items: [
      _statusItem,
      MenuItem.separator(),
      _openItem,
      _refreshItem,
      MenuItem.separator(),
      _quitItem,
    ],
  );
  String? _renderedIcon, _renderedToolTip;
  (String, String, String, String)? _renderedMenu;
  Future<void>? _rendering;
  bool _dirty = false, _menuOpen = false;
  int _statusRequest = 0, _lifecycle = 0;

  /// Replaces the strings and redraws, so a language switch reaches the tray
  /// without waiting for the next status poll.
  Future<void> updateStrings(TrayStrings strings) async {
    _strings = strings;
    if (_initialized) {
      await _scheduleRender();
    }
  }

  Future<void> initialize({
    required Future<TrayStatus> Function() statusProvider,
    required VoidCallback showApp,
    required VoidCallback quitApp,
    required TrayStrings strings,
  }) async {
    if (!Platform.isWindows && !Platform.isLinux && !Platform.isMacOS) {
      return;
    }
    _strings = strings;
    if (_initialized) {
      _statusProvider = statusProvider;
      _showApp = showApp;
      _quitApp = quitApp;
      await refreshStatus();
      return;
    }

    _statusProvider = statusProvider;
    _showApp = showApp;
    _quitApp = quitApp;

    _trayManager.addListener(this);
    _initialized = true;
    final lifecycle = _lifecycle;
    try {
      await _applyStatus(_status);
      if (!_initialized || lifecycle != _lifecycle) return;
      _startPolling();
    } catch (_) {
      dispose();
      rethrow;
    }
  }

  void dispose() {
    _statusTimer?.cancel();
    _statusTimer = null;
    _trayManager.removeListener(this);
    _initialized = false;
    _statusRequest++;
    _lifecycle++;
    _dirty = false;
    _renderedIcon = _renderedToolTip = null;
    _renderedMenu = null;
  }

  void _startPolling() {
    _statusTimer?.cancel();
    _statusTimer = Timer.periodic(const Duration(seconds: 6), (_) async {
      await refreshStatus();
    });
    refreshStatus();
  }

  Future<void> refreshStatus() async {
    if (!_initialized || _statusProvider == null) {
      return;
    }
    final request = ++_statusRequest;
    TrayStatus next;
    try {
      next = await _statusProvider!();
    } catch (_) {
      next = const TrayStatus(
        serverAvailable: false,
        activeConnections: 0,
        registeredServices: 0,
        connectedClients: 0,
      );
    }
    // A slower older read must never overwrite the most recent observation.
    if (!_initialized || request != _statusRequest) return;
    try {
      await _applyStatus(next);
    } catch (error) {
      debugPrint('Tray refresh failed: $error');
    }
  }

  Future<void> _applyStatus(TrayStatus next) {
    _status = next;
    return _scheduleRender();
  }

  Future<void> _scheduleRender() {
    _dirty = true;
    if (_rendering != null) return _rendering!;
    if (!_initialized || _menuOpen) return Future<void>.value();
    final completion = Completer<void>();
    _rendering = completion.future;
    unawaited(_drainRender(completion));
    return completion.future;
  }

  Future<void> _drainRender(Completer<void> completion) async {
    try {
      while (_initialized && _dirty && !_menuOpen) {
        _dirty = false;
        await _renderStatus();
      }
      completion.complete();
    } catch (error, stack) {
      _dirty = true;
      completion.completeError(error, stack);
    } finally {
      _rendering = null;
    }
  }

  Future<void> _renderStatus() async {
    final strings = _strings;
    if (!_initialized || strings == null) {
      return;
    }
    final lifecycle = _lifecycle;
    final statusText = _status.displayText(strings);
    final iconPath = _iconFor(_status);
    final menuLabels = (
      statusText,
      strings.open,
      strings.refresh,
      strings.quit,
    );
    if (_iconSupported && _renderedIcon != iconPath) {
      await _invokeTrayMethod(
        // A template image is black plus alpha, and macOS tints it to match
        // the menu bar: light in dark mode, dark in light mode, inverted while
        // the item is highlighted. That is why the rest of the bar is
        // monochrome, so the state has to read from the badge shape instead of
        // colour. Only macOS has the concept; the flag is ignored elsewhere.
        action: () => _trayManager.setIcon(iconPath, isTemplate: true),
        onUnsupported: () => _iconSupported = false,
        methodName: 'setIcon',
      );
      if (!_initialized || lifecycle != _lifecycle) return;
      _renderedIcon = iconPath;
    }
    if (_toolTipSupported && _renderedToolTip != statusText) {
      await _invokeTrayMethod(
        action: () => _trayManager.setToolTip(statusText),
        onUnsupported: () => _toolTipSupported = false,
        methodName: 'setToolTip',
      );
      if (!_initialized || lifecycle != _lifecycle) return;
      _renderedToolTip = statusText;
    }
    if (_contextMenuSupported && _renderedMenu != menuLabels) {
      _statusItem.label = statusText;
      _openItem.label = strings.open;
      _refreshItem.label = strings.refresh;
      _quitItem.label = strings.quit;
      await _invokeTrayMethod(
        action: () => _trayManager.setContextMenu(_menu),
        onUnsupported: () => _contextMenuSupported = false,
        methodName: 'setContextMenu',
      );
      if (!_initialized || lifecycle != _lifecycle) return;
      _renderedMenu = menuLabels;
    }
  }

  Future<void> _invokeTrayMethod({
    required Future<void> Function() action,
    required VoidCallback onUnsupported,
    required String methodName,
  }) async {
    try {
      await action();
    } on MissingPluginException {
      onUnsupported();
      debugPrint('Tray method "$methodName" is unavailable on this platform.');
    } on UnimplementedError {
      onUnsupported();
      debugPrint(
        'Tray method "$methodName" is unimplemented on this platform.',
      );
    } on PlatformException catch (e) {
      final code = e.code.toLowerCase();
      if (code.contains('unimplemented') || code.contains('missing')) {
        onUnsupported();
        debugPrint(
          'Tray method "$methodName" is unsupported: ${e.code} ${e.message ?? ''}',
        );
        return;
      }
      rethrow;
    }
  }

  String _iconFor(TrayStatus status) {
    if (!status.serverAvailable) {
      return _assetPath('assets/tray/tray_offline');
    }
    if (status.hasConnections) {
      return _assetPath('assets/tray/tray_active');
    }
    return _assetPath('assets/tray/tray_idle');
  }

  String _assetPath(String base) {
    if (Platform.isWindows) {
      return '$base.ico';
    }
    if (Platform.isMacOS) {
      // The plugin reads the asset with rootBundle.load, which takes an exact
      // key and so never resolves a resolution variant, then pins the image to
      // 18pt. Ask for the 2x file directly: on Retina it fills those points
      // properly, and on a 1x display macOS scales it back down cleanly.
      return '$base@2x.png';
    }
    return '$base.png';
  }

  @override
  void onTrayIconMouseDown() {
    // On macOS the menu bar convention is that a left click opens the menu,
    // and it is also the only click the plugin reports reliably there: the
    // status item's right-click arrives through an NSView subview that does
    // not always receive the event. Elsewhere a left click raises the window
    // and the menu stays on the right button.
    if (Platform.isMacOS) {
      _popUpContextMenu();
      return;
    }
    _showApp?.call();
  }

  @override
  void onTrayIconRightMouseDown() {
    _popUpContextMenu();
  }

  void _popUpContextMenu() {
    if (!_initialized || !_contextMenuSupported || _menuOpen) {
      return;
    }
    // Reserve synchronously: repeated clicks must not nest native menu loops.
    _menuOpen = true;
    unawaited(
      _showMenu().catchError((Object error) {
        debugPrint('Tray menu failed: $error');
      }),
    );
  }

  Future<void> _showMenu() async {
    final lifecycle = _lifecycle;
    try {
      // Finish any in-flight native update before opening the menu. While
      // TrackPopupMenu is running, collect desired state without replacing its
      // HMENU entries or the Dart menu IDs used to dispatch the user's click.
      await _rendering;
      if (!_initialized || lifecycle != _lifecycle) return;
      await _renderStatus();
      if (!_initialized || lifecycle != _lifecycle) return;
      await _invokeTrayMethod(
        // Win32 requires the menu's owner to be the foreground window before
        // TrackPopupMenu, or the menu stays on screen when the user clicks
        // away. bringAppToFront is the plugin's only route to
        // SetForegroundWindow; it is deprecated upstream but still the fix.
        // ignore: deprecated_member_use
        action: () => _trayManager.popUpContextMenu(bringAppToFront: true),
        onUnsupported: () => _contextMenuSupported = false,
        methodName: 'popUpContextMenu',
      );
    } finally {
      _menuOpen = false;
      if (_initialized && _dirty) await _scheduleRender();
    }
  }

  @override
  void onTrayMenuItemClick(MenuItem menuItem) {
    switch (menuItem.key) {
      case 'open':
        _showApp?.call();
        break;
      case 'refresh':
        refreshStatus();
        break;
      case 'quit':
        _quitApp?.call();
        break;
    }
  }

  Future<void> hideToTray() async {
    await _windowManager.hide();
  }

  Future<void> showFromTray() async {
    if (await _windowManager.isMinimized()) {
      await _windowManager.restore();
    }
    await _windowManager.show();
    await _windowManager.focus();

    // macOS hides by ordering the window out, which also drops the app from the
    // foreground. A single show/focus pair sometimes lands while the shell still
    // owns activation — the click on the tray just gave it away — and the window
    // stays gone. Confirm it came back and ask once more if it did not.
    if (!Platform.isMacOS) return;
    await Future<void>.delayed(const Duration(milliseconds: 120));
    if (await _windowManager.isVisible()) return;
    await _windowManager.show();
    await _windowManager.focus();
  }
}
