import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/models/service_config.dart';
import 'package:pb_mapper_ui/src/widgets/service_card.dart';
import 'package:pb_mapper_ui/src/widgets/edit_service_dialog.dart';

class ServiceRegistrationView extends StatefulWidget {
  const ServiceRegistrationView({super.key});

  @override
  State<ServiceRegistrationView> createState() =>
      _ServiceRegistrationViewState();
}

class _ServiceRegistrationViewState extends State<ServiceRegistrationView> {
  final PbMapperApi _api = PbMapperApi();
  final _serviceKeyController = TextEditingController();
  final _localAddressController = TextEditingController(text: '127.0.0.1:8080');
  bool _isEncryptionEnabled = true;
  bool _isKeepAliveEnabled = true;
  String _selectedProtocol = 'TCP';
  String _serverAddress = 'localhost:7666'; // Will be updated from config
  bool _serverAvailable = false;
  bool _serverStatusRetryPending = false;
  List<ServiceConfig> _serviceConfigs = [];
  bool _isRegistering = false;

  @override
  void initState() {
    super.initState();
    _refreshPrerequisites();
    _loadServiceConfigs();
  }

  Future<void> _refreshPrerequisites() async {
    await Future.wait([_loadConfig(), _loadServerStatus()]);
  }

  Future<void> _loadConfig() async {
    try {
      final config = await _api.fetchConfig();
      if (!mounted) return;
      setState(() {
        _serverAddress = config.serverAddress;
        _isKeepAliveEnabled = config.keepAliveEnabled;
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _serverAddress = 'localhost:7666';
      });
    }
  }

  Future<void> _loadServerStatus() async {
    try {
      final status = await _api.getServerStatusDetail();
      if (!mounted) return;
      setState(() {
        _serverAvailable = status.serverAvailable;
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _serverAvailable = false;
      });
    }
    _scheduleServerStatusRetryIfNeeded();
  }

  void _scheduleServerStatusRetryIfNeeded() {
    if (_serverAvailable) {
      _serverStatusRetryPending = false;
      return;
    }
    if (_serverStatusRetryPending) {
      return;
    }
    _serverStatusRetryPending = true;
    Future.delayed(const Duration(seconds: 1), () {
      if (!mounted) return;
      _serverStatusRetryPending = false;
      if (!_serverAvailable) {
        _loadServerStatus();
      }
    });
  }

  Future<List<ServiceConfig>> _fetchServiceConfigs() async {
    try {
      final services = await _api.getServiceConfigs();
      final configs = services
          .map(
            (service) => ServiceConfig(
              serviceKey: service.serviceKey,
              localAddress: service.localAddress,
              protocol: service.protocol,
              enableEncryption: service.enableEncryption,
              enableKeepAlive: service.enableKeepAlive,
              status: _parseStatus(service.status),
              statusMessage: service.statusMessage,
              createdAt: DateTime.fromMillisecondsSinceEpoch(
                service.createdAtMs,
              ),
              updatedAt: DateTime.fromMillisecondsSinceEpoch(
                service.updatedAtMs,
              ),
            ),
          )
          .toList();

      configs.sort((a, b) => a.createdAt.compareTo(b.createdAt));
      return configs;
    } catch (_) {
      return [];
    }
  }

  Future<void> _loadServiceConfigs() async {
    final configs = await _fetchServiceConfigs();
    if (!mounted) return;
    setState(() {
      _serviceConfigs = configs;
    });
  }

  ServiceStatus _parseStatus(String statusString) {
    switch (statusString.toLowerCase()) {
      case 'running':
        return ServiceStatus.running;
      case 'retrying':
        return ServiceStatus.retrying;
      case 'failed':
        return ServiceStatus.failed;
      case 'stopped':
      default:
        return ServiceStatus.stopped;
    }
  }

  @override
  void dispose() {
    _serviceKeyController.dispose();
    _localAddressController.dispose();
    super.dispose();
  }

