import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/models/client_config.dart';
import 'package:pb_mapper_ui/src/widgets/list_card.dart';
import 'package:url_launcher/url_launcher.dart';

class ClientCard extends StatefulWidget {
  final ClientConfig config;
  final VoidCallback? onDelete;
  final VoidCallback? onConnectDisconnect;
  final VoidCallback? onRefresh;
  final Function(ClientConfig)? onStatusChanged;

  const ClientCard({
    super.key,
    required this.config,
    this.onDelete,
    this.onConnectDisconnect,
    this.onRefresh,
    this.onStatusChanged,
  });

  @override
  State<ClientCard> createState() => _ClientCardState();
}

class _ClientCardState extends State<ClientCard> {
  late ClientConfig _config;
  bool _isOperating = false;

  @override
  void initState() {
    super.initState();
    _config = widget.config;
  }

  @override
  void didUpdateWidget(covariant ClientCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.config != widget.config) {
      setState(() {
        _config = widget.config;
        _isOperating = false;
      });
    }
  }

  Future<void> _copyLocalAddress() async {
    final addr = _config.localAddress.trim();
    if (addr.isEmpty) return;
    await Clipboard.setData(ClipboardData(text: addr));
    if (mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(
        SnackBar(content: Text(context.l10n.copiedToClipboard(addr))),
      );
    }
  }

  String _getStatusText(BuildContext context) {
    switch (_config.status) {
      case ClientStatus.running:
        return context.l10n.statusConnected;
      case ClientStatus.retrying:
        return context.l10n.statusRetrying;
      case ClientStatus.failed:
        return context.l10n.statusFailed;
      case ClientStatus.stopped:
        return context.l10n.statusDisconnected;
    }
  }

  void _toggleConnection() {
    if (_isOperating) return;

    setState(() => _isOperating = true);

    if (_config.status == ClientStatus.running ||
        _config.status == ClientStatus.retrying) {
      // Disconnect via parent callback
      widget.onConnectDisconnect?.call();
      setState(() {
        _config = _config.copyWith(
          status: ClientStatus.stopped,
          statusMessage: context.l10n.disconnecting,
        );
      });
    } else {
      // Connect via parent callback
      widget.onConnectDisconnect?.call();
      setState(() {
        _config = _config.copyWith(
          status: ClientStatus.retrying,
          statusMessage: context.l10n.connecting,
        );
      });
    }

    // Clear operating state after a delay if no status update received
    Future.delayed(const Duration(seconds: 10), () {
      if (mounted && _isOperating) {
        setState(() => _isOperating = false);
      }
    });
  }

  Future<void> _openLocalAddress() async {
    final addr = _config.localAddress.trim();
    if (addr.isEmpty) return;

    // Prefix scheme if missing to help browsers resolve the URL
    final String urlString = addr.contains('://') ? addr : 'http://$addr';
    Uri? uri;
    try {
      uri = Uri.parse(urlString);
    } catch (_) {
      uri = null;
    }

    if (uri != null && await canLaunchUrl(uri)) {
      await launchUrl(uri, mode: LaunchMode.externalApplication);
    } else if (mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(context.l10n.cannotOpen(urlString))));
    }
  }

  bool get _isUp =>
      _config.status == ClientStatus.running ||
      _config.status == ClientStatus.retrying;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final l10n = context.l10n;
    final tone = _statusTone().resolve(context);

    // Two rows, not three. The old card put the action on both rows: a green
    // play icon in the header and a green pill below it, which read as two
    // different buttons for the same thing.
    return ListCardShell(
      child: ListCardRow(
        tone: tone,
        heading: Row(
          children: [
            Flexible(
              child: Text(
                _config.serviceKey,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.titleSmall?.copyWith(
                  fontWeight: FontWeight.w600,
                ),
              ),
            ),
            const SizedBox(width: 10),
            Flexible(
              child: Text(
                _config.statusMessage.isNotEmpty
                    ? _config.statusMessage
                    : _getStatusText(context),
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall?.copyWith(color: tone),
              ),
            ),
          ],
        ),
        facts: [
          ListCardFact(
            icon: Icons.lan_outlined,
            label: _config.localAddress,
            onTap: _copyLocalAddress,
            tooltip: l10n.copy,
          ),
          ListCardFact(
            icon: Icons.swap_horiz_rounded,
            label: _config.protocol,
          ),
          if (_config.updatedAt != _config.createdAt)
            ListCardFact(
              icon: Icons.schedule_rounded,
              label: _formatDateTime(context, _config.updatedAt),
            ),
        ],
        // One primary action, then the secondary ones as plain icons.
        actions: [
          SizedBox(
            width: 88,
            height: 30,
            child: _isOperating
                ? const Center(
                    child: SizedBox(
                      width: 15,
                      height: 15,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    ),
                  )
                : _isUp
                ? OutlinedButton(
                    onPressed: _toggleConnection,
                    style: listCardActionStyle(context, filled: false),
                    child: Text(l10n.disconnect),
                  )
                : FilledButton(
                    onPressed: _toggleConnection,
                    style: listCardActionStyle(context, filled: true),
                    child: Text(l10n.connect),
                  ),
          ),
          const SizedBox(width: 4),
          ListCardIconAction(
            icon: Icons.open_in_new_rounded,
            tooltip: l10n.openInBrowser,
            onPressed: _openLocalAddress,
          ),
          ListCardIconAction(
            icon: Icons.refresh_rounded,
            tooltip: l10n.refreshStatus,
            onPressed: widget.onRefresh,
          ),
          ListCardIconAction(
            icon: Icons.delete_outline_rounded,
            tooltip: l10n.deleteConfig,
            onPressed: widget.onDelete,
            danger: true,
          ),
        ],
      ),
    );
  }

  StatusTone _statusTone() {
    switch (_config.status) {
      case ClientStatus.running:
        return StatusTone.ok;
      case ClientStatus.retrying:
        return StatusTone.pending;
      case ClientStatus.failed:
        return StatusTone.bad;
      case ClientStatus.stopped:
        return StatusTone.idle;
    }
  }

  String _formatDateTime(BuildContext context, DateTime dateTime) {
    final l10n = context.l10n;
    final difference = DateTime.now().difference(dateTime);

    if (difference.inMinutes < 1) {
      return l10n.justNow;
    } else if (difference.inHours < 1) {
      return l10n.minutesAgo(difference.inMinutes);
    } else if (difference.inDays < 1) {
      return l10n.hoursAgo(difference.inHours);
    } else {
      return l10n.daysAgo(difference.inDays);
    }
  }
}
