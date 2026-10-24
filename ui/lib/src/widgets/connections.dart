import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

/// What the relay holds for one key, asked for only when opened: the
/// protocol's structured `Service` query, one row per control connection with
/// whether it is healthy and when it last spoke.
class ConnectionsDisclosure extends StatefulWidget {
  const ConnectionsDisclosure({
    super.key,
    required this.api,
    required this.serviceKey,
  });

  final PbMapperApiClient api;
  final String serviceKey;

  @override
  State<ConnectionsDisclosure> createState() => _ConnectionsDisclosureState();
}

class _ConnectionsDisclosureState extends State<ConnectionsDisclosure> {
  bool _open = false;

  /// Null until asked: "not asked yet" is not "none held".
  List<ServiceConnInfo>? _conns;
  bool _loading = false;

  Future<void> _toggle() async {
    setState(() => _open = !_open);
    if (_open) await _load();
  }

  Future<void> _load() async {
    if (_loading) return;
    setState(() => _loading = true);
    List<ServiceConnInfo> conns;
    try {
      conns = await widget.api.getServiceConns(widget.serviceKey);
    } catch (_) {
      conns = const [];
    }
    if (!mounted) return;
    setState(() {
      _conns = conns;
      _loading = false;
    });
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final conns = _conns;
    final header = InkWell(
      key: Key('conns-${widget.serviceKey}'),
      borderRadius: BorderRadius.circular(8),
      onTap: _toggle,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 4, horizontal: 2),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            AnimatedRotation(
              turns: _open ? 0.25 : 0,
              duration: Motion.of(context, Motion.fast),
              child: Icon(
                AppIcons.chevronRight,
                size: 15,
                color: scheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(width: 4),
            Text(
              l10n.connsTitle,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontWeight: FontWeight.w500,
              ),
            ),
            if (conns != null) ...[
              const SizedBox(width: 6),
              Text(
                '${conns.length}',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: onSurfaceMuted(scheme),
                ),
              ),
            ],
          ],
        ),
      ),
    );
    Widget body;
    if (!_open) {
      body = const SizedBox(width: double.infinity);
    } else if (_loading && conns == null) {
      body = const Padding(
        padding: EdgeInsets.symmetric(vertical: 10),
        child: SizedBox.square(
          dimension: 16,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    } else if ((conns ?? const []).isEmpty) {
      body = Padding(
        padding: const EdgeInsets.fromLTRB(20, 4, 0, 6),
        child: Text(
          l10n.connsNone,
          style: theme.textTheme.bodySmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
      );
    } else {
      body = Padding(
        padding: const EdgeInsets.only(left: 20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [for (final conn in conns!) ConnectionRow(conn: conn)],
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        header,
        Motion.reduced(context)
            ? body
            : AnimatedSize(
                duration: Motion.medium,
                curve: Motion.move,
                alignment: Alignment.topLeft,
                child: body,
              ),
      ],
    );
  }
}

/// One control connection the relay holds.
class ConnectionRow extends StatelessWidget {
  const ConnectionRow({super.key, required this.conn});

  final ServiceConnInfo conn;

  static String _age(Duration age) {
    if (age.inSeconds < 1) return '${age.inMilliseconds}ms';
    if (age.inMinutes < 1) return '${age.inSeconds}s';
    if (age.inHours < 1) return '${age.inMinutes}m';
    return '${age.inHours}h';
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = mono(fontSize: 11.5, color: scheme.onSurfaceVariant);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Wrap(
        spacing: 12,
        runSpacing: 2,
        crossAxisAlignment: WrapCrossAlignment.center,
        children: [
          Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              StatusDot(
                color: healthColor(
                  scheme,
                  conn.healthy ? TunnelHealth.running : TunnelHealth.failed,
                ),
                size: 7,
              ),
              const SizedBox(width: 8),
              SelectableText(
                '#${conn.connId}',
                style: mono(
                  fontSize: 12.5,
                  fontWeight: FontWeight.w600,
                  color: scheme.onSurface,
                ),
              ),
            ],
          ),
          Text(l10n.connLastSeen(_age(conn.lastRxAge)), style: muted),
          Text(l10n.connGeneration('${conn.generation}'), style: muted),
          Text(l10n.connProtocol('${conn.protocolVersion}'), style: muted),
          if (!conn.healthy)
            Text(
              l10n.connUnhealthy,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.error,
                fontWeight: FontWeight.w600,
              ),
            ),
        ],
      ),
    );
  }
}
