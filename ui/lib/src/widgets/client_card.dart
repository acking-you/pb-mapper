import 'package:flutter/material.dart';
import 'package:ui/src/models/client_config.dart';
import 'package:ui/src/bindings/bindings.dart';

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
    _setupStatusListener();
  }

  void _setupStatusListener() {
    // Listen for client connection status updates
    ClientConnectionStatus.rustSignalStream.listen((signal) {
      if (mounted) {
        final message = signal.message.status;
        // Check if this update is for our client
        if (message.contains(_config.serviceKey)) {
          setState(() => _isOperating = false);
          widget.onStatusChanged?.call(_config);
        }
      }
    });

    // Listen for individual client status responses
    ClientStatusResponse.rustSignalStream.listen((signal) {
      if (signal.message.serviceKey == _config.serviceKey && mounted) {
        final status = _parseStatus(signal.message.status);
        setState(() {
          _config = _config.copyWith(
            status: status,
            statusMessage: signal.message.message,
          );
          _isOperating = false;
        });
        widget.onStatusChanged?.call(_config);
      }
    });
  }

  ClientStatus _parseStatus(String statusString) {
    switch (statusString.toLowerCase()) {
      case 'running':
      case 'connected':
        return ClientStatus.running;
      case 'retrying':
        return ClientStatus.retrying;
      case 'failed':
        return ClientStatus.failed;
      case 'stopped':
      default:
        return ClientStatus.stopped;
    }
  }

  Color _getStatusColor() {
    switch (_config.status) {
      case ClientStatus.running:
        return Colors.green;
      case ClientStatus.retrying:
        return Colors.orange;
      case ClientStatus.failed:
        return Colors.red;
      case ClientStatus.stopped:
        return Colors.grey;
    }
  }

  IconData _getStatusIcon() {
    switch (_config.status) {
      case ClientStatus.running:
        return Icons.check_circle;
      case ClientStatus.retrying:
        return Icons.sync;
      case ClientStatus.failed:
        return Icons.error;
      case ClientStatus.stopped:
        return Icons.stop_circle;
    }
  }

  String _getStatusText() {
    switch (_config.status) {
      case ClientStatus.running:
        return 'Connected';
      case ClientStatus.retrying:
        return 'Retrying Connection';
      case ClientStatus.failed:
        return 'Connection Failed';
      case ClientStatus.stopped:
        return 'Disconnected';
    }
  }

  void _toggleConnection() {
    if (_isOperating) return;

    setState(() => _isOperating = true);

    if (_config.status == ClientStatus.running ||
        _config.status == ClientStatus.retrying) {
      // Disconnect the client
      DisconnectServiceRequest(
        serviceKey: _config.serviceKey,
      ).sendSignalToRust();
      setState(() {
        _config = _config.copyWith(
          status: ClientStatus.stopped,
          statusMessage: 'Disconnecting...',
        );
      });
    } else {
      // Connect the client
      ConnectServiceRequest(
        serviceKey: _config.serviceKey,
        localAddress: _config.localAddress,
        protocol: _config.protocol,
        enableKeepAlive: _config.enableKeepAlive,
      ).sendSignalToRust();
      setState(() {
        _config = _config.copyWith(
          status: ClientStatus.retrying,
          statusMessage: 'Connecting...',
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
                    // Connect/Disconnect button
                    IconButton(
                      onPressed: widget.onConnectDisconnect,
                      icon: Icon(
                        _config.status == ClientStatus.running ||
                                _config.status == ClientStatus.retrying
                            ? Icons.stop
                            : Icons.play_arrow,
                        size: 20,
                      ),
                      tooltip:
                          _config.status == ClientStatus.running ||
                              _config.status == ClientStatus.retrying
                          ? 'Disconnect'
                          : 'Connect',
                      color:
                          _config.status == ClientStatus.running ||
                              _config.status == ClientStatus.retrying
                          ? Colors.orange
                          : Colors.green,
                    ),
                    // Refresh button
                    IconButton(
                      onPressed: widget.onRefresh,
                      icon: const Icon(Icons.refresh, size: 20),
                      tooltip: 'Refresh Status',
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

            // Client details (no encryption chip since clients don't support encryption)
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
                // Connect/Disconnect button
                SizedBox(
                  height: 32,
                  child: ElevatedButton(
                    onPressed: _isOperating ? null : _toggleConnection,
                    style: ElevatedButton.styleFrom(
                      backgroundColor:
                          _config.status == ClientStatus.running ||
                              _config.status == ClientStatus.retrying
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
                            _config.status == ClientStatus.running ||
                                    _config.status == ClientStatus.retrying
                                ? 'Disconnect'
                                : 'Connect',
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
