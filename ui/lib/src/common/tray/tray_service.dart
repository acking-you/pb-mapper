import 'dart:async' show Timer, unawaited;
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

  /// Replaces the strings and redraws, so a language switch reaches the tray
  /// without waiting for the next status poll.
  Future<void> updateStrings(TrayStrings strings) async {
    _strings = strings;
    if (_initialized) {
      await _applyStatus(_status);
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
      return;
    }

    _statusProvider = statusProvider;
    _showApp = showApp;
    _quitApp = quitApp;

    _trayManager.addListener(this);
    await _applyStatus(_status);
    _startPolling();
    _initialized = true;
  }

  void dispose() {
    _statusTimer?.cancel();
    _statusTimer = null;
    _trayManager.removeListener(this);
    _initialized = false;
  }

  void _startPolling() {
    _statusTimer?.cancel();
    _statusTimer = Timer.periodic(const Duration(seconds: 6), (_) async {
      await refreshStatus();
    });
    refreshStatus();
  }

  Future<void> refreshStatus() async {
    if (_statusProvider == null) {
      return;
    }
    try {
      final next = await _statusProvider!();
      await _applyStatus(next);
    } catch (_) {
      await _applyStatus(
        const TrayStatus(
          serverAvailable: false,
          activeConnections: 0,
          registeredServices: 0,
          connectedClients: 0,
        ),
      );
    }
  }

  Future<void> _applyStatus(TrayStatus next) async {
    _status = next;
    final strings = _strings;
    if (strings == null) {
      return;
    }
    final statusText = next.displayText(strings);
    final iconPath = _iconFor(next);
    if (_iconSupported) {
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
    }
    if (_toolTipSupported) {
      await _invokeTrayMethod(
        action: () => _trayManager.setToolTip(statusText),
        onUnsupported: () => _toolTipSupported = false,
        methodName: 'setToolTip',
      );
    }
    if (_contextMenuSupported) {
      await _invokeTrayMethod(
        action: () => _trayManager.setContextMenu(
          Menu(
            items: [
              MenuItem(key: 'status', label: statusText, disabled: true),
              MenuItem.separator(),
              MenuItem(key: 'open', label: strings.open),
              MenuItem(key: 'refresh', label: strings.refresh),
              MenuItem.separator(),
              MenuItem(key: 'quit', label: strings.quit),
            ],
          ),
        ),
        onUnsupported: () => _contextMenuSupported = false,
        methodName: 'setContextMenu',
      );
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
    if (!_contextMenuSupported) {
      return;
    }
    unawaited(
      _invokeTrayMethod(
        // Win32 requires the menu's owner to be the foreground window before
        // TrackPopupMenu, or the menu stays on screen when the user clicks
        // away. bringAppToFront is the plugin's only route to
        // SetForegroundWindow; it is deprecated upstream but still the fix.
        // ignore: deprecated_member_use
        action: () => _trayManager.popUpContextMenu(bringAppToFront: true),
        onUnsupported: () => _contextMenuSupported = false,
        methodName: 'popUpContextMenu',
      ),
    );
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
