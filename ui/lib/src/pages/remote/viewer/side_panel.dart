import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/motion.dart';
import '../../../theme/viewer_palette.dart';
import 'viewer_menus.dart';

/// A viewer menu on a touch screen (ui §9.3): the desktop menu's entries
/// as 48-tall rows, on a full-height sheet that slides in from the trailing
/// edge. A tap outside or Android's Back closes it; a chosen item runs once
/// the sheet is gone. The caller gives the picture its focus back.
Future<void> showViewerSidePanel(
  BuildContext context, {
  required String title,
  required List<ViewerMenuEntry> entries,
}) async {
  final picked = await showGeneralDialog<ViewerMenuItem>(
    context: context,
    barrierDismissible: true,
    barrierLabel: MaterialLocalizations.of(context).modalBarrierDismissLabel,
    barrierColor: const Color(0x52000000),
    transitionDuration: Motion.of(context, Motion.medium),
    pageBuilder: (context, animation, _) {
      final rtl = Directionality.of(context) == TextDirection.rtl;
      return Align(
        alignment: AlignmentDirectional.centerEnd,
        child: SlideTransition(
          position: Tween(begin: Offset(rtl ? -1 : 1, 0), end: Offset.zero)
              .animate(
                CurvedAnimation(
                  parent: animation,
                  curve: Motion.emphasized,
                  reverseCurve: Curves.easeIn,
                ),
              ),
          child: _SidePanel(title: title, entries: entries),
        ),
      );
    },
  );
  if (picked != null) await picked.onSelected();
}

class _SidePanel extends StatelessWidget {
  const _SidePanel({required this.title, required this.entries});

  final String title;
  final List<ViewerMenuEntry> entries;

  @override
  Widget build(BuildContext context) {
    // A trailing danger item ends the session, so it stays in reach below
    // the scrolling rows (ui §9.3); its hairline stands for the divider.
    final split =
        entries.lastIndexWhere((e) => !(e is ViewerMenuItem && e.danger)) + 1;
    final footer = entries.sublist(split);
    final rows = entries.take(split).toList();
    if (footer.isNotEmpty && rows.lastOrNull is ViewerMenuDivider) {
      rows.removeLast();
    }
    return Material(
      color: ViewerPalette.panel,
      child: Container(
        width: 300,
        height: double.infinity,
        decoration: const BoxDecoration(
          border: BorderDirectional(
            start: BorderSide(color: ViewerPalette.edge),
          ),
        ),
        child: SafeArea(
          left: false,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsetsDirectional.fromSTEB(20, 8, 8, 4),
                child: Row(
                  children: [
                    Expanded(
                      child: Semantics(
                        header: true,
                        child: Text(
                          title,
                          style: const TextStyle(
                            fontSize: 15,
                            fontWeight: FontWeight.w600,
                            color: ViewerPalette.text,
                          ),
                        ),
                      ),
                    ),
                    IconButton(
                      tooltip: context.l10n.close,
                      onPressed: () => Navigator.pop(context),
                      icon: const Icon(
                        AppIcons.close,
                        size: AppIconSize.button,
                        color: ViewerPalette.textSecondary,
                      ),
                    ),
                  ],
                ),
              ),
              Expanded(
                child: ListView(
                  padding: const EdgeInsets.only(bottom: 12),
                  children: [for (final entry in rows) _row(context, entry)],
                ),
              ),
              if (footer.isNotEmpty) ...[
                Container(height: 1, color: ViewerPalette.edge),
                for (final entry in footer) _row(context, entry),
              ],
            ],
          ),
        ),
      ),
    );
  }

  Widget _row(BuildContext context, ViewerMenuEntry entry) => switch (entry) {
    ViewerMenuSection(:final label) => _section(label),
    ViewerMenuDivider() => Container(
      height: 1,
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      color: ViewerPalette.edge,
    ),
    final ViewerMenuItem item => _Row(item: item),
    // A submenu lies flat here: a section of its own.
    ViewerMenuSubmenu(:final label, :final note, :final entries) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _section(label, note: note),
        for (final entry in entries) _row(context, entry),
      ],
    ),
  };

  static Widget _section(String label, {String? note}) => Padding(
    padding: const EdgeInsetsDirectional.fromSTEB(20, 14, 20, 4),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          label,
          style: const TextStyle(
            fontSize: 12,
            fontWeight: FontWeight.w600,
            color: ViewerPalette.textQuiet,
          ),
        ),
        if (note != null)
          Text(
            note,
            style: const TextStyle(
              fontSize: 11.5,
              color: ViewerPalette.textQuiet,
            ),
          ),
      ],
    ),
  );
}

class _Row extends StatelessWidget {
  const _Row({required this.item});

  final ViewerMenuItem item;

  @override
  Widget build(BuildContext context) {
    final ink = !item.enabled
        ? ViewerPalette.textQuiet
        : item.danger
        ? ViewerPalette.danger
        : ViewerPalette.text;
    final checked = item.checked;
    final subtitle = item.enabled
        ? item.detail
        : (item.disabledHint ?? item.detail);
    final hint = item.enabled ? item.hint : null;
    final Widget? leading = checked != null
        ? SizedBox.square(
            dimension: AppIconSize.inline,
            child: checked
                ? Icon(AppIcons.check, size: AppIconSize.inline, color: ink)
                : null,
          )
        : item.icon == null
        ? null
        : Icon(item.icon, size: AppIconSize.inline, color: ink);
    return Semantics(
      checked: checked,
      child: InkWell(
        key: item.key,
        onTap: item.enabled ? () => Navigator.pop(context, item) : null,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 48),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 6),
            child: Row(
              children: [
                if (leading != null) ...[leading, const SizedBox(width: 14)],
                Expanded(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        item.label,
                        style: TextStyle(fontSize: 14, color: ink),
                      ),
                      if (subtitle != null)
                        Text(
                          subtitle,
                          style: const TextStyle(
                            fontSize: 12,
                            color: ViewerPalette.textSecondary,
                          ),
                        ),
                    ],
                  ),
                ),
                if (hint != null) ...[
                  const SizedBox(width: 12),
                  Text(
                    hint,
                    style: mono(
                      fontSize: 12,
                      color: ViewerPalette.textSecondary,
                    ),
                  ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }
}
