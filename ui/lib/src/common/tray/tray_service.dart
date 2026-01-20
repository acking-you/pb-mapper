import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:tray_manager/tray_manager.dart';
import 'package:window_manager/window_manager.dart';

class TrayStatus {
  final bool serverAvailable;
  final int activeConnections;
  final int registeredServices;

  const TrayStatus({
    required this.serverAvailable,
    required this.activeConnections,
    required this.registeredServices,
  });

  String get displayText {
    if (!serverAvailable) {
      return 'Offline · pb-mapper';
    }
    if (activeConnections > 0) {
      return 'Active · $activeConnections connections';
    }
    if (registeredServices > 0) {
      return 'Idle · $registeredServices services';
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
  );
  Timer? _statusTimer;
  Future<TrayStatus> Function()? _statusProvider;
  VoidCallback? _showApp;
  VoidCallback? _quitApp;

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
      await _applyStatus(const TrayStatus(
        serverAvailable: false,
        activeConnections: 0,
        registeredServices: 0,
      ));
    }
  }

  Future<void> _applyStatus(TrayStatus next) async {
    _status = next;
    final iconPath = _iconFor(next);
    await _trayManager.setIcon(iconPath);
    await _trayManager.setToolTip(next.displayText);
    await _trayManager.setContextMenu(
      Menu(
        items: [
          MenuItem(
            key: 'status',
            label: next.displayText,
            disabled: true,
          ),
          MenuItem.separator(),
          MenuItem(
            key: 'open',
            label: 'Open pb-mapper',
          ),
          MenuItem(
            key: 'refresh',
            label: 'Refresh status',
          ),
          MenuItem.separator(),
          MenuItem(
            key: 'quit',
            label: 'Quit',
          ),
        ],
      ),
    );
  }

  String _iconFor(TrayStatus status) {
    if (!status.serverAvailable) {
      return _assetPath('assets/tray/tray_offline');
    }
    if (status.activeConnections > 0) {
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
    _trayManager.popUpContextMenu();
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
