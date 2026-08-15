import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/widgets/list_card.dart';

/// Text that can be selected and copied, for the values worth copying.
///
/// A service key is 50-odd characters of hex and colons, and a connection id is
/// what you paste into a log search. Rendering those as plain Text meant
/// retyping them by eye.
class CopyableValue extends StatelessWidget {
  const CopyableValue({
    super.key,
    required this.value,
    this.style,
    this.maxLines,
  });

  final String value;
  final TextStyle? style;
  final int? maxLines;

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: context.l10n.copy,
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: () async {
          await Clipboard.setData(ClipboardData(text: value));
          if (!context.mounted) return;
          showToast(context, context.l10n.copiedToClipboard(value));
        },
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 2, vertical: 1),
          child: SelectableText(value, style: style, maxLines: maxLines),
        ),
      ),
    );
  }
}

/// Copies [value], for the places where the text itself must stay selectable
/// and so cannot also be the copy button.
class CopyIconAction extends StatelessWidget {
  const CopyIconAction({super.key, required this.value});

  final String value;

  @override
  Widget build(BuildContext context) {
    return ListCardIconAction(
      icon: Icons.copy_rounded,
      tooltip: context.l10n.copy,
      onPressed: () async {
        await Clipboard.setData(ClipboardData(text: value));
        if (!context.mounted) return;
        showToast(context, context.l10n.copiedToClipboard(value));
      },
    );
  }
}

/// One control connection the server is holding.
///
/// Every field here comes from the protocol's structured status. The section
/// this replaces rendered `format!("{map:?}")` — the same facts, printed as a
/// Rust Debug dump, which said whether a connection existed and nothing about
/// whether it was any good.
class ConnectionRow extends StatelessWidget {
  const ConnectionRow({super.key, required this.conn});

  final ServiceConnInfo conn;

  static String _age(BuildContext context, Duration age) {
    if (age.inSeconds < 1) return '${age.inMilliseconds}ms';
    if (age.inMinutes < 1) return '${age.inSeconds}s';
    if (age.inHours < 1) return '${age.inMinutes}m';
    return '${age.inHours}h';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tone = (conn.healthy ? StatusTone.ok : StatusTone.bad).resolve(
      context,
    );

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 5),
      child: Row(
        children: [
          Container(
            width: 7,
            height: 7,
            margin: const EdgeInsets.only(right: 10),
            decoration: BoxDecoration(color: tone, shape: BoxShape.circle),
          ),
          CopyableValue(
            value: '#${conn.connId}',
            style: theme.textTheme.bodyMedium?.copyWith(
              fontWeight: FontWeight.w600,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Wrap(
              spacing: 12,
              runSpacing: 2,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                ListCardFact(
                  icon: Icons.schedule_rounded,
                  label: context.l10n.connLastSeen(
                    _age(context, conn.lastRxAge),
                  ),
                ),
                ListCardFact(
                  icon: Icons.layers_outlined,
                  label: context.l10n.connGeneration('${conn.generation}'),
                ),
                ListCardFact(
                  icon: Icons.tag_rounded,
                  label: context.l10n.connProtocol('${conn.protocolVersion}'),
                ),
                if (!conn.healthy)
                  Text(
                    context.l10n.connUnhealthy,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: scheme.error,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// Connection ids as chips rather than a `list:[1, 2, 3]` string.
///
/// The server sends these as a hand-formatted line, so they are parsed
/// defensively: anything that does not look like the expected shape falls back
/// to showing the raw text rather than silently dropping it.
class ConnectionIdChips extends StatelessWidget {
  const ConnectionIdChips({super.key, required this.raw, required this.label});

  final String raw;
  final String label;

  /// Pulls the ids out of `... list:[1, 2, 3] ...`.
  static List<int>? parseIds(String raw) {
    final match = RegExp(r'list:\s*\[([^\]]*)\]').firstMatch(raw);
    if (match == null) return null;
    final body = match.group(1)?.trim() ?? '';
    if (body.isEmpty) return const [];
    final ids = <int>[];
    for (final part in body.split(',')) {
      final n = int.tryParse(part.trim());
      if (n == null) return null;
      ids.add(n);
    }
    return ids;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final ids = parseIds(raw);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text(
              label,
              style: theme.textTheme.labelLarge?.copyWith(
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(width: 8),
            if (ids != null)
              Text(
                '${ids.length}',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
          ],
        ),
        const SizedBox(height: 6),
        if (ids == null)
          // Not the shape we expected. Showing it raw is worse than chips but
          // far better than pretending there is nothing here.
          SelectableText(
            raw.trim(),
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          )
        else if (ids.isEmpty)
          Text(
            context.l10n.noConnectionIds,
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          )
        else
          Wrap(
            spacing: 6,
            runSpacing: 6,
            children: [
              for (final id in ids)
                CopyableValue(
                  value: '#$id',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
            ],
          ),
      ],
    );
  }
}
