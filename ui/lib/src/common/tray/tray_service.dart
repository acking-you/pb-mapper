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

  String get displayText {
    if (!serverAvailable) {
      return 'Offline · pb-mapper';
    }
    if (activeConnections > 0 || connectedClients > 0) {
      return 'Connected · $connectionCount connections';
    }
    if (registeredServices > 0) {
      return 'Connected · $registeredServices services';
    }
    return 'Online · No services';
  }
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

  Future<void> initialize({
    required Future<TrayStatus> Function() statusProvider,
    required VoidCallback showApp,
    required VoidCallback quitApp,
  }) async {
    if (!Platform.isWindows && !Platform.isLinux && !Platform.isMacOS) {
      return;
    }
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
    final iconPath = _iconFor(next);
    if (_iconSupported) {
      await _invokeTrayMethod(
        action: () => _trayManager.setIcon(iconPath),
        onUnsupported: () => _iconSupported = false,
        methodName: 'setIcon',
      );
    }
    if (_toolTipSupported) {
      await _invokeTrayMethod(
        action: () => _trayManager.setToolTip(next.displayText),
        onUnsupported: () => _toolTipSupported = false,
        methodName: 'setToolTip',
      );
    }
    if (_contextMenuSupported) {
      await _invokeTrayMethod(
        action: () => _trayManager.setContextMenu(
          Menu(
            items: [
              MenuItem(key: 'status', label: next.displayText, disabled: true),
              MenuItem.separator(),
              MenuItem(key: 'open', label: 'Open pb-mapper'),
              MenuItem(key: 'refresh', label: 'Refresh status'),
              MenuItem.separator(),
              MenuItem(key: 'quit', label: 'Quit'),
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
    return '$base.png';
  }

  @override
  void onTrayIconMouseDown() {
    _showApp?.call();
  }

  @override
  void onTrayIconRightMouseDown() {
    if (!_contextMenuSupported) {
      return;
    }
    unawaited(
      _invokeTrayMethod(
        action: () => _trayManager.popUpContextMenu(),
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
    await _windowManager.show();
    await _windowManager.focus();
  }
}
