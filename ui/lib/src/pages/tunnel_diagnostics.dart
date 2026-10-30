/// A running tunnel's diagnostics, opened from its menu and from the galaxy's
/// details panel: how it reaches the relay, how its sessions travel, its port
/// mapping, and what happened to it, as it happens. The whole of it copies as
/// one JSON report.
library;

import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/widgets/carrier.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

Future<void> openTunnelDiagnostics(
  BuildContext context,
  AppStore store,
  Tunnel tunnel,
) => showAdaptivePanel<void>(
  context: context,
  maxWidth: 560,
  builder: (_) => TunnelDiagnosticsPanel(store: store, tunnel: tunnel),
);

/// How many events the panel lists, newest first.
const int _kept = 200;

class TunnelDiagnosticsPanel extends StatefulWidget {
  const TunnelDiagnosticsPanel({
    super.key,
    required this.store,
    required this.tunnel,
  });

  final AppStore store;
  final Tunnel tunnel;

  @override
  State<TunnelDiagnosticsPanel> createState() => _TunnelDiagnosticsPanelState();
}

class _TunnelDiagnosticsPanelState extends State<TunnelDiagnosticsPanel> {
  late final StreamSubscription<TunnelEventsUpdate> _subscription;

  /// Ages are said relative to now, so they are said again now and then.
  late final Timer _clock;
  TunnelDiagnostics? _diagnostics;
  final List<_Received> _events = [];
  int _missed = 0;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _subscription = widget.store.api
        .tunnelEvents(
          serviceKey: widget.tunnel.key,
          share: widget.tunnel.kind == TunnelKind.share,
        )
        .listen(
          _receive,
          onError: (Object error) {
            if (mounted) setState(() => _error = error);
          },
        );
    _clock = Timer.periodic(const Duration(seconds: 15), (_) {
      if (mounted) setState(() {});
    });
  }

  void _receive(TunnelEventsUpdate update) {
    if (!mounted) return;
    setState(() {
      _diagnostics = update.diagnostics ?? _diagnostics;
      _missed += update.missed;
      final event = update.event;
      if (event != null &&
          (_events.isEmpty || event.seq > _events.last.event.seq)) {
        _events.add(_Received(event, DateTime.now().subtract(event.age)));
        if (_events.length > _kept) _events.removeAt(0);
      }
    });
  }

  @override
  void dispose() {
    _clock.cancel();
    unawaited(_subscription.cancel());
    super.dispose();
  }

  Future<void> _copy() async {
    final report = {
      'tunnel': {'kind': widget.tunnel.kind.name, 'key': widget.tunnel.key},
      'diagnostics': _diagnostics?.raw,
      'events': [for (final received in _events) received.event.raw],
      if (_missed > 0) 'missed': _missed,
    };
    await Clipboard.setData(
      ClipboardData(text: const JsonEncoder.withIndent('  ').convert(report)),
    );
    if (!mounted) return;
    showToast(
      context,
      context.l10n.diagnosticsCopied,
      kind: ToastKind.success,
    );
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final diagnostics = _diagnostics;
    final shown = [
      for (final received in _events.reversed)
        if (!_quiet.contains(received.event.kind)) received,
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(22, 18, 14, 22),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          ListenableBuilder(
            listenable: widget.store,
            builder: (context, _) => _Header(
              tunnel:
                  widget.store.find(widget.tunnel.kind, widget.tunnel.key) ??
                  widget.tunnel,
              onCopy: diagnostics == null ? null : _copy,
            ),
          ),
          const SizedBox(height: 18),
          Padding(
            padding: const EdgeInsetsDirectional.only(end: 8),
            child: switch ((diagnostics, _error)) {
              (final TunnelDiagnostics diagnostics, _) => Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                mainAxisSize: MainAxisSize.min,
                children: [
                  _Facts(tunnel: widget.tunnel, diagnostics: diagnostics),
                  const SizedBox(height: 16),
                  _Heading(l10n.portMapping),
                  _PortMapping(
                    tunnel: widget.tunnel,
                    mapping: diagnostics.portMapping,
                  ),
                  const SizedBox(height: 20),
                  _Heading(l10n.diagnosticsEvents, count: shown.length),
                  if (_missed > 0) _Missed(count: _missed),
                  if (shown.isEmpty)
                    EmptyNote(
                      icon: AppIcons.diagnostics,
                      text: l10n.diagnosticsNoEvents,
                    )
                  else
                    for (final received in shown)
                      _EventRow(
                        key: Key('tunnel-event-${received.event.seq}'),
                        received: received,
                      ),
                ],
              ),
              (null, final Object _) => EmptyNote(
                key: const Key('diagnostics-unavailable'),
                icon: AppIcons.info,
                text: l10n.diagnosticsUnavailable,
              ),
              (null, null) => const Padding(
                padding: EdgeInsets.symmetric(vertical: 28),
                child: Center(
                  child: SizedBox.square(
                    dimension: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                ),
              ),
            },
          ),
        ],
      ),
    );
  }
}

