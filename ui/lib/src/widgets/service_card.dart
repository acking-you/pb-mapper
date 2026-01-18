import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/models/service_config.dart';

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

  Color _getStatusColor() {
    switch (_config.status) {
      case ServiceStatus.running:
        return Colors.green;
      case ServiceStatus.retrying:
        return Colors.orange;
      case ServiceStatus.failed:
        return Colors.red;
      case ServiceStatus.stopped:
        return Colors.grey;
    }
  }

  IconData _getStatusIcon() {
    switch (_config.status) {
      case ServiceStatus.running:
        return Icons.check_circle;
      case ServiceStatus.retrying:
        return Icons.sync;
      case ServiceStatus.failed:
        return Icons.error;
      case ServiceStatus.stopped:
        return Icons.stop_circle;
    }
  }

  String _getStatusText() {
    switch (_config.status) {
      case ServiceStatus.running:
        return 'Running';
      case ServiceStatus.retrying:
        return 'Retrying Connection';
      case ServiceStatus.failed:
        return 'Connection Failed';
      case ServiceStatus.stopped:
        return 'Stopped';
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
          statusMessage: 'Stopping...',
        );
      });
    } else {
      // Start/restart the service via parent callback
      widget.onStartStop?.call();
      setState(() {
        _config = _config.copyWith(
          status: ServiceStatus.retrying,
          statusMessage: 'Starting...',
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

  @override
  Widget build(BuildContext context) {
    return Card(
      margin: const EdgeInsets.only(bottom: 12),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            // Header row with service name and status
            Row(
              children: [
                // Status indicator
                Container(
                  width: 12,
                  height: 12,
                  decoration: BoxDecoration(
                    color: _getStatusColor(),
                    shape: BoxShape.circle,
                  ),
                ),
                const SizedBox(width: 8),
                // Service key
                Expanded(
                  child: Text(
                    _config.serviceKey,
                    style: Theme.of(context).textTheme.titleMedium?.copyWith(
                      fontWeight: FontWeight.bold,
                    ),
                  ),
                ),
                // Action buttons
                Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    // Start/Stop button
                    IconButton(
                      onPressed: widget.onStartStop,
                      icon: Icon(
                        _config.status == ServiceStatus.running ||
                                _config.status == ServiceStatus.retrying
                            ? Icons.stop
                            : Icons.play_arrow,
                        size: 20,
                      ),
                      tooltip:
                          _config.status == ServiceStatus.running ||
                              _config.status == ServiceStatus.retrying
                          ? 'Stop Service'
                          : 'Start Service',
                      color:
                          _config.status == ServiceStatus.running ||
                              _config.status == ServiceStatus.retrying
                          ? Colors.orange
                          : Colors.green,
                    ),
                    // Refresh button
                    IconButton(
                      onPressed: widget.onRefresh,
                      icon: const Icon(Icons.refresh, size: 20),
                      tooltip: 'Refresh Status',
                    ),
                    // Edit button
                    IconButton(
                      onPressed: widget.onEdit,
                      icon: const Icon(Icons.edit, size: 20),
                      tooltip: 'Edit Configuration',
                    ),
                    // Delete button (delete config permanently)
                    IconButton(
                      onPressed: widget.onDelete,
                      icon: const Icon(
                        Icons.delete_forever,
                        size: 20,
                        color: Colors.red,
                      ),
                      tooltip: 'Delete Configuration',
                    ),
                  ],
                ),
              ],
            ),

            const SizedBox(height: 12),

            // Service details
            Row(
              children: [
                _buildDetailChip(
                  icon: Icons.computer,
                  label: _config.localAddress,
                  color: Colors.blue,
                ),
                const SizedBox(width: 8),
                _buildDetailChip(
                  icon: _config.protocol == 'TCP' ? Icons.share : Icons.grain,
                  label: _config.protocol,
                  color: _config.protocol == 'TCP'
                      ? Colors.green
                      : Colors.purple,
                ),
                if (_config.enableEncryption) ...[
                  const SizedBox(width: 8),
                  _buildDetailChip(
                    icon: Icons.lock,
                    label: 'Encrypted',
                    color: Colors.orange,
                  ),
                ],
              ],
            ),

            const SizedBox(height: 12),

            // Status row
            Row(
              children: [
                Icon(_getStatusIcon(), size: 16, color: _getStatusColor()),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    _config.statusMessage.isNotEmpty
                        ? _config.statusMessage
                        : _getStatusText(),
                    style: Theme.of(
                      context,
                    ).textTheme.bodySmall?.copyWith(color: _getStatusColor()),
                  ),
                ),
                // Start/Stop button
                SizedBox(
                  height: 32,
                  child: ElevatedButton(
                    onPressed: _isOperating ? null : _toggleService,
                    style: ElevatedButton.styleFrom(
                      backgroundColor:
                          _config.status == ServiceStatus.running ||
                              _config.status == ServiceStatus.retrying
                          ? Colors.red
                          : Colors.green,
                      foregroundColor: Colors.white,
                      shape: RoundedRectangleBorder(
                        borderRadius: BorderRadius.circular(16),
                      ),
                    ),
                    child: _isOperating
                        ? const SizedBox(
                            width: 16,
                            height: 16,
                            child: CircularProgressIndicator(
                              strokeWidth: 2,
                              color: Colors.white,
                            ),
                          )
                        : Text(
                            _config.status == ServiceStatus.running ||
                                    _config.status == ServiceStatus.retrying
                                ? 'Stop'
                                : 'Start',
                            style: const TextStyle(fontSize: 12),
                          ),
                  ),
                ),
              ],
            ),

            // Last updated info
            if (_config.updatedAt != _config.createdAt) ...[
              const SizedBox(height: 8),
              Text(
                'Updated: ${_formatDateTime(_config.updatedAt)}',
                style: Theme.of(context).textTheme.bodySmall?.copyWith(
                  color: Colors.grey,
                  fontSize: 11,
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _buildDetailChip({
    required IconData icon,
    required String label,
    required Color color,
  }) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.1),
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: color.withValues(alpha: 0.3)),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 14, color: color),
          const SizedBox(width: 4),
          Text(
            label,
            style: TextStyle(
              fontSize: 12,
              color: color,
              fontWeight: FontWeight.w500,
            ),
          ),
        ],
      ),
    );
  }

  String _formatDateTime(DateTime dateTime) {
    final now = DateTime.now();
    final difference = now.difference(dateTime);

    if (difference.inMinutes < 1) {
      return 'Just now';
    } else if (difference.inHours < 1) {
      return '${difference.inMinutes}m ago';
    } else if (difference.inDays < 1) {
      return '${difference.inHours}h ago';
    } else {
      return '${difference.inDays}d ago';
    }
  }
}
