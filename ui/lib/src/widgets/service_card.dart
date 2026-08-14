import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/models/service_config.dart';
import 'package:pb_mapper_ui/src/widgets/list_card.dart';

class ServiceCard extends StatefulWidget {
  final ServiceConfig config;
  final VoidCallback? onEdit;
  final VoidCallback? onDelete;
  final VoidCallback? onStartStop;
  final VoidCallback? onRefresh;
  final Function(ServiceConfig)? onStatusChanged;

  const ServiceCard({
    super.key,
    required this.config,
    this.onEdit,
    this.onDelete,
    this.onStartStop,
    this.onRefresh,
    this.onStatusChanged,
  });

  @override
  State<ServiceCard> createState() => _ServiceCardState();
}

class _ServiceCardState extends State<ServiceCard> {
  late ServiceConfig _config;
  bool _isOperating = false;

  @override
  void initState() {
    super.initState();
    _config = widget.config;
  }

  @override
  void didUpdateWidget(covariant ServiceCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.config != widget.config) {
      setState(() {
        _config = widget.config;
        _isOperating = false;
      });
    }
  }

  String _getStatusText(BuildContext context) {
    switch (_config.status) {
      case ServiceStatus.running:
        return context.l10n.statusRunning;
      case ServiceStatus.retrying:
        return context.l10n.statusRetrying;
      case ServiceStatus.failed:
        return context.l10n.statusFailed;
      case ServiceStatus.stopped:
        return context.l10n.statusStopped;
    }
  }

  void _toggleService() {
    if (_isOperating) return;

    setState(() => _isOperating = true);

    if (_config.status == ServiceStatus.running ||
        _config.status == ServiceStatus.retrying) {
      // Stop the service via parent callback
      widget.onStartStop?.call();
      setState(() {
        _config = _config.copyWith(
          status: ServiceStatus.stopped,
          statusMessage: context.l10n.stopping,
        );
      });
    } else {
      // Start/restart the service via parent callback
      widget.onStartStop?.call();
      setState(() {
        _config = _config.copyWith(
          status: ServiceStatus.retrying,
          statusMessage: context.l10n.starting,
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

  bool get _isUp =>
      _config.status == ServiceStatus.running ||
      _config.status == ServiceStatus.retrying;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final l10n = context.l10n;
    final tone = _statusTone().resolve(context);

    // Same shape as the connect row: dot, name and state, facts, one action.
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
          ),
          ListCardFact(
            icon: Icons.swap_horiz_rounded,
            label: _config.protocol,
          ),
          if (_config.enableEncryption)
            ListCardFact(
              icon: Icons.lock_outline_rounded,
              label: l10n.encrypted,
            ),
          if (_config.updatedAt != _config.createdAt)
            ListCardFact(
              icon: Icons.schedule_rounded,
              label: _formatDateTime(context, _config.updatedAt),
            ),
        ],
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
                    onPressed: _toggleService,
                    style: listCardActionStyle(context, filled: false),
                    child: Text(l10n.stop),
                  )
                : FilledButton(
                    onPressed: _toggleService,
                    style: listCardActionStyle(context, filled: true),
                    child: Text(l10n.start),
                  ),
          ),
          const SizedBox(width: 4),
          ListCardIconAction(
            icon: Icons.refresh_rounded,
            tooltip: l10n.refreshStatus,
            onPressed: widget.onRefresh,
          ),
          ListCardIconAction(
            icon: Icons.edit_outlined,
            tooltip: l10n.editConfig,
            onPressed: widget.onEdit,
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
      case ServiceStatus.running:
        return StatusTone.ok;
      case ServiceStatus.retrying:
        return StatusTone.pending;
      case ServiceStatus.failed:
        return StatusTone.bad;
      case ServiceStatus.stopped:
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
