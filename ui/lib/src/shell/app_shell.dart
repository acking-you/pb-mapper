import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/navigation.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';

/// Whether this window gets the rail and the sheet, or a bottom bar.
///
/// A touch screen keeps the bottom bar until it is properly wide: no phone or
/// tablet puts its navigation on the left, and a landscape phone measures as
/// wide as a small desktop window.
bool usesRail(BuildContext context) {
  final platform = Theme.of(context).platform;
  final touch =
      platform == TargetPlatform.android || platform == TargetPlatform.iOS;
  return MediaQuery.sizeOf(context).width >= (touch ? 900 : 720);
}

/// The app's frame, in the layers of POCOX's design: on a wide window the
/// rail sits straight on the lit backdrop and the page on a rounded sheet
/// beside it; on a compact one the page fills the window above a bottom bar.
class AppShell extends StatelessWidget {
  const AppShell({
    super.key,
    required this.page,
    required this.onSelect,
    required this.child,
  });

  final AppPage page;
  final ValueChanged<AppPage> onSelect;

  /// The page itself, built by the caller.
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final wide = usesRail(context);
    final scheme = Theme.of(context).colorScheme;
    // Each page settles onto its sheet as it arrives, so a switch reads as a
    // change of page rather than a flash.
    final content = EntranceMotion(
      key: ValueKey(page),
      offset: 8,
      child: child,
    );
    if (!wide) {
      return ShellLayout(
        wide: false,
        child: Scaffold(
          backgroundColor: sheetColor(scheme, grouped: page.grouped),
          body: content,
          bottomNavigationBar: NavBar(page: page, onSelect: onSelect),
        ),
      );
    }
    return ShellLayout(
      wide: true,
      child: Scaffold(
        backgroundColor: backdropBase(scheme),
        body: AppBackdrop(
          child: Stack(
            children: [
              SafeArea(
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    SizedBox(
                      width: kRailWidth,
                      child: NavRail(page: page, onSelect: onSelect),
                    ),
                    Expanded(
                      child: Padding(
                        padding: const EdgeInsets.fromLTRB(
                          0,
                          kSheetInset,
                          kSheetInset,
                          kSheetInset,
                        ),
                        child: ContentSheet(
                          grouped: page.grouped,
                          child: content,
                        ),
                      ),
                    ),
                  ],
                ),
              ),
              // The strip above the sheet is window chrome too: it drags.
              if (isFramelessDesktop)
                const Positioned(
                  top: 0,
                  left: kRailWidth,
                  right: 0,
                  height: kSheetInset,
                  child: WindowDragArea(),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
