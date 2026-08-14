import 'package:flutter/material.dart';

/// The shared visual language for the two workspace lists.
///
/// Both lists show the same kind of thing: a named tunnel, where it points, and
/// whether it is up. Keeping the shell, the status tone and the section header
/// in one place is what stops the register and connect lists from drifting into
/// two different-looking pages.

/// What a row's state means, independent of which list it is in.
enum StatusTone { ok, pending, bad, idle }

extension StatusToneColor on StatusTone {
  Color resolve(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final dark = Theme.of(context).brightness == Brightness.dark;
    switch (this) {
      case StatusTone.ok:
        return dark ? const Color(0xFF6BD68A) : const Color(0xFF1B8E4B);
      case StatusTone.pending:
        return dark ? const Color(0xFFE8B457) : const Color(0xFFA96A0B);
      case StatusTone.bad:
        return scheme.error;
      case StatusTone.idle:
        return scheme.onSurfaceVariant;
    }
  }
}

/// A hairline card that sits on the content panel without a drop shadow.
///
/// The previous filled Card read as a grey block against the panel, which is
/// what made the list look pasted on rather than part of the page.
class ListCardShell extends StatelessWidget {
  const ListCardShell({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      margin: const EdgeInsets.only(bottom: 10),
      decoration: BoxDecoration(
        color: theme.brightness == Brightness.dark
            ? scheme.surfaceContainer
            : scheme.surfaceContainerLow,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: scheme.outlineVariant.withValues(alpha: 0.6)),
      ),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
        child: child,
      ),
    );
  }
}

/// The one primary action on a row, sized so rows line up down the list.
///
/// Filled to start something, outlined to stop it: the destructive direction is
/// the quieter of the two, which is the opposite of the old card where stopping
/// was a solid red block.
ButtonStyle listCardActionStyle(BuildContext context, {required bool filled}) {
  final theme = Theme.of(context);
  final base = ButtonStyle(
    padding: const WidgetStatePropertyAll(EdgeInsets.zero),
    minimumSize: const WidgetStatePropertyAll(Size(0, 30)),
    textStyle: WidgetStatePropertyAll(
      theme.textTheme.labelMedium?.copyWith(fontWeight: FontWeight.w600),
    ),
    shape: WidgetStatePropertyAll(
      RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
    ),
  );
  if (filled) return base;
  return base.copyWith(
    side: WidgetStatePropertyAll(
      BorderSide(color: theme.colorScheme.outlineVariant),
    ),
    foregroundColor: WidgetStatePropertyAll(theme.colorScheme.onSurface),
  );
}

/// A secondary action on a row. Quiet by default; only destructive ones carry
/// colour, and only once hovered.
class ListCardIconAction extends StatelessWidget {
  const ListCardIconAction({
    super.key,
    required this.icon,
    required this.tooltip,
    required this.onPressed,
    this.danger = false,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;

    return IconButton(
      onPressed: onPressed,
      icon: Icon(icon, size: 18),
      tooltip: tooltip,
      visualDensity: VisualDensity.compact,
      style: ButtonStyle(
        padding: const WidgetStatePropertyAll(EdgeInsets.all(6)),
        minimumSize: const WidgetStatePropertyAll(Size(30, 30)),
        foregroundColor: WidgetStateProperty.resolveWith((states) {
          if (danger && states.contains(WidgetState.hovered)) {
            return scheme.error;
          }
          return scheme.onSurfaceVariant;
        }),
      ),
    );
  }
}

/// What a list pane shows when there is nothing in it yet.
///
/// Sits at the top of the pane like a row would, rather than centred in the
/// window, so switching between the panes does not move the content around.
class ListPaneEmpty extends StatelessWidget {
  const ListPaneEmpty({
    super.key,
    required this.icon,
    required this.title,
    required this.hint,
  });