/// Events the panel does not list: one per session, which the session
/// figures already count.
const _quiet = {'session_opened', 'session_closed'};

class _Received {
  _Received(this.event, this.at);

  final TunnelEvent event;

  /// When it happened, by this computer's clock.
  final DateTime at;
}

class _Header extends StatelessWidget {
  const _Header({required this.tunnel, required this.onCopy});

  final Tunnel tunnel;
  final VoidCallback? onCopy;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Row(
      children: [
        PlanetGlyph(color: healthColor(scheme, tunnel.health), size: 34),
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(l10n.tunnelDiagnostics, style: theme.textTheme.titleMedium),
              const SizedBox(height: 2),
              Text.rich(
                TextSpan(
                  children: [
                    TextSpan(
                      text: tunnel.kind == TunnelKind.share
                          ? l10n.kindShare
                          : l10n.kindConnect,
                    ),
                    const TextSpan(text: ' · '),
                    TextSpan(text: tunnel.key, style: mono(fontSize: 12)),
                  ],
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
        IconButton(
          key: const Key('diagnostics-copy'),
          tooltip: l10n.diagnosticsCopy,
          onPressed: onCopy,
          icon: const Icon(AppIcons.copy, size: 18),
        ),
        IconButton(
          tooltip: l10n.close,
          onPressed: () => Navigator.of(context).pop(),
          icon: const Icon(AppIcons.close, size: 18),
        ),
      ],
    );
  }
}

/// The tunnel at a glance: its carrier, the relay, and its sessions.
class _Facts extends StatelessWidget {
  const _Facts({required this.tunnel, required this.diagnostics});

  final Tunnel tunnel;
  final TunnelDiagnostics diagnostics;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final sessions = diagnostics.direct;
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        CarrierChip(carrier: RelayTransport.parse(diagnostics.relayTransport)),
        if (diagnostics.consecutiveFailures > 0)
          _Pill(
            icon: AppIcons.warning,
            text: l10n.diagnosticsRelayFailures(
              diagnostics.consecutiveFailures,
            ),
            color: cautionColor(scheme),
          )
        else if (diagnostics.lastSetup case final setup?)
          _Pill(
            icon: AppIcons.relay,
            text: l10n.diagnosticsRelay(_ms(setup)),
          ),
        if (sessions != null)
          _Pill(
            icon: AppIcons.direct,
            text: tunnel.kind == TunnelKind.share
                ? l10n.diagnosticsDirectSessions(sessions.direct)
                : l10n.diagnosticsSessions(sessions.direct, sessions.relay),
          ),
        if (sessions != null && sessions.fallbacks > 0)
          _Pill(
            icon: AppIcons.mixedPath,
            text: l10n.diagnosticsFallbacks(sessions.fallbacks),
          ),
      ],
    );
  }
}

class _Pill extends StatelessWidget {
  const _Pill({required this.icon, required this.text, this.color});

  final IconData icon;
  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final ink = color ?? scheme.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 4),
      decoration: BoxDecoration(
        color: color?.withValues(alpha: 0.12) ?? softWash(scheme, strength: 1.3),
        borderRadius: BorderRadius.circular(20),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 13, color: ink),
          const SizedBox(width: 5),
          Text(
            text,
            style: TextStyle(
              fontSize: 12,
              fontWeight: FontWeight.w500,
              color: ink,
            ),
          ),
        ],
      ),
    );
  }
}

class _Heading extends StatelessWidget {
  const _Heading(this.text, {this.count});

