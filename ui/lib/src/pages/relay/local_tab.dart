import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// A relay this computer runs itself: for other devices on the network, or
/// on the internet once its ports are forwarded. Start it, stop it, and hand
/// out its administrator key — or use it from this computer straight away.
class LocalRelayTab extends StatefulWidget {
  const LocalRelayTab({super.key});

  @override
  State<LocalRelayTab> createState() => _LocalRelayTabState();
}

class _LocalRelayTabState extends State<LocalRelayTab> {
  final TextEditingController _port = TextEditingController(text: '7666');
  final TextEditingController _quicPort = TextEditingController(text: '7667');
  bool _keepAlive = true;
  bool _offerQuic = false;
  bool _busy = false;
  String _key = '';
  String? _portError;
  String? _quicPortError;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // A relay already running keeps the port it was started on, whatever
    // this field held when the tab was last built.
    final local = AppScope.of(context).relay.local;
    if (local.port case final port?) _port.text = '$port';
    if (local.quicPort case final quic?) {
      _quicPort.text = '$quic';
      _offerQuic = true;
    }
  }

  @override
  void dispose() {
    _port.dispose();
    _quicPort.dispose();
    super.dispose();
  }

  int? get _portNumber => _parsePort(_port.text);

  int? get _quicPortNumber => _parsePort(_quicPort.text);

  static int? _parsePort(String text) {
    final port = int.tryParse(text.trim());
    return port != null && port > 0 && port < 65536 ? port : null;
  }

  Future<void> _toggle(RelayStore relay) async {
    final l10n = context.l10n;
    final running = relay.local.isRunning;
    final port = _portNumber;
    final quicPort = _offerQuic ? _quicPortNumber : null;
    if (!running) {
      // Its own UDP port: the TCP port's carries direct sessions.
      final quicError = !_offerQuic
          ? null
          : quicPort == null
          ? l10n.localPortInvalid
          : quicPort == port
          ? l10n.localQuicPortClash
          : null;
      setState(() {
        _portError = port == null ? l10n.localPortInvalid : null;
        _quicPortError = quicError;
      });
      if (port == null || quicError != null) return;
    }
    setState(() => _busy = true);
    final result = running
        ? await relay.stopLocalRelay()
        : await relay.startLocalRelay(
            port: port!,
            keepAlive: _keepAlive,
            quicPort: quicPort,
          );
    if (!mounted) return;
    setState(() => _busy = false);
    if (!result.success) {
      showToast(context, result.message, kind: ToastKind.error);
    } else {
      showToast(
        context,
        running ? l10n.localStoppedToast : l10n.localStarted(port!),
        kind: ToastKind.success,
      );
    }
  }

  Future<void> _reveal(RelayStore relay) async {
    final key = await relay.api.revealIsolatedRelayAdminKey();
    if (mounted) setState(() => _key = key);
  }

  Future<void> _useHere(RelayStore relay) async {
    final l10n = context.l10n;
    final port = relay.local.port ?? _portNumber ?? 7666;
    final key = _key.isEmpty
        ? await relay.api.revealIsolatedRelayAdminKey()
        : _key;
    if (!mounted || key.isEmpty) return;
    final address = '127.0.0.1:$port';
    // QUIC stays chosen only where this relay offers it.
    final carrier = relay.local.quicPort == null
        ? RelayTransport.tcp
        : relay.app.config.relayTransport;
    final result = await relay.app.saveRelay(
      address: address,
      credential: key,
      keepAlive: _keepAlive,
      relayTransport: carrier,
    );
    if (!mounted) return;
    showToast(
      context,
      result.success ? l10n.localUsing(address) : result.message,
      kind: result.success ? ToastKind.success : ToastKind.error,
    );
  }

  @override
  Widget build(BuildContext context) => RelayListener(builder: _build);

  Widget _build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final relay = AppScope.of(context).relay;
    final local = relay.local;
    final running = local.isRunning;
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    final keySet = relay.app.config.isolatedRelayAdminKeySet;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 14),
        GroupCard(
          title: l10n.localTitle,
          icon: AppIcons.computer,
          trailing: StatusChip(
            key: const Key('local-state'),
            color: running ? successColor(scheme) : availableColor(scheme),
            label: running ? l10n.localRunning : l10n.localStopped,
            pulsing: running,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                l10n.localBody(local.port ?? _portNumber ?? 7666),
                style: muted,
              ),
              if (running) ...[
                const SizedBox(height: 10),
                Text(
                  [
                    l10n.localUptime(
                      formatSpan(
                        context,
                        Duration(seconds: local.uptimeSeconds),
                      ),
                    ),
                    l10n.relayServiceConnections(local.activeConnections),
                    l10n.servicesCount(local.registeredServices),
                    if (local.quicPort case final quic?) l10n.localQuicOn(quic),
                  ].join('  ·  '),
                  key: const Key('local-stats'),
                  style: theme.textTheme.bodyMedium,
                ),
              ],
              const SizedBox(height: 14),
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(
                    width: 140,
                    child: TextField(
                      key: const Key('local-port'),
                      controller: _port,
                      enabled: !running && !_busy,
                      keyboardType: TextInputType.number,
                      style: mono(fontSize: 13.5),
                      decoration: InputDecoration(
                        labelText: l10n.localPort,
                        errorText: _portError,
                        isDense: true,
                      ),
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: SwitchListTile(
                      contentPadding: const EdgeInsets.symmetric(horizontal: 4),
                      title: Text(l10n.keepAliveDefault),
                      value: _keepAlive,
                      onChanged: running || _busy
                          ? null
                          : (v) => setState(() => _keepAlive = v),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(
                    width: 140,
                    child: TextField(
                      key: const Key('local-quic-port'),
                      controller: _quicPort,
                      enabled: _offerQuic && !running && !_busy,
                      keyboardType: TextInputType.number,
                      style: mono(fontSize: 13.5),
                      decoration: InputDecoration(
                        labelText: l10n.localQuicPort,
                        errorText: _quicPortError,
                        isDense: true,
                      ),
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: SwitchListTile(
                      key: const Key('local-quic'),
                      contentPadding: const EdgeInsets.symmetric(horizontal: 4),
                      title: Text(l10n.localQuic),
                      subtitle: Text(l10n.localQuicHelp),
                      value: _offerQuic,
                      onChanged: running || _busy
                          ? null
                          : (v) => setState(() {
                              _offerQuic = v;
                              _quicPortError = null;
                            }),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              Align(
                alignment: AlignmentDirectional.centerStart,
                child: running
                    ? OutlinedButton.icon(
                        key: const Key('local-toggle'),
                        onPressed: _busy ? null : () => _toggle(relay),
                        icon: const Icon(AppIcons.stop, size: 16),
                        label: Text(l10n.localStop),
                      )
                    : FilledButton.icon(
                        key: const Key('local-toggle'),
                        onPressed: _busy ? null : () => _toggle(relay),
                        icon: const Icon(AppIcons.start, size: 16),
                        label: Text(l10n.localStart),
                      ),
              ),
            ],
          ),
        ),
        if (keySet || running) ...[
          const SizedBox(height: 16),
          GroupCard(
            title: l10n.embeddedRelayKey,
            icon: AppIcons.key,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(l10n.embeddedRelayKeyHelp, style: muted),
                const SizedBox(height: 10),
                if (_key.isEmpty)
                  OutlinedButton.icon(
                    key: const Key('settings-reveal'),
                    onPressed: () => _reveal(relay),
                    icon: const Icon(AppIcons.show, size: 16),
                    label: Text(l10n.reveal),
                  )
                else
                  SelectableText(_key, style: mono(fontSize: 13)),
                const SizedBox(height: 10),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    if (_key.isNotEmpty)
                      TextButton.icon(
                        onPressed: () =>
                            copySecret(context, _key, l10n.credentialCopied),
                        icon: const Icon(AppIcons.copy, size: 16),
                        label: Text(l10n.credentialCopy),
                      ),
                    TextButton.icon(
                      key: const Key('local-use-here'),
                      onPressed: () => _useHere(relay),
                      icon: const Icon(AppIcons.link, size: 16),
                      label: Text(l10n.localUseHere),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ],
      ],
    );
  }
}