  Widget _buildServerUnavailableBanner() {
    return Card(
      color: Colors.amber.shade50,
      child: Padding(
        padding: const EdgeInsets.all(16.0),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Icon(Icons.warning_amber, color: Colors.orange),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const Text(
                    'No pb-mapper server is reachable.',
                    style: TextStyle(fontWeight: FontWeight.bold),
                  ),
                  const SizedBox(height: 6),
                  Text(
                    'Please configure a reachable server and matching '
                    'MSG_HEADER_KEY in the Config page before registering services.',
                    style: TextStyle(color: Colors.grey.shade700),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            ElevatedButton(
              onPressed: AppNavigationManager.navigateToConfigPage,
              child: const Text('Go to Config'),
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildSetupStepCard() {
    final statusColor = _serverAvailable ? Colors.green : Colors.red;
    final statusText = _serverAvailable ? 'Reachable' : 'Unreachable';

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                const Icon(Icons.looks_one, color: Colors.blueGrey),
                const SizedBox(width: 8),
                Text(
                  'Step 1: Configure Server / MSG_HEADER_KEY',
                  style: Theme.of(context).textTheme.titleMedium?.copyWith(
                    fontWeight: FontWeight.bold,
                  ),
                ),
                const Spacer(),
                Icon(Icons.circle, color: statusColor, size: 12),
                const SizedBox(width: 6),
                Text(
                  statusText,
                  style: TextStyle(
                    color: statusColor,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ],
            ),
            const SizedBox(height: 10),
            SelectableText(
              _serverAddress,
              style: Theme.of(
                context,
              ).textTheme.bodyLarge?.copyWith(fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                ElevatedButton.icon(
                  onPressed: AppNavigationManager.navigateToConfigPage,
                  icon: const Icon(Icons.settings),
                  label: const Text('Open Config'),
                ),
                OutlinedButton.icon(
                  onPressed: _refreshPrerequisites,
                  icon: const Icon(Icons.refresh),
                  label: const Text('Re-check Server'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _wrapIfUnavailable(bool unavailable, Widget child) {
    if (!unavailable) {
      return child;
    }
    return IgnorePointer(
      ignoring: true,
      child: Opacity(opacity: 0.5, child: child),
    );
  }

  void _registerService() {
    if (_serviceKeyController.text.isEmpty) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(const SnackBar(content: Text('Service key is required')));
      return;
    }

    // Check if service key already exists
    final existingConfig = _serviceConfigs.firstWhere(
      (config) => config.serviceKey == _serviceKeyController.text,
      orElse: () => ServiceConfig(
        serviceKey: '',
        localAddress: '',
        protocol: '',
        enableEncryption: false,
        enableKeepAlive: false,
      ),
    );

    if (existingConfig.serviceKey.isNotEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(
            'Service "${_serviceKeyController.text}" already exists',
          ),
        ),
      );
      return;
    }

    setState(() => _isRegistering = true);

    // Send registration request to Rust backend
    final serviceKey = _serviceKeyController.text;

    _api
        .registerService(
          serviceKey: serviceKey,
          localAddress: _localAddressController.text,
          protocol: _selectedProtocol,
          enableEncryption: _isEncryptionEnabled,
          enableKeepAlive: _isKeepAliveEnabled,
        )
        .then((result) {
          if (!mounted) return;
          if (!result.success) {
            setState(() => _isRegistering = false);
            ScaffoldMessenger.of(context).showSnackBar(
              SnackBar(
                content: Text(result.message),
                backgroundColor: Colors.red,
              ),
            );
            return;
          }

          ScaffoldMessenger.of(
            context,
          ).showSnackBar(SnackBar(content: Text('Registering $serviceKey...')));

          // Poll for registration status
          _pollRegistrationStatus(serviceKey);
        });
  }

  void _pollRegistrationStatus(String serviceKey) async {
    // Poll for up to 30 seconds to check registration status
    for (int i = 0; i < 30; i++) {
      await Future.delayed(Duration(seconds: 1));

      if (!mounted) return;

      // Check if service was successfully registered by loading configs
      final configs = await _fetchServiceConfigs();
      if (!mounted) return;
      setState(() {
        _serviceConfigs = configs;
      });

      final config = configs.firstWhere(
        (c) => c.serviceKey == serviceKey,
        orElse: () => ServiceConfig(
          serviceKey: '',
          localAddress: '',
          protocol: '',
          enableEncryption: false,
          enableKeepAlive: false,
        ),
      );

      if (config.serviceKey.isNotEmpty) {
        // Service was successfully saved, stop polling
        setState(() => _isRegistering = false);
        _clearForm();
        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text('Service "$serviceKey" registered successfully'),
            ),
          );
        }
        await _pollServiceStatusUntilStable(serviceKey);
        return;
      }
    }

    // Timeout after 30 seconds
    if (mounted) {
      setState(() => _isRegistering = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('Registration timeout for "$serviceKey"'),
          backgroundColor: Colors.red,
        ),
      );
    }
  }

  // Poll the native status cache for a short time so the UI reflects state changes quickly.
  Future<void> _pollServiceStatusUntilStable(String serviceKey) async {
    for (int i = 0; i < 10; i++) {
      await Future.delayed(const Duration(seconds: 1));
      if (!mounted) return;

      final configs = await _fetchServiceConfigs();
      if (!mounted) return;

      setState(() {
        _serviceConfigs = configs;
      });

      final config = configs.firstWhere(
        (c) => c.serviceKey == serviceKey,
        orElse: () => ServiceConfig(
          serviceKey: '',
          localAddress: '',
          protocol: '',
          enableEncryption: false,
          enableKeepAlive: false,
        ),
      );

      if (config.serviceKey.isEmpty) {
        continue;
      }

      if (config.status != ServiceStatus.retrying) {
        return;
      }
    }
  }

  void _clearForm() {
    _serviceKeyController.clear();
    _localAddressController.text = '127.0.0.1:8080';
    setState(() {
      _selectedProtocol = 'TCP';
      _isEncryptionEnabled = true;
      _isKeepAliveEnabled = true;
    });
  }

  void _editService(ServiceConfig config) async {
    final updatedConfig = await showDialog<ServiceConfig>(
      context: context,
      builder: (context) => EditServiceDialog(config: config),
    );

    if (updatedConfig != null) {
      // Check if the new service key already exists (exclude current service being edited)
      final existingConfig = _serviceConfigs.firstWhere(
        (c) =>
            c.serviceKey == updatedConfig.serviceKey &&
            c.serviceKey != config.serviceKey,
        orElse: () => ServiceConfig(
          serviceKey: '',
          localAddress: '',
          protocol: '',
          enableEncryption: false,
          enableKeepAlive: false,
        ),
      );

      if (existingConfig.serviceKey.isNotEmpty) {
        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text(
                'Service key "${updatedConfig.serviceKey}" already exists',
              ),
              backgroundColor: Colors.red,
            ),
          );
        }
        return;
      }

      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Testing new configuration...')),
        );
      }

