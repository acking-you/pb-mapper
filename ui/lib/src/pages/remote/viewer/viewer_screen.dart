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

  /// The session's computer is remembered: only such a computer has an
  /// invitation to replace. One added with Remember on becomes so at its
  /// first picture, during this viewing.
  bool _remembered = false;

  /// A phone or a tablet.
  static bool get _touch =>
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final remote = AppScope.of(context).remote;
    if (identical(remote, _remote)) return;
    _unlisten();
    _remote = remote;
    remote.desktop.session.addListener(_sync);
    remote.computers.addListener(_sync);
    _title = remote.title;
    _remembered = remote.rememberedComputer != null;
    _setWindowTitle();
  }

  @override
  void dispose() {
    _unlisten();
    _title = '';
    _setWindowTitle();
    super.dispose();
  }

  void _unlisten() {
    _remote?.desktop.session.removeListener(_sync);
    _remote?.computers.removeListener(_sync);
  }

  /// The computer's name, and whether it is remembered: they change with
  /// the session's target and with the remembered computers.
  void _sync() {
    final remote = _remote;
    if (remote == null) return;
    final title = remote.title;
    final remembered = remote.rememberedComputer != null;
    if (title == _title && remembered == _remembered) return;
    final retitled = title != _title;
    setState(() {
      _title = title;
      _remembered = remembered;
    });
    if (retitled) _setWindowTitle();
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
    final computer = _remote?.rememberedComputer;
    if (computer == null) return;
    unawaited(showAddComputer(context, replacing: computer));
  }

  @override
  Widget build(BuildContext context) {
    final remote = AppScope.of(context).remote;
    final paste = _remembered ? _pasteNewInvite : null;
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
