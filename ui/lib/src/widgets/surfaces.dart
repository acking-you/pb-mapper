import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';

/// A white plate raised off a grouped page by a hairline and a soft lift. A
/// Material rather than a decorated box, so the rows inside it can show their
/// hover and ink.
class RaisedSurface extends StatelessWidget {
  const RaisedSurface({
    super.key,
    required this.child,
    this.radius = kPanelRadius,
  });

  final Widget child;
  final double radius;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final shape = RoundedRectangleBorder(
      borderRadius: BorderRadius.circular(radius),
      side: BorderSide(color: raisedCardEdge(scheme)),
    );
    return DecoratedBox(
      decoration: ShapeDecoration(
        shape: shape,
        shadows: raisedCardShadows(scheme),
      ),
      child: Material(
        color: raisedCardColor(scheme),
        shape: shape,
        clipBehavior: Clip.antiAlias,
        child: child,
      ),
    );
  }
}

/// A white card raised off a grouped page, with an optional title row, so
/// groups read as calm plates rather than boxes.
class GroupCard extends StatelessWidget {
  const GroupCard({
    super.key,
    required this.child,
    this.title,
    this.icon,
    this.trailing,
    this.padding = const EdgeInsets.fromLTRB(18, 16, 18, 18),
  });

  final Widget child;
  final String? title;
  final IconData? icon;

  /// Beside the title: a count, a small action.
  final Widget? trailing;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return RaisedSurface(
      radius: kCardRadius,
      child: Padding(
        padding: padding,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            if (title != null) ...[
              Row(
                children: [
                  if (icon != null) ...[
                    Icon(icon, size: 18, color: scheme.onSurfaceVariant),
                    const SizedBox(width: 10),
                  ],
                  Expanded(
                    child: Text(title!, style: theme.textTheme.titleMedium),
                  ),
                  ?trailing,
                ],
              ),
              const SizedBox(height: 14),
            ],
            child,
          ],
        ),
      ),
    );
  }
}

/// A heading over a group of cards on a page: its name, how many, and an
/// optional quiet action at the end.
class SectionHeader extends StatelessWidget {
  const SectionHeader({
    super.key,
    required this.title,
    this.count,
    this.action,
  });

  final String title;
  final int? count;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 22, 0, 10),
      // The title and its count share what the action leaves, so the action
      // sits at the cards' edge however long the title is.
      child: Row(
        children: [
          Expanded(
            child: Row(
              children: [
                Flexible(
                  child: Semantics(
                    header: true,
                    child: Text(
                      title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.titleSmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                        fontSize: 13.5,
                      ),
                    ),
                  ),
                ),
                if (count != null) ...[
                  const SizedBox(width: 8),
                  Container(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 7,
                      vertical: 1,
                    ),
                    decoration: BoxDecoration(
                      color: softWash(scheme, strength: 1.4),
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
                ],
              ],
            ),
          ),
          ?action,
        ],
      ),
    );
  }
}

/// A calm empty state set where the first row would be, so a page does not
/// jump when its first item arrives.
class EmptyNote extends StatelessWidget {
  const EmptyNote({
    super.key,
    required this.icon,
    required this.text,
    this.action,
  });

  final IconData icon;
  final String text;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      padding: const EdgeInsets.fromLTRB(18, 18, 14, 18),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(kCardRadius),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Row(
        children: [
          Icon(icon, size: 20, color: onSurfaceMuted(scheme)),
          const SizedBox(width: 14),
          Expanded(
            child: Text(
              text,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontSize: 13,
              ),
            ),
          ),
          if (action != null) ...[const SizedBox(width: 10), action!],
        ],
      ),
    );
  }
}

/// Width at or above which a transient panel is a centred dialog rather than
/// a bottom sheet — a sheet rises from the edge the thumb is near, which on a
/// desktop window puts it as far from the pointer as it can be.
const double kDialogPanelWidth = 720;

/// Show [builder] as the right kind of transient panel for the window. Both
/// forms are height-bounded and scroll; the sheet must be scroll-controlled or
/// Flutter caps it at 9/16 of the screen and clips the rest.
Future<T?> showAdaptivePanel<T>({
  required BuildContext context,
  required WidgetBuilder builder,
  double maxWidth = 480,
}) {
  final size = MediaQuery.sizeOf(context);
  if (size.width < kDialogPanelWidth) {
    return showModalBottomSheet<T>(
      context: context,
      isScrollControlled: true,
      constraints: BoxConstraints(maxHeight: size.height * 0.92),
      builder: (c) => SafeArea(
        child: Padding(
          padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(c).bottom),
          child: SingleChildScrollView(child: builder(c)),
        ),
      ),
    );
  }
  return showDialog<T>(
    context: context,
    builder: (c) => Dialog(
      clipBehavior: Clip.antiAlias,
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxWidth: maxWidth,
          maxHeight: size.height * 0.86,
        ),
        child: SingleChildScrollView(child: builder(c)),
      ),
    ),
  );
}

/// Ask before something that cannot be undone. True only on an explicit yes.
Future<bool> confirmDestructive(
  BuildContext context, {
  required String title,
  required String body,
  required String action,
}) async {
  final scheme = Theme.of(context).colorScheme;
  final yes = await showDialog<bool>(
    context: context,
    builder: (c) => AlertDialog(
      icon: Icon(AppIcons.warning, color: scheme.error),
      title: Text(title),
      content: Text(body),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(c).pop(false),
          child: Text(c.l10n.cancel),
        ),
        FilledButton(
          key: const Key('confirm-destructive'),
          style: FilledButton.styleFrom(
            backgroundColor: scheme.error,
            foregroundColor: scheme.onError,
          ),
          onPressed: () => Navigator.of(c).pop(true),
          child: Text(action),
        ),
      ],
    ),
  );
  return yes ?? false;
}