  final String text;
  final int? count;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final style = theme.textTheme.labelMedium?.copyWith(
      color: scheme.onSurfaceVariant,
      fontWeight: FontWeight.w600,
      letterSpacing: 0.2,
    );
    return Padding(
      padding: const EdgeInsets.only(left: 2, bottom: 8),
      child: Semantics(
        header: true,
        child: Row(
          children: [
            Text(text, style: style),
            if (count != null && count! > 0) ...[
              const SizedBox(width: 6),
              Text(
                '$count',
                style: style?.copyWith(color: onSurfaceMuted(scheme)),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// How a stage, or an event, reads at a glance. A mapping the router will not
/// make, like a session on the relay, is no fault of the tunnel's, so nothing
/// here is red.
enum _Tone { ok, signal, busy, caution, quiet }

Color _toneColor(ColorScheme scheme, _Tone tone) => switch (tone) {
  _Tone.ok => successColor(scheme),
  _Tone.signal => signalColor(scheme),
  _Tone.busy => scheme.onSurfaceVariant,
  _Tone.caution => cautionColor(scheme),
  _Tone.quiet => onSurfaceMuted(scheme),
};

_Tone _mappingTone(PortMappingStage? stage) => switch (stage) {
  PortMappingStage.active || PortMappingStage.renewing => _Tone.ok,
  PortMappingStage.discovering ||
  PortMappingStage.gatewayFound ||
  PortMappingStage.discovered ||
  PortMappingStage.requesting ||
  PortMappingStage.networkChanged ||
  PortMappingStage.releasing => _Tone.busy,
  PortMappingStage.retrying ||
  PortMappingStage.expired ||
  PortMappingStage.failed ||
  PortMappingStage.releaseFailed => _Tone.caution,
  _ => _Tone.quiet,
};

String _stageLabel(BuildContext context, PortMappingStatus status) =>
    context.l10n.portMappingStage(status.stage?.wire ?? '');

/// The port mapping, as it stands: whether it is on, where it got to, the
/// address it maps to, and what it took.
class _PortMapping extends StatelessWidget {
  const _PortMapping({required this.tunnel, required this.mapping});

  final Tunnel tunnel;
  final PortMappingDiagnostics? mapping;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    final mapping = this.mapping;
    final off = tunnel.portMapping == PortMappingMode.off
        ? l10n.portMappingOff
        : !tunnel.allowDirect
        ? l10n.portMappingNeedsDirect
        : null;
    final Widget body;
    if (off != null || mapping == null) {
      body = Text(
        off ?? l10n.portMappingStage('waiting_for_direct'),
        key: const Key('port-mapping-off'),
        style: muted,
      );
    } else {
      final status = mapping.current;
      final tone = _mappingTone(status.stage);
      final color = _toneColor(scheme, tone);
      final lease = status.lease;
      final facts = [
        if (status.active && lease != null)
          l10n.portMappingLease(formatSpan(context, lease)),
        if (status.gateway case final gateway?) l10n.portMappingRouter(gateway),
      ];
      body = Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            children: [
              tone == _Tone.busy
                  ? PulsingDot(color: color)
                  : StatusDot(color: color),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  _stageLabel(context, status),
                  key: const Key('port-mapping-stage'),
                  style: theme.textTheme.bodyMedium?.copyWith(
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              if (status.protocol case final protocol?)
                _Pill(icon: AppIcons.router, text: protocol.label),
            ],
          ),
          if (status.externalAddress case final external?) ...[
            const SizedBox(height: 8),
            SelectableText(
              '${status.localPort == null ? '' : ':${status.localPort} → '}$external',
              key: const Key('port-mapping-address'),
              style: mono(
                fontSize: 13,
                color: status.active ? scheme.onSurface : onSurfaceMuted(scheme),
              ),
            ),
          ],
          if (facts.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(facts.join(' · '), style: muted),
          ],
          if (status.error case final error?) ...[
            const SizedBox(height: 6),
            Text(error, style: muted?.copyWith(color: cautionColor(scheme))),
          ],
          const SizedBox(height: 8),
          Text(
            l10n.portMappingFigures(
              mapping.attempts,
              mapping.renewals,
              mapping.failures,
            ),
            style: muted?.copyWith(color: onSurfaceMuted(scheme), fontSize: 12),
          ),
        ],
      );
    }
    return Container(
      padding: const EdgeInsets.fromLTRB(14, 12, 12, 12),
      decoration: BoxDecoration(
        color: softWash(scheme),
        borderRadius: BorderRadius.circular(14),
      ),
      child: body,
    );
  }
}

class _Missed extends StatelessWidget {
  const _Missed({required this.count});

  final int count;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final caution = cautionColor(theme.colorScheme);
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(AppIcons.info, size: 14, color: caution),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              context.l10n.diagnosticsMissed(count),
              style: theme.textTheme.bodySmall?.copyWith(color: caution),
            ),
          ),
        ],
      ),
    );
  }
}

/// What an event says: its title, a detail, the glyph and tone it is shown
/// with.
typedef _Reading = ({String title, String? detail, IconData icon, _Tone tone});

