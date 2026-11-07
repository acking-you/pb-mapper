import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/shell/sharing_indicator.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';

/// Which layout a page is being shown in, for the pieces that differ: a title
/// bar inside the rounded sheet beside the rail, or across the top of the
/// window above a bottom bar.
class ShellLayout extends InheritedWidget {
  const ShellLayout({super.key, required this.wide, required super.child});

  final bool wide;

  static bool wideOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<ShellLayout>()?.wide ?? false;

  @override
  bool updateShouldNotify(ShellLayout old) => old.wide != wide;
}

/// A page's main action in the title bar: labelled when the bar has room,
/// a filled icon button carrying the label as its tooltip when it does not.
class PrimaryAction {
  const PrimaryAction({
    required this.key,
    required this.icon,
    required this.label,
    required this.onPressed,
  });

  final Key key;
  final IconData icon;
  final String label;
  final VoidCallback onPressed;
}

/// A page: its title bar and its body. The bar is window chrome too — its
/// empty space drags the window, and on Windows and Linux it carries the
/// caption buttons.
class PageFrame extends StatelessWidget {
  const PageFrame({
    super.key,
    required this.title,
    required this.body,
    this.actions = const [],
    this.primary,
    this.overlay = false,
    this.onBack,
    this.sharingIndicator = true,
  });

  final String title;
  final Widget body;

  /// Quiet actions for this page, before [primary].
  final List<Widget> actions;
  final PrimaryAction? primary;

  /// Let [body] run under the bar, for a page that is one picture (the
  /// galaxy): the bar floats over it instead of cutting a strip off the top.
  final bool overlay;

  /// A back button at the start of the bar, for a page reached from
  /// another rather than from the navigation.
  final VoidCallback? onBack;

  /// Whether the bar carries the capsule shown while this computer is
  /// viewed (D15). A page that offers the same controls itself turns it
  /// off.
  final bool sharingIndicator;

  /// The bar's height, for a body that lays things out beneath it.
  static double barHeight(BuildContext context) =>
      ShellLayout.wideOf(context) ? 58 : 54 + MediaQuery.paddingOf(context).top;

  @override
  Widget build(BuildContext context) {
    final wide = ShellLayout.wideOf(context);
    final bar = _TitleBar(
      title: title,
      actions: actions,
      primary: primary,
      inSheet: wide,
      onBack: onBack,
      sharingIndicator: sharingIndicator,
    );
    if (overlay) {
      return Stack(
        children: [
          Positioned.fill(child: body),
          Positioned(top: 0, left: 0, right: 0, child: bar),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        bar,
        Expanded(child: body),
      ],
    );
  }
}

/// The bar width from which the primary action carries its label: a 560
/// window across the top of a phone.
const double _labelledPrimaryWidth = 560;

class _TitleBar extends StatelessWidget {
  const _TitleBar({
    required this.title,
    required this.actions,
    required this.primary,
    required this.inSheet,
    required this.onBack,
    required this.sharingIndicator,
  });

  final String title;
  final List<Widget> actions;
  final PrimaryAction? primary;
  final bool inSheet;
  final VoidCallback? onBack;
  final bool sharingIndicator;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final height = inSheet ? 58.0 : 54.0;
    final onBack = this.onBack;
    // Across the top of the window the traffic lights sit at its corner;
    // inside the sheet the rail has already made room for them. A back
    // button brings its own margin.
    final lead =
        (inSheet ? 0 : trafficLightInset) +
        (inSheet ? 22.0 : 16.0) -
        (onBack == null ? 0 : 12);
    final primary = this.primary;
    final bar = SizedBox(
      height: height,
      child: Stack(
        // Centred: a bar with no actions is only as tall as its title.
        alignment: AlignmentDirectional.centerStart,
        children: [
          const Positioned.fill(child: WindowDragArea()),
          // The capsule's form and the primary action's label follow the
          // whole bar's width (beside the rail, the sheet's), not the
          // window's and not the padded row's: a narrow window's sheet has
          // no room for both the label and the capsule.
          LayoutBuilder(
            builder: (context, box) => Padding(
              padding: EdgeInsetsDirectional.only(
                start: lead,
                end: drawsCaptionButtons ? kCaptionButtonsWidth + 16 : 8,
              ),
              child: Row(
                children: [
                  if (onBack != null) ...[
                    IconButton(
                      key: const Key('page-back'),
                      tooltip: context.l10n.back,
                      onPressed: onBack,
                      icon: const Icon(AppIcons.back, size: AppIconSize.button),
                    ),
                    const SizedBox(width: 4),
                  ],
                  Expanded(
                    child: IgnorePointer(
                      child: Text(
                        title,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.titleLarge?.copyWith(
                          fontSize: inSheet ? 19 : 18,
                        ),
                      ),
                    ),
                  ),
                  if (sharingIndicator) ...[
                    const SizedBox(width: 12),
                    SharingIndicator(form: sharingFormFor(box.maxWidth)),
                  ],
                  ...actions,
                  if (primary != null) ...[
                    const SizedBox(width: 6),
                    if (box.maxWidth >= _labelledPrimaryWidth)
                      FilledButton.icon(
                        key: primary.key,
                        onPressed: primary.onPressed,
                        style: FilledButton.styleFrom(
                          minimumSize: const Size(0, 38),
                          padding: const EdgeInsetsDirectional.fromSTEB(
                            14,
                            0,
                            18,
                            0,
                          ),
                        ),
                        icon: Icon(primary.icon, size: 18),
                        label: Text(primary.label),
                      )
                    else
                      IconButton.filled(
                        key: primary.key,
                        tooltip: primary.label,
                        onPressed: primary.onPressed,
                        style: IconButton.styleFrom(
                          backgroundColor: theme.colorScheme.primary,
                          foregroundColor: theme.colorScheme.onPrimary,
                        ),
                        icon: Icon(primary.icon),
                      ),
                  ],
                ],
              ),
            ),
          ),
          // The bar's top-right corner is the window's in every layout that
          // draws them: the sheet runs to the window's edges, and a compact
          // page fills it.
          if (drawsCaptionButtons)
            const Positioned(top: 0, right: 0, child: CaptionButtons()),
        ],
      ),
    );
    if (inSheet) return bar;
    return SafeArea(bottom: false, child: bar);
  }
}
