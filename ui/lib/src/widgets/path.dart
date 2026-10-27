/// Which way a tunnel's traffic goes — straight to the other computer, or
/// through the relay — as a chip, as a small route, and in words.
library;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';

/// [tunnel]'s path as a pill: an icon and a word, with the reason on hover.
/// Nothing for a tunnel that is not up.
class PathChip extends StatelessWidget {
  const PathChip({super.key, required this.tunnel, this.shrinks = false});

  final Tunnel tunnel;

  /// Ellipsize the label when room runs out; see [StatusChip.shrinks].
  final bool shrinks;

  @override
  Widget build(BuildContext context) {
    final mode = tunnel.pathMode;
    if (mode == null) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    final color = pathColor(scheme, mode);
    final text = Text(
      pathLabel(context, tunnel),
      maxLines: 1,
      overflow: shrinks ? TextOverflow.ellipsis : null,
      style: TextStyle(
        fontSize: 12,
        fontWeight: FontWeight.w500,
        color: scheme.onSurfaceVariant,
      ),
    );
    Widget icon = Icon(pathIcon(mode), size: 13, color: color);
    if (mode == PathMode.trying) icon = _Breathing(child: icon);
    return Tooltip(
      message: pathDetail(context, tunnel),
      child: Container(
        key: Key('path-${tunnel.id}'),
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(
          color: color.withValues(alpha: 0.13),
          borderRadius: BorderRadius.circular(20),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            icon,
            const SizedBox(width: 5),
            shrinks ? Flexible(child: text) : text,
          ],
        ),
      ),
    );
  }
}

/// The route [tunnel]'s traffic takes, drawn: this computer and the service
/// at either end, and the relay between them — on the line when traffic
/// crosses it, set aside when it goes direct.
class PathRoute extends StatelessWidget {
  const PathRoute({super.key, required this.tunnel});

  final Tunnel tunnel;

  @override
  Widget build(BuildContext context) {
    final mode = tunnel.pathMode;
    if (mode == null) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    final direct = switch (mode) {
      PathMode.direct || PathMode.mixed || PathMode.trying => true,
      _ => false,
    };
    final throughRelay = switch (mode) {
      PathMode.direct || PathMode.trying => false,
      _ => true,
    };
    final muted = scheme.onSurfaceVariant;
    final signal = signalColor(scheme);
    Widget end(IconData icon) => Icon(icon, size: 18, color: muted);
    return SizedBox(
      key: Key('route-${tunnel.id}'),
      height: 34,
      child: Row(
        children: [
          end(
            tunnel.kind == TunnelKind.share
                ? AppIcons.service
                : AppIcons.computer,
          ),
          const SizedBox(width: 6),
          Expanded(
            child: CustomPaint(
              painter: _RoutePainter(
                direct: direct ? signal : null,
                relay: throughRelay ? scheme.outline : null,
                dashed: mode == PathMode.trying,
              ),
              child: Align(
                // On the line when traffic crosses it, lifted off when not.
                alignment: throughRelay
                    ? Alignment.center
                    : const Alignment(0, 0.95),
                child: Icon(
                  AppIcons.relay,
                  size: 14,
                  color: throughRelay ? muted : muted.withValues(alpha: 0.45),
                ),
              ),
            ),
          ),
          const SizedBox(width: 6),
          end(
            tunnel.kind == TunnelKind.share
                ? AppIcons.computer
                : AppIcons.service,
          ),
        ],
      ),
    );
  }
}

class _RoutePainter extends CustomPainter {
  _RoutePainter({this.direct, this.relay, this.dashed = false});

  /// The direct line's colour, when there is one: an arc over the relay.
  final Color? direct;

  /// The relayed line's colour, when there is one: straight through it.
  final Color? relay;
  final bool dashed;

  @override
  void paint(Canvas canvas, Size size) {
    final mid = size.height / 2;
    if (relay case final color?) {
      // Into the relay and out again, leaving it room on the line.
      final paint = Paint()
        ..color = color
        ..strokeWidth = 1.2;
      final gap = size.width / 2 - 12;
      canvas
        ..drawLine(Offset(0, mid), Offset(gap, mid), paint)
        ..drawLine(
          Offset(size.width - gap, mid),
          Offset(size.width, mid),
          paint,
        );
    }
    if (direct case final color?) {
      final lift = relay == null ? mid - 2 : mid - 10;
      final path = Path()
        ..moveTo(0, mid)
        ..quadraticBezierTo(size.width / 2, mid - lift * 2, size.width, mid);
      final paint = Paint()
        ..color = color
        ..style = PaintingStyle.stroke
        ..strokeCap = StrokeCap.round
        ..strokeWidth = 1.8;
      if (!dashed) {
        canvas.drawPath(path, paint);
        return;
      }
      for (final metric in path.computeMetrics()) {
        for (var at = 0.0; at < metric.length; at += 7) {
          canvas.drawPath(metric.extractPath(at, at + 3.5), paint);
        }
      }
    }
  }

  @override
  bool shouldRepaint(_RoutePainter old) =>
      old.direct != direct || old.relay != relay || old.dashed != dashed;
}