_Reading _read(BuildContext context, TunnelEvent event) {
  final l10n = context.l10n;
  String stage() => _directStage(context, DirectStage.parse(event.stage));
  switch (event.kind) {
    case 'relay_ready':
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: event.latency == null ? null : _ms(event.latency!),
        icon: AppIcons.relay,
        tone: _Tone.ok,
      );
    case 'relay_failed':
      final retry = event.retryIn;
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: [
          l10n.relayFailure(event.failure ?? ''),
          if (retry != null && retry > Duration.zero)
            l10n.tunnelEventRetryIn(formatSpan(context, retry)),
        ].join(' · '),
        icon: AppIcons.warning,
        tone: _Tone.caution,
      );
    case 'direct_established':
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: event.setup == null
            ? null
            : l10n.tunnelEventSetup(event.setup!.inMilliseconds),
        icon: AppIcons.direct,
        tone: _Tone.signal,
      );
    case 'direct_fallback':
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: l10n.tunnelEventAtStage(stage()),
        icon: AppIcons.relay,
        tone: _Tone.quiet,
      );
    case 'direct_abandoned':
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: l10n.tunnelEventAtStage(stage()),
        icon: AppIcons.direct,
        tone: _Tone.quiet,
      );
    case 'direct_stage_changed':
      return (
        title: l10n.tunnelEventDirectStage(stage()),
        detail: null,
        icon: AppIcons.direct,
        tone: _Tone.busy,
      );
    case 'phase_changed':
      return (
        title: l10n.relayPhase(event.phase ?? ''),
        detail: null,
        icon: AppIcons.relay,
        tone: switch (event.phase) {
          'ready' => _Tone.ok,
          'backoff' => _Tone.caution,
          _ => _Tone.busy,
        },
      );
    case 'port_mapping_changed':
      final status = event.mapping;
      return (
        title: status == null
            ? l10n.portMapping
            : _stageLabel(context, status),
        detail: switch (status) {
          PortMappingStatus(:final error?) => error,
          PortMappingStatus(:final externalAddress?, :final protocol) => [
            ?protocol?.label,
            externalAddress,
          ].join(' · '),
          _ => null,
        },
        icon: AppIcons.router,
        tone: _mappingTone(status?.stage),
      );
    case 'network_changed':
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: null,
        icon: AppIcons.online,
        tone: _Tone.quiet,
      );
    case 'stopped':
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: null,
        icon: AppIcons.stop,
        tone: _Tone.quiet,
      );
    default:
      return (
        title: l10n.tunnelEventTitle(event.kind),
        detail: null,
        icon: AppIcons.info,
        tone: _Tone.quiet,
      );
  }
}

String _directStage(BuildContext context, DirectStage? stage) {
  final l10n = context.l10n;
  return switch (stage) {
    DirectStage.negotiating || null => l10n.pathStageNegotiating,
    DirectStage.discovering => l10n.pathStageDiscovering,
    DirectStage.punching => l10n.pathStagePunching,
    DirectStage.committing => l10n.pathStageCommitting,
  };
}

String _ms(Duration d) => '${d.inMilliseconds} ms';

/// One event: its glyph, what it says and when, and on a tap the fields it
/// came with.
class _EventRow extends StatefulWidget {
  const _EventRow({super.key, required this.received});

  final _Received received;

  @override
  State<_EventRow> createState() => _EventRowState();
}

class _EventRowState extends State<_EventRow> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final event = widget.received.event;
    final reading = _read(context, event);
    final color = _toneColor(scheme, reading.tone);
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    final fields = _open
        ? Container(
            width: double.infinity,
            margin: const EdgeInsets.fromLTRB(40, 2, 0, 8),
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: codeSurface(scheme),
              borderRadius: BorderRadius.circular(10),
            ),
            child: SelectableText(
              const JsonEncoder.withIndent('  ').convert(event.raw),
              style: mono(fontSize: 11.5, color: scheme.onSurfaceVariant),
            ),
          )
        : const SizedBox(width: double.infinity);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        InkWell(
          borderRadius: BorderRadius.circular(10),
          onTap: () => setState(() => _open = !_open),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 2, vertical: 7),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Container(
                  width: 28,
                  height: 28,
                  decoration: BoxDecoration(
                    color: color.withValues(alpha: 0.13),
                    shape: BoxShape.circle,
                  ),
                  child: Icon(reading.icon, size: 15, color: color),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const SizedBox(height: 4),
                      Text(reading.title, style: theme.textTheme.bodyMedium),
                      if (reading.detail case final detail?
                          when detail.isNotEmpty) ...[
                        const SizedBox(height: 2),
                        Text(detail, style: muted),
                      ],
                    ],
                  ),
                ),
                const SizedBox(width: 8),
                Padding(
                  padding: const EdgeInsets.only(top: 5),
                  child: Text(
                    [
                      if (event.repeats > 1) '×${event.repeats}',
                      formatAgo(
                        context,
                        DateTime.now().difference(widget.received.at),
                      ),
                    ].join('  '),
                    style: muted?.copyWith(
                      color: onSurfaceMuted(scheme),
                      fontFeatures: const [FontFeature.tabularFigures()],
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
        Motion.reduced(context)
            ? fields
            : AnimatedSize(
                duration: Motion.medium,
                curve: Motion.move,
                alignment: Alignment.topLeft,
                child: fields,
              ),
      ],
    );
  }
}
