import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/bindings/bindings.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/views/log_view_button.dart';

class ServerManagementView extends StatefulWidget {
  const ServerManagementView({super.key});

  @override
  State<ServerManagementView> createState() => _ServerManagementViewState();
}

class _ServerManagementViewState extends State<ServerManagementView> {
  final _portController = TextEditingController(text: '7666');
  bool _isKeepAliveEnabled = true;
  bool _isServerRunning = false;
  String _serverStatus = 'Stopped';
  int _activeConnections = 0;
  int _registeredServices = 0;
  int _uptime = 0;
  bool _isStarting = false;
  bool _isStopping = false;

  @override
  void initState() {
    super.initState();
    _setupServerStatusListener();
    _requestServerStatus(); // Actively request status on page load
  }

  void _setupServerStatusListener() {
    LocalServerStatusUpdate.rustSignalStream.listen((signal) {
      if (mounted) {
        setState(() {
          _isServerRunning = signal.message.isRunning;
          _activeConnections = signal.message.activeConnections;
          _registeredServices = signal.message.registeredServices;
          _uptime = signal.message.uptimeSeconds.toInt();
          _serverStatus = signal.message.isRunning ? 'Running' : 'Stopped';

          // Clear loading states when we get a status update
          _isStarting = false;
          _isStopping = false;
        });
      }
    });
  }

  void _requestServerStatus() {
    const RequestLocalServerStatus().sendSignalToRust();
  }

  @override
  void dispose() {
    _portController.dispose();
    super.dispose();
  }

