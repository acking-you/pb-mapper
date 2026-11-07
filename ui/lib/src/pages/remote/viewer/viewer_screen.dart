import 'dart:async';

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/pages/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';
import 'package:window_manager/window_manager.dart';

/// The viewer, shown by the app in place of the shell while
/// `ViewerHost.showing`, so there is only ever one.
///
/// Transitional: it hosts the previous `DesktopViewer` until the full-window
/// viewer replaces it. That viewer pops its route on Disconnect, so it runs
/// in a navigator of its own whose removal leaves the viewer.
class RemoteViewerScreen extends StatefulWidget {
  const RemoteViewerScreen({super.key});

  @override
  State<RemoteViewerScreen> createState() => _RemoteViewerScreenState();
}

class _RemoteViewerScreenState extends State<RemoteViewerScreen> {
  static const _base = ValueKey('viewer-base');
  static const _viewer = ValueKey('viewer');
  String? _title;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final title = AppScope.of(context).remote.title;
    if (title != _title) {
      _title = title;
      _setWindowTitle(title.isEmpty ? 'pb-mapper' : '$title — pb-mapper');
    }
  }

  @override
  void dispose() {
    _setWindowTitle('pb-mapper');
    super.dispose();
  }

  void _setWindowTitle(String title) {
    if (!isFramelessDesktop) return;
    unawaited(
      windowManager.setTitle(title).catchError((Object error) {
        debugPrint('Could not title the window: $error');
      }),
    );
  }

  @override
  Widget build(BuildContext context) {
    final remote = AppScope.of(context).remote;
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
            child: Scaffold(
              backgroundColor: ViewerPalette.ground,
              body: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  if (isFramelessDesktop)
                    SizedBox(
                      height: 36,
                      child: Row(
                        children: [
                          const Expanded(child: WindowDragArea()),
                          if (drawsCaptionButtons)
                            const Padding(
                              padding: EdgeInsetsDirectional.only(end: 4),
                              child: CaptionButtons(tone: CaptionTone.viewer),
                            ),
                        ],
                      ),
                    ),
                  Expanded(
                    child: Navigator(
                      pages: [
                        const MaterialPage(key: _base, child: SizedBox()),
                        MaterialPage(
                          key: _viewer,
                          child: DesktopViewer(store: remote.desktop),
                        ),
                      ],
                      // The old viewer's back arrow leaves the picture; its
                      // Disconnect ends the session before it pops.
                      onDidRemovePage: (page) {
                        if (page.key != _viewer) return;
                        if (remote.desktop.session.value.target == null) {
                          remote.viewer.close();
                        } else {
                          remote.back();
                        }
                      },
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