/// Fades its child in and out, for an attempt in flight. Still under
/// reduced motion.
class _Breathing extends StatefulWidget {
  const _Breathing({required this.child});

  final Widget child;

  @override
  State<_Breathing> createState() => _BreathingState();
}

class _BreathingState extends State<_Breathing>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 900),
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (Motion.reduced(context)) {
      _c
        ..stop()
        ..value = 1;
    } else if (!_c.isAnimating) {
      _c.repeat(reverse: true);
    }
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => FadeTransition(
    opacity: Tween(begin: 0.35, end: 1.0).animate(_c),
    child: widget.child,
  );
}

/// The colour of a path: the signal blue for a path of its own, past the
/// relay, as live tunnels are drawn; a neutral grey for the relay.
Color pathColor(ColorScheme scheme, PathMode mode) => switch (mode) {
  PathMode.direct || PathMode.mixed || PathMode.trying => signalColor(scheme),
  PathMode.relay ||
  PathMode.relayOnly ||
  PathMode.directFirst => scheme.outline,
};

IconData pathIcon(PathMode mode) => switch (mode) {
  PathMode.direct || PathMode.trying || PathMode.directFirst => AppIcons.direct,
  PathMode.mixed => AppIcons.mixedPath,
  PathMode.relay || PathMode.relayOnly => AppIcons.relay,
};

/// Whether [mode] sends traffic through the relay, where the carrier
/// matters: a direct path is its own QUIC, past the relay.
bool _crossesRelay(PathMode? mode) => switch (mode) {
  PathMode.relay || PathMode.relayOnly || PathMode.mixed => true,
  _ => false,
};

/// What [tunnel]'s path is called, in a word or two; with the carrier when
/// that is QUIC, TCP being what a relay is reached over unless said.
String pathLabel(BuildContext context, Tunnel tunnel) {
  final label = _pathLabel(context, tunnel);
  return tunnel.overQuic && _crossesRelay(tunnel.pathMode)
      ? '$label · ${RelayTransport.quic.label}'
      : label;
}

String _pathLabel(BuildContext context, Tunnel tunnel) {
  final l10n = context.l10n;
  final path = tunnel.path;
  return switch (tunnel.pathMode) {
    PathMode.relayOnly => l10n.pathRelayOnly,
    PathMode.directFirst => l10n.pathDirectFirst,
    PathMode.trying => l10n.pathTrying,
    PathMode.direct when tunnel.kind == TunnelKind.share =>
      l10n.pathShareDirect(path?.activeDirect ?? 0),
    PathMode.direct => l10n.pathDirect,
    PathMode.relay => l10n.pathRelay,
    PathMode.mixed => l10n.pathMixed(
      path?.activeDirect ?? 0,
      path?.activeRelay ?? 0,
    ),
    null => '',
  };
}

/// What [tunnel]'s path means, and when it is the relay after all, why.
String pathDetail(BuildContext context, Tunnel tunnel) {
  final detail = _pathDetail(context, tunnel);
  return tunnel.overQuic && _crossesRelay(tunnel.pathMode)
      ? '$detail ${context.l10n.pathOverQuic}'
      : detail;
}

String _pathDetail(BuildContext context, Tunnel tunnel) {
  final l10n = context.l10n;
  final path = tunnel.path;
  final share = tunnel.kind == TunnelKind.share;
  switch (tunnel.pathMode) {
    case PathMode.relayOnly:
      return share ? l10n.pathRelayOnlyShareDetail : l10n.pathRelayOnlyDetail;
    case PathMode.directFirst:
      return l10n.pathDirectFirstDetail;
    case PathMode.trying:
      return l10n.pathTryingDetail(_stage(context, path?.attempt));
    case PathMode.direct when share:
      return l10n.pathShareDirectDetail(path?.activeDirect ?? 0);
    case PathMode.direct:
      final setup = path?.setupMs;
      return setup == null
          ? l10n.pathDirectDetail
          : '${l10n.pathDirectDetail} ${l10n.pathSetup(setup)}';
    case PathMode.mixed:
      return l10n.pathMixedDetail(
        path?.activeDirect ?? 0,
        path?.activeRelay ?? 0,
      );
    case PathMode.relay:
      final reason = switch (path?.fallbackStage) {
        DirectStage.negotiating => l10n.pathWhyRelayHasNoDirect,
        DirectStage.discovering => l10n.pathWhyShareDeclines,
        DirectStage.punching => l10n.pathWhyNetworks,
        DirectStage.committing => l10n.pathWhyClosed,
        null => null,
      };
      return reason == null
          ? l10n.pathRelayDetail
          : '${l10n.pathRelayBecause(reason)} ${l10n.pathRetryLater}';
    case null:
      return '';
  }
}

String _stage(BuildContext context, DirectStage? stage) {
  final l10n = context.l10n;
  return switch (stage) {
    DirectStage.negotiating || null => l10n.pathStageNegotiating,
    DirectStage.discovering => l10n.pathStageDiscovering,
    DirectStage.punching => l10n.pathStagePunching,
    DirectStage.committing => l10n.pathStageCommitting,
  };
}