  void _startServer() {
    final port = int.tryParse(_portController.text) ?? 7666;
    setState(() => _isStarting = true);

    StartServerRequest(
      port: port,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();

    // Request status update after a brief delay to allow server to start
    Future.delayed(const Duration(milliseconds: 500), () {
      if (mounted) {
        _requestServerStatus();
      }
    });
  }

  void _stopServer() {
    setState(() => _isStopping = true);
    StopServerRequest().sendSignalToRust();

    // Request status update after a brief delay to allow server to stop
    Future.delayed(const Duration(milliseconds: 500), () {
      if (mounted) {
        _requestServerStatus();
      }
    });
  }

  String _formatUptime(int uptimeSeconds) {
    if (uptimeSeconds < 60) {
      return '${uptimeSeconds}s';
    } else if (uptimeSeconds < 3600) {
      final minutes = (uptimeSeconds / 60).floor();
      final seconds = uptimeSeconds % 60;
      return '${minutes}m ${seconds}s';
    } else {
      final hours = (uptimeSeconds / 3600).floor();
      final minutes = ((uptimeSeconds % 3600) / 60).floor();
      return '${hours}h ${minutes}m';
    }
  }

  Color _getStatusColor() {
    if (_serverStatus.startsWith('Error:')) {
      return Colors.red;
    } else if (_isServerRunning) {
      return Colors.green;
    } else {
      return Colors.grey;
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: ResponsiveLayout.getScreenPadding(context),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Card(
              child: Padding(
                padding: EdgeInsets.all(
                  ResponsiveLayout.getCardPadding(context),
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Server Configuration',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    TextField(
                      controller: _portController,
                      decoration: const InputDecoration(
                        labelText: 'Server Port',
                        hintText: '7666',
                      ),
                      keyboardType: TextInputType.number,
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    SwitchListTile(
                      title: const Text('Enable TCP Keep-Alive'),
                      value: _isKeepAliveEnabled,
                      onChanged: (value) {
                        setState(() => _isKeepAliveEnabled = value);
                      },
                    ),
                  ],
                ),
              ),
            ),
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),

            // Server Status Information Card
            Card(
              child: Padding(
                padding: EdgeInsets.all(
                  ResponsiveLayout.getCardPadding(context),
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Server Status',
                      style: Theme.of(context).textTheme.titleMedium,
                    ),
                    SizedBox(
                      height:
                          ResponsiveLayout.getVerticalSpacing(context) * 0.5,
                    ),
                    Row(
                      children: [
                        Container(
                          width: 12,
                          height: 12,
                          decoration: BoxDecoration(
                            color: _getStatusColor(),
                            shape: BoxShape.circle,
                          ),
                        ),
                        const SizedBox(width: 8),
                        Expanded(
                          child: Text(
                            _serverStatus,
                            style: Theme.of(context).textTheme.bodyMedium
                                ?.copyWith(
                                  fontWeight: FontWeight.w500,
                                  color: _serverStatus.startsWith('Error:')
                                      ? Colors.red
                                      : null,
                                ),
                          ),
                        ),
                      ],
                    ),
                    if (_isServerRunning) ...[
                      SizedBox(
                        height:
                            ResponsiveLayout.getVerticalSpacing(context) * 0.25,
                      ),
                      Text(
                        'Active Connections: $_activeConnections',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                      Text(
                        'Registered Services: $_registeredServices',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                      Text(
                        'Uptime: ${_formatUptime(_uptime)}',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    ],
                  ],
                ),
              ),
            ),
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),

            ResponsiveLayout.buildResponsiveRow(
              context: context,
              children: [
                // Start Server Button
                ResponsiveLayout.isMobile(context)
                    ? SizedBox(
                        width: double.infinity,
                        height: 48,
                        child: ElevatedButton(
                          onPressed: (_isServerRunning || _isStarting)
                              ? null
                              : _startServer,
                          style: ElevatedButton.styleFrom(
                            shape: RoundedRectangleBorder(
                              borderRadius: BorderRadius.circular(24),
                            ),
                          ),
                          child: _isStarting
                              ? const Row(
                                  mainAxisAlignment: MainAxisAlignment.center,
                                  children: [
                                    SizedBox(
                                      width: 16,
                                      height: 16,
                                      child: CircularProgressIndicator(
                                        strokeWidth: 2,
                                      ),
                                    ),
                                    SizedBox(width: 8),
                                    Text(
                                      'Starting...',
                                      style: TextStyle(fontSize: 16),
                                    ),
                                  ],
                                )
                              : Text(
                                  _isServerRunning
                                      ? 'Server Running'
                                      : 'Start Server',
                                  style: const TextStyle(fontSize: 16),
                                ),
                        ),
                      )
                    : Expanded(
                        child: SizedBox(
                          height: 48,
                          child: ElevatedButton(
                            onPressed: (_isServerRunning || _isStarting)
                                ? null
                                : _startServer,
                            style: ElevatedButton.styleFrom(
                              shape: RoundedRectangleBorder(
                                borderRadius: BorderRadius.circular(24),
                              ),
                            ),
                            child: _isStarting
                                ? const Row(
                                    mainAxisAlignment: MainAxisAlignment.center,
                                    children: [
                                      SizedBox(
                                        width: 16,
                                        height: 16,
                                        child: CircularProgressIndicator(
                                          strokeWidth: 2,
                                        ),
                                      ),
                                      SizedBox(width: 8),
                                      Text(
                                        'Starting...',
                                        style: TextStyle(fontSize: 16),
                                      ),
                                    ],
                                  )
                                : Text(
                                    _isServerRunning
                                        ? 'Server Running'
                                        : 'Start Server',
                                    style: const TextStyle(fontSize: 16),
                                  ),
                          ),
                        ),
                      ),

                if (!ResponsiveLayout.isMobile(context))
                  SizedBox(
                    width: ResponsiveLayout.getHorizontalPadding(context),
                  ),

                // Stop Server Button
                ResponsiveLayout.isMobile(context)
                    ? SizedBox(
                        width: double.infinity,
                        height: 48,
                        child: ElevatedButton(
                          onPressed: (!_isServerRunning || _isStopping)
                              ? null
                              : _stopServer,
                          style: ElevatedButton.styleFrom(
                            shape: RoundedRectangleBorder(
                              borderRadius: BorderRadius.circular(24),
                            ),
                          ),
                          child: _isStopping
                              ? const Row(
                                  mainAxisAlignment: MainAxisAlignment.center,
                                  children: [
                                    SizedBox(
                                      width: 16,
                                      height: 16,
                                      child: CircularProgressIndicator(
                                        strokeWidth: 2,
                                      ),
                                    ),
                                    SizedBox(width: 8),
                                    Text(
                                      'Stopping...',
                                      style: TextStyle(fontSize: 16),
                                    ),
                                  ],
                                )
                              : const Text(
                                  'Stop Server',
                                  style: TextStyle(fontSize: 16),
                                ),
                        ),
                      )
                    : Expanded(
                        child: SizedBox(
                          height: 48,
                          child: ElevatedButton(
                            onPressed: (!_isServerRunning || _isStopping)
                                ? null
                                : _stopServer,
                            style: ElevatedButton.styleFrom(
                              shape: RoundedRectangleBorder(
                                borderRadius: BorderRadius.circular(24),
                              ),
                            ),
                            child: _isStopping
                                ? const Row(
                                    mainAxisAlignment: MainAxisAlignment.center,
                                    children: [
                                      SizedBox(
                                        width: 16,
                                        height: 16,
                                        child: CircularProgressIndicator(
                                          strokeWidth: 2,
                                        ),
                                      ),
                                      SizedBox(width: 8),
                                      Text(
                                        'Stopping...',
                                        style: TextStyle(fontSize: 16),
                                      ),
                                    ],
                                  )
                                : const Text(
                                    'Stop Server',
                                    style: TextStyle(fontSize: 16),
                                  ),
                          ),
                        ),
                      ),
              ],
            ),
            SizedBox(
              height: ResponsiveLayout.getVerticalSpacing(context) * 1.5,
            ),
            // Log view button
            const LogViewButton(),
          ],
        ),
      ),
    );
  }
}