  final IconData icon;
  final String title;
  final String hint;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 34),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(12),
        border: Border.all(
          color: scheme.outlineVariant.withValues(alpha: 0.6),
        ),
      ),
      child: Column(
        children: [
          Icon(icon, size: 30, color: scheme.onSurfaceVariant),
          const SizedBox(height: 12),
          Text(
            title,
            style: theme.textTheme.titleSmall?.copyWith(
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(height: 4),
          Text(
            hint,
            textAlign: TextAlign.center,
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

/// A quiet label for one fact about a row: its address, its protocol.
///
/// These were coloured chips with tinted borders, one hue each, which put four
/// competing colours on a row whose only real signal is up or down. Colour is
/// reserved for status now; these carry an icon and read as text.
class ListCardFact extends StatelessWidget {
  const ListCardFact({
    super.key,
    required this.icon,
    required this.label,
    this.onTap,
    this.tooltip,
  });

  final IconData icon;
  final String label;
  final VoidCallback? onTap;
  final String? tooltip;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final content = Padding(
      padding: const EdgeInsets.symmetric(horizontal: 2, vertical: 2),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 14, color: scheme.onSurfaceVariant),
          const SizedBox(width: 5),
          Text(
            label,
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
        ],
      ),
    );

    if (onTap == null) return content;

    return Tooltip(
      message: tooltip ?? '',
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(6),
        child: content,
      ),
    );
  }
}

/// The shape of a list row: a status dot, what the row is, what you can do.
///
/// One line wherever there is room for one. The actions are a fixed ~190px —
/// an 88px button and three icon buttons — so on a phone they left the name and
/// the facts about 100px to fight over, and the facts, which are sized to their
/// text and cannot shrink, simply overflowed. Below [_stackWidth] they drop to
/// their own line and the row becomes two.
///
/// The switch is on the width this row actually gets, not on a global phone
/// breakpoint: the same row also has to survive a narrow desktop window.
class ListCardRow extends StatelessWidget {
  const ListCardRow({
    super.key,
    required this.tone,
    required this.heading,
    required this.facts,
    required this.actions,
  });

  /// Room for the actions plus a readable amount of name and facts.
  static const double _stackWidth = 420;

  final Color tone;
  final Widget heading;
  final List<Widget> facts;
  final List<Widget> actions;

  // Animated: the dot is the row's whole status signal, and a service going
  // from stopped to running should read as a change rather than a repaint.
  Widget _dot({required bool stacked}) => AnimatedContainer(
    duration: const Duration(milliseconds: 260),
    curve: Curves.easeOut,
    width: 8,
    height: 8,
    // Stacked, the dot sits beside the first line of the heading rather than
    // halfway down a two-line block.
    margin: EdgeInsets.only(right: 11, top: stacked ? 6 : 0),
    decoration: BoxDecoration(color: tone, shape: BoxShape.circle),
  );

  @override
  Widget build(BuildContext context) {
    // Wrap, not Row: a fact is sized to its text, so a row of them overflows
    // the moment the panel is narrower than their total.
    final body = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      // Size to the text. Left at max the column fills whatever height it is
      // given, which drags the row's height with it and strands the action
      // halfway down a tall parent.
      mainAxisSize: MainAxisSize.min,
      children: [
        heading,
        const SizedBox(height: 5),
        Wrap(
          spacing: 12,
          runSpacing: 4,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: facts,
        ),
      ],
    );

    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxWidth >= _stackWidth) {
          return Row(
            children: [
              _dot(stacked: false),
              Expanded(child: body),
              const SizedBox(width: 12),
              ...actions,
            ],
          );
        }

        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [_dot(stacked: true), Expanded(child: body)],
            ),
            const SizedBox(height: 10),
            // Trailing, so the primary action keeps the same edge it has on the
            // wide layout instead of jumping to the other side of the row.
            Row(mainAxisAlignment: MainAxisAlignment.end, children: actions),
          ],
        );
      },
    );
  }
}

/// The heading of a list pane: what you are looking at, and how many.
class ListPaneHeader extends StatelessWidget {
  const ListPaneHeader({
    super.key,
    required this.title,
    required this.count,
    required this.onRefresh,
    required this.refreshTooltip,
  });

  final String title;
  final int count;
  final VoidCallback onRefresh;
  final String refreshTooltip;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Padding(
      padding: const EdgeInsets.only(bottom: 14, left: 2),
      child: Row(
        children: [
          Text(
            title,
            style: theme.textTheme.titleMedium?.copyWith(
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(width: 8),
          // The count belongs next to the title, not floating in the list.
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
            decoration: BoxDecoration(
              color: scheme.surfaceContainerHighest,
              borderRadius: BorderRadius.circular(999),
            ),
            child: Text(
              '$count',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
          const Spacer(),
          IconButton(
            onPressed: onRefresh,
            icon: const Icon(Icons.refresh_rounded, size: 19),
            tooltip: refreshTooltip,
            visualDensity: VisualDensity.compact,
            color: scheme.onSurfaceVariant,
          ),
        ],
      ),
    );
  }
}
