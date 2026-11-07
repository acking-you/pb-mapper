import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/pages/remote/add_computer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/touch_viewer.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';
import 'package:window_manager/window_manager.dart';

/// The viewer, shown by the app in place of the shell while
/// `ViewerHost.showing`, so there is only ever one. Back returns to the
/// list with the session kept, paused.
class RemoteViewerScreen extends StatefulWidget {
  const RemoteViewerScreen({super.key});

  @override
  State<RemoteViewerScreen> createState() => _RemoteViewerScreenState();
}

class _RemoteViewerScreenState extends State<RemoteViewerScreen> {
  RemoteDesktop? _remote;
  String _title = '';

  /// A phone or a tablet.
  static bool get _touch =>
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final remote = AppScope.of(context).remote;
    if (identical(remote, _remote)) return;
    _remote?.desktop.session.removeListener(_retitle);
    _remote = remote..desktop.session.addListener(_retitle);
    _title = remote.title;
    _setWindowTitle();
  }

  @override
  void dispose() {
    _remote?.desktop.session.removeListener(_retitle);
    _title = '';
    _setWindowTitle();
    super.dispose();
  }

  /// The computer's name, which changes only with the session's target.
  void _retitle() {
    final title = _remote?.title ?? '';
    if (title == _title) return;
    setState(() => _title = title);
    _setWindowTitle();
  }

  void _setWindowTitle() {
    if (!isFramelessDesktop) return;
    final title = _title.isEmpty ? 'pb-mapper' : '$_title — pb-mapper';
    unawaited(
      windowManager.setTitle(title).catchError((Object error) {
        debugPrint('Could not title the window: $error');
      }),
    );
  }

  /// Asks for the host's new invitation, which replaces the remembered
  /// computer's own: the only path that touches a computer whose identity
  /// changed. The add flow keeps its name.
  void _pasteNewInvite() {
    final remote = _remote;
    final id = remote?.desktop.session.value.target?.computerId;
    final computer = id == null ? null : remote?.computers.byId(id);
    if (computer == null) return;
    unawaited(showAddComputer(context, replacing: computer));
  }

  @override
  Widget build(BuildContext context) {
    final remote = AppScope.of(context).remote;
    final id = remote.desktop.session.value.target?.computerId;
    // Only a remembered computer has an invitation to replace.
    final paste = id != null && remote.computers.byId(id) != null
        ? _pasteNewInvite
        : null;
    return PopScope(
      canPop: false,
      // Back returns to the list with the session alive (paused).
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) remote.back();
      },
      child: Theme(
        data: viewerTheme(context),
        child: MediaQuery.withClampedTextScaling(
          maxScaleFactor: 1.3,
          child: EntranceMotion(
            offset: 0,
            // A Windows touchscreen keeps the desktop viewer, touching
            // directly.
            child: _touch
                ? TouchViewer(
                    desktop: remote.desktop,
                    prefs: remote.prefs,
                    title: _title,
                    onBack: remote.back,
                    onDisconnect: remote.disconnect,
                    onQuality: (quality) =>
                        unawaited(remote.setQuality(quality)),
                    onReconnect: () => unawaited(remote.reconnect()),
                    onPasteNewInvite: paste,
                  )
                : DesktopViewer(
                    desktop: remote.desktop,
                    prefs: remote.prefs,
                    title: _title,
                    onBack: remote.back,
                    onDisconnect: remote.disconnect,
                    onQuality: (quality) =>
                        unawaited(remote.setQuality(quality)),
                    onReconnect: () => unawaited(remote.reconnect()),
                    onPasteNewInvite: paste,
                  ),
          ),
        ),
      ),
    );
  }
}