      // Handle same-key edits: if service key hasn't changed, stop existing service first
      if (updatedConfig.serviceKey == config.serviceKey) {
        // Same key - need to unregister first, then register with new config
        final stopResult = await _api.unregisterService(config.serviceKey);
        if (!stopResult.success) {
          if (mounted) {
            ScaffoldMessenger.of(context).showSnackBar(
              SnackBar(
                content: Text(stopResult.message),
                backgroundColor: Colors.red,
              ),
            );
          }
          return;
        }

        // Wait a moment for unregister to complete
        await Future.delayed(Duration(milliseconds: 500));
      }

      // Register with new configuration
      final registerResult = await _api.registerService(
        serviceKey: updatedConfig.serviceKey,
        localAddress: updatedConfig.localAddress,
        protocol: updatedConfig.protocol,
        enableEncryption: updatedConfig.enableEncryption,
        enableKeepAlive: updatedConfig.enableKeepAlive,
      );

      if (!registerResult.success) {
        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text(registerResult.message),
              backgroundColor: Colors.red,
            ),
          );
        }
        return;
      }

      // Poll for test registration result
      _testAndUpdateService(config, updatedConfig);
    }
  }

  void _testAndUpdateService(
    ServiceConfig oldConfig,
    ServiceConfig newConfig,
  ) async {
    // Poll for up to 10 seconds to check if new configuration works
    for (int i = 0; i < 10; i++) {
      await Future.delayed(Duration(seconds: 1));

      if (!mounted) return;

      // Check if new service was successfully registered
      final configs = await _fetchServiceConfigs();
      if (!mounted) return;
      setState(() {
        _serviceConfigs = configs;
      });

      final testConfig = configs.firstWhere(
        (c) => c.serviceKey == newConfig.serviceKey,
        orElse: () => ServiceConfig(
          serviceKey: '',
          localAddress: '',
          protocol: '',
          enableEncryption: false,
          enableKeepAlive: false,
        ),
      );

      if (testConfig.serviceKey.isNotEmpty &&
          (testConfig.status == ServiceStatus.running ||
              testConfig.status == ServiceStatus.retrying)) {
        // New configuration works! Now clean up old service if it's different
        if (oldConfig.serviceKey != newConfig.serviceKey) {
          // Different service key, stop and delete old service
          await _api.unregisterService(oldConfig.serviceKey);
          await _api.deleteServiceConfig(oldConfig.serviceKey);
        }

        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text(
                'Service "${newConfig.serviceKey}" updated successfully',
              ),
              backgroundColor: Colors.green,
            ),
          );
        }
        return;
      }
    }

    // Test failed - stop the test service and show error
    await _api.unregisterService(newConfig.serviceKey);
    await _api.deleteServiceConfig(newConfig.serviceKey);

    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(
            'Failed to connect with new configuration for "${newConfig.serviceKey}"',
          ),
          backgroundColor: Colors.red,
        ),
      );
    }
  }

  void _deleteService(ServiceConfig config) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Delete Service'),
        content: Text(
          'Are you sure you want to delete "${config.serviceKey}"?',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Cancel'),
          ),
          ElevatedButton(
            onPressed: () => Navigator.of(context).pop(true),
            style: ElevatedButton.styleFrom(backgroundColor: Colors.red),
            child: const Text('Delete'),
          ),
        ],
      ),
    );

    if (confirmed == true) {
      // Delete the configuration completely (this will also stop the service)
      await _api.deleteServiceConfig(config.serviceKey);

      await _loadServiceConfigs();

      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text(
              'Service "${config.serviceKey}" configuration deleted',
            ),
          ),
        );
      }
    }
  }

  void _onServiceStatusChanged(ServiceConfig config) async {
    // Status changes are now handled through Rust backend
    // Just reload configs to get latest status
    _loadServiceConfigs();
  }

  void _startStopService(ServiceConfig config) async {
    if (config.status == ServiceStatus.running ||
        config.status == ServiceStatus.retrying) {
      // Stop the service
      final result = await _api.unregisterService(config.serviceKey);
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text(result.message),
            backgroundColor: result.success ? Colors.green : Colors.red,
          ),
        );
      }
    } else {
      // Start the service
      final result = await _api.registerService(
        serviceKey: config.serviceKey,
        localAddress: config.localAddress,
        protocol: config.protocol,
        enableEncryption: config.enableEncryption,
        enableKeepAlive: config.enableKeepAlive,
      );
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text(result.message),
            backgroundColor: result.success ? Colors.green : Colors.red,
          ),
        );
      }

      if (!result.success) {
        return;
      }

      await _pollServiceStatusUntilStable(config.serviceKey);
    }

    // Refresh status after a delay
    Future.delayed(Duration(seconds: 1), () {
      _loadServiceConfigs();
    });
  }

  void _refreshServiceStatus(ServiceConfig config) {
    // Refresh service status directly and reload list
    _api.getServiceStatus(config.serviceKey).then((_) => _loadServiceConfigs());

    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Refreshing status for "${config.serviceKey}"')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final bool disableUi = !_serverAvailable;
    return Padding(
      padding: const EdgeInsets.all(16.0),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _buildSetupStepCard(),
            const SizedBox(height: 12),
            if (disableUi) ...[
              _buildServerUnavailableBanner(),
              const SizedBox(height: 12),
            ],
            _wrapIfUnavailable(
              disableUi,
              Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Card(
                    child: Padding(
                      padding: const EdgeInsets.all(16.0),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            'Step 2: Register Service',
                            style: Theme.of(context).textTheme.titleLarge,
                          ),
                          const SizedBox(height: 16),
                          DropdownButtonFormField<String>(
                            initialValue: _selectedProtocol,
                            items: ['TCP', 'UDP']
                                .map(
                                  (protocol) => DropdownMenuItem(
                                    value: protocol,
                                    child: Text(protocol),
                                  ),
                                )
                                .toList(),
                            onChanged: (value) {
                              setState(() => _selectedProtocol = value!);
                            },
                            decoration: const InputDecoration(
                              labelText: 'Protocol',
                              border: OutlineInputBorder(),
                            ),
                          ),
                          const SizedBox(height: 16),
                          TextField(
                            controller: _serviceKeyController,
                            enabled: _serverAvailable,
                            decoration: InputDecoration(
                              labelText: 'Service Key',
                              hintText: _serverAvailable
                                  ? 'unique-service-key'
                                  : 'Server unavailable',
                              border: const OutlineInputBorder(),
                              prefixIcon: Icon(
                                _serverAvailable
                                    ? Icons.cloud_done
                                    : Icons.cloud_off,
                                color: _serverAvailable
                                    ? Colors.green
                                    : Colors.red,
                              ),
                            ),
                          ),
                          const SizedBox(height: 16),
                          TextField(
                            controller: _localAddressController,
                            enabled: _serverAvailable,
                            decoration: const InputDecoration(
                              labelText: 'Local Address',
                              hintText: '127.0.0.1:8080',
                              border: OutlineInputBorder(),
                            ),
                          ),
                          const SizedBox(height: 16),
                          SwitchListTile(
                            title: const Text('Enable Encryption'),
                            value: _isEncryptionEnabled,
                            onChanged: _serverAvailable
                                ? (value) {
                                    setState(
                                      () => _isEncryptionEnabled = value,
                                    );
                                  }
                                : null,
                          ),
                          SwitchListTile(
                            title: const Text('Enable TCP Keep-Alive'),
                            value: _isKeepAliveEnabled,
                            onChanged: _serverAvailable
                                ? (value) {
                                    setState(() => _isKeepAliveEnabled = value);
                                  }
                                : null,
                          ),
                          const SizedBox(height: 16),
                          Container(
                            padding: const EdgeInsets.all(12),
                            decoration: BoxDecoration(
                              border: Border.all(color: Colors.grey),
                              borderRadius: BorderRadius.circular(8),
                            ),
                            child: Row(
                              children: [
                                const Icon(Icons.dns, color: Colors.blue),
                                const SizedBox(width: 12),
                                Expanded(
                                  child: Column(
                                    crossAxisAlignment:
                                        CrossAxisAlignment.start,
                                    children: [
                                      const Text(
                                        'Server Address',
                                        style: TextStyle(
                                          fontSize: 12,
                                          color: Colors.grey,
                                        ),
                                      ),
                                      const SizedBox(height: 4),
                                      Text(
                                        _serverAddress,
                                        style: const TextStyle(fontSize: 16),
                                      ),
                                    ],
                                  ),
                                ),
                                TextButton(
                                  onPressed: () {
                                    AppNavigationManager.navigateToConfigPage();
                                  },
                                  child: const Text(
                                    'Configure in Settings',
                                    style: TextStyle(fontSize: 12),
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                  const SizedBox(height: 16),
                  SizedBox(
                    height: 48,
                    width: double.infinity,
                    child: ElevatedButton(
                      onPressed: (_serverAvailable && !_isRegistering)
                          ? _registerService
                          : null,
                      style: ElevatedButton.styleFrom(
                        shape: RoundedRectangleBorder(
                          borderRadius: BorderRadius.circular(24),
                        ),
                        backgroundColor: !_serverAvailable ? Colors.grey : null,
                      ),
                      child: _isRegistering
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
                                  'Registering...',
                                  style: TextStyle(fontSize: 16),
                                ),
                              ],
                            )
                          : Text(
                              _serverAvailable
                                  ? 'Register & Start'
                                  : 'Server Unavailable',
                              style: const TextStyle(fontSize: 16),
                            ),
                    ),
                  ),
                  const SizedBox(height: 24),
                  if (_serviceConfigs.isNotEmpty) ...[
                    Card(
                      child: Padding(
                        padding: const EdgeInsets.all(16.0),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Row(
                              children: [
                                const Icon(Icons.dns, color: Colors.blue),
                                const SizedBox(width: 8),
                                Expanded(
                                  child: Text(
                                    'Registered Services (${_serviceConfigs.length})',
                                    overflow: TextOverflow.ellipsis,
                                    style: Theme.of(context)
                                        .textTheme
                                        .titleMedium
                                        ?.copyWith(fontWeight: FontWeight.bold),
                                  ),
                                ),
                                TextButton.icon(
                                  onPressed: AppNavigationManager
                                      .navigateToConnectPage,
                                  icon: const Icon(Icons.cable),
                                  label: const Text('Open Connect'),
                                ),
                              ],
                            ),
                            const SizedBox(height: 16),
                            ..._serviceConfigs.map(
                              (config) => ServiceCard(
                                key: Key(config.serviceKey),
                                config: config,
                                onEdit: () => _editService(config),
                                onDelete: () => _deleteService(config),
                                onStartStop: () => _startStopService(config),
                                onRefresh: () => _refreshServiceStatus(config),
                                onStatusChanged: _onServiceStatusChanged,
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                    const SizedBox(height: 16),
                  ],
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
