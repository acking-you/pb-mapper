import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

Future<void> openTunnelDiagnostics(
  BuildContext context,
  AppStore store,
  Tunnel tunnel,
) => showAdaptivePanel<void>(
  context: context,
  maxWidth: 640,
  builder: (_) => TunnelDiagnosticsPanel(api: store.api, tunnel: tunnel),
);

class TunnelDiagnosticsPanel extends StatefulWidget {
  const TunnelDiagnosticsPanel({
    super.key,
    required this.api,
    required this.tunnel,
  });
  final PbMapperApiClient api;
  final Tunnel tunnel;
  @override
  State<TunnelDiagnosticsPanel> createState() => _TunnelDiagnosticsPanelState();
}

class _TunnelDiagnosticsPanelState extends State<TunnelDiagnosticsPanel> {
  StreamSubscription<DiagnosticUpdate>? _subscription;
  final List<TunnelDiagnosticEvent> _events = [];
  Map<String, dynamic>? _snapshot;
  Map<String, dynamic>? _mapping;
  int _lastSeq = 0;
  int _lagged = 0;
  bool _closed = false;
  bool _refreshing = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _listen();
  }

  void _listen() {
    _subscription = widget.api
        .diagnostics(
          key: widget.tunnel.key,
          share: widget.tunnel.kind == TunnelKind.share,
        )
        .listen(
          _update,
          onError: (Object error) {
            if (mounted) setState(() => _error = error.toString());
          },
          onDone: () {
            if (mounted) setState(() => _closed = true);
          },
        );
  }

  void _update(DiagnosticUpdate update) {
    if (!mounted) return;
    setState(() {
      if (update.snapshot case final snapshot?) {
        _snapshot = snapshot;
        final mapping = snapshot['port_mapping'];
        if (mapping is Map && mapping['current'] is Map) {
          _mapping = Map<String, dynamic>.from(mapping['current'] as Map);
        }
        for (final event in (snapshot['events'] as List? ?? const [])) {
          _append(
            TunnelDiagnosticEvent(Map<String, dynamic>.from(event as Map)),
          );
        }
      }
      if (update.event case final event?) {
        _append(event);
      }
      _lagged += update.lagged;
      _closed = update.closed;
    });
  }

  void _append(TunnelDiagnosticEvent event) {
    if (event.seq <= _lastSeq) return;
    _lastSeq = event.seq;
    if (event.mapping case final mapping?) _mapping = mapping;
    _events.add(event);
    if (_events.length > 128) _events.removeAt(0);
  }

  Future<void> _refresh() async {
    if (_refreshing) return;
    setState(() => _refreshing = true);
    try {
      await _subscription?.cancel();
      if (!mounted) return;
      setState(() {
        _error = null;
        _closed = false;
        _lastSeq = 0;
        _lagged = 0;
        _snapshot = null;
        _mapping = null;
        _events.clear();
      });
      _listen();
    } catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) setState(() => _refreshing = false);
    }
  }

  @override
  void dispose() {
    unawaited(_subscription?.cancel());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final mapping = _mapping;
    return Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  l10n.tunnelDiagnostics,
                  style: theme.textTheme.titleLarge,
                ),
              ),
              IconButton(
                tooltip: l10n.diagnosticsRefresh,
                onPressed: _refreshing ? null : _refresh,
                icon: const Icon(AppIcons.refresh),
              ),
              IconButton(
                tooltip: l10n.diagnosticsCopy,
                onPressed: () => Clipboard.setData(
                  ClipboardData(
                    text: const JsonEncoder.withIndent('  ').convert({
                      'snapshot': _snapshot,
                      'mapping': _mapping,
                      'events': _events.map((e) => e.data).toList(),
                      'lagged': _lagged,
                      'closed': _closed,
                    }),
                  ),
                ),
                icon: const Icon(AppIcons.copy),
              ),
              IconButton(
                tooltip: l10n.close,
                onPressed: () => Navigator.of(context).pop(),
                icon: const Icon(AppIcons.close),
              ),
            ],
          ),
          Text(widget.tunnel.key, style: theme.textTheme.titleSmall),
          const SizedBox(height: 12),
          Text(
            '${l10n.portMapping} · ${!widget.tunnel.portMapping ? l10n.diagnosticStage('disabled') : l10n.diagnosticStage(mapping?['stage']?.toString() ?? (widget.tunnel.allowDirect ? 'waiting_for_direct' : 'relay_only'))}',
            key: const Key('mapping-state'),
            style: theme.textTheme.titleMedium,
          ),
          if (mapping != null) ...[
            const SizedBox(height: 4),
            if (mapping['external_addr'] != null)
              SelectableText(
                '${mapping['protocol']?.toString().toUpperCase() ?? ''}  :${mapping['local_port']} → ${mapping['external_addr']}',
                style: theme.textTheme.bodyMedium,
              ),
            if (mapping['error'] != null)
              Text(
                mapping['error'].toString(),
                style: TextStyle(color: theme.colorScheme.error),
              ),
          ],
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _error!,
                style: TextStyle(color: theme.colorScheme.error),
              ),
            ),
          if (_lagged > 0)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(l10n.diagnosticsLagged(_lagged)),
            ),
          const SizedBox(height: 16),
          const Divider(),
          if (_events.isEmpty)
            Text(l10n.diagnosticsWaiting, style: theme.textTheme.bodySmall),
          // The surrounding adaptive panel owns scrolling at narrow sizes.
          for (final event in _events.reversed)
            ExpansionTile(
              key: Key('diagnostic-${event.seq}'),
              tilePadding: EdgeInsets.zero,
              title: Text(
                l10n.diagnosticStage(event.stage),
                style: theme.textTheme.bodyMedium,
              ),
              subtitle: event.mapping?['error'] == null
                  ? null
                  : Text(
                      event.mapping!['error'].toString(),
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
              children: [
                Align(
                  alignment: AlignmentDirectional.centerStart,
                  child: SelectableText(
                    const JsonEncoder.withIndent('  ').convert(event.data),
                    style: theme.textTheme.bodySmall,
                  ),
                ),
              ],
            ),
        ],
      ),
    );
  }
}
