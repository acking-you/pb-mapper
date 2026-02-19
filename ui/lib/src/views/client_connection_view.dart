import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/models/client_config.dart';
import 'package:pb_mapper_ui/src/widgets/client_card.dart';

class ClientConnectionView extends StatefulWidget {
  const ClientConnectionView({super.key});

  @override
  State<ClientConnectionView> createState() => _ClientConnectionViewState();
}

class _ClientConnectionViewState extends State<ClientConnectionView> {
  final PbMapperApi _api = PbMapperApi();
  final _localAddressController = TextEditingController(text: '127.0.0.1:9090');
  final _serviceKeyInputController = TextEditingController();
  bool _isKeepAliveEnabled = true;
  String _selectedProtocol = 'TCP';
  String _serverAddress = 'localhost:7666'; // Will be updated from config
  String? _selectedServiceKey;
  List<String> _availableServices = [];
  List<ClientConfig> _clientConfigs = [];
  bool _isLoading = true;

  @override
  void initState() {
    super.initState();
    _loadClientConfigs();
    _loadConfig();
    _loadAvailableServices();
    _checkForPreSelectedService();
  }

  Future<void> _loadConfig() async {
    try {
      final config = await _api.fetchConfig();
      if (!mounted) return;
      setState(() {
        _serverAddress = config.serverAddress;
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _serverAddress = 'localhost:7666';
      });
    }
  }

  Future<void> _loadAvailableServices() async {
    try {
      final status = await _api.forceRefreshServerStatus();
      if (!mounted) return;
      setState(() {
        _availableServices = status.registeredServices;
        if (_selectedServiceKey != null &&
            !_availableServices.contains(_selectedServiceKey)) {
          _selectedServiceKey = null;
        }
        if (_selectedServiceKey == null && _availableServices.isNotEmpty) {
          _selectedServiceKey = _availableServices.first;
        }
      });
    } catch (e) {
      // Silently fail — user can still type service key manually
      debugPrint('Failed to load available services: $e');
    }
  }

  Future<void> _loadClientConfigs() async {
    try {
      final configs = await _api.getClientConfigs();
      if (!mounted) return;
      _updateClientConfigsFromSignal(configs);
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _clientConfigs = [];
        _isLoading = false;
      });
    }
  }

  void _updateClientConfigsFromSignal(List<ClientConfigInfo> configs) {
    final clientConfigs = configs.map((config) {
      final status = _parseClientStatus(config.status);
      return ClientConfig(
        serviceKey: config.serviceKey,
        localAddress: config.localAddress,
        protocol: config.protocol,
        enableKeepAlive: config.enableKeepAlive,
        createdAt: DateTime.fromMillisecondsSinceEpoch(
          config.createdAtMs.toInt(),
        ),
        updatedAt: DateTime.fromMillisecondsSinceEpoch(
          config.updatedAtMs.toInt(),
        ),
        status: status,
        statusMessage: config.statusMessage,
      );
    }).toList();

    setState(() {
      _clientConfigs = clientConfigs;
      _isLoading = false;
    });
  }

  ClientStatus _parseClientStatus(String statusString) {
    switch (statusString.toLowerCase()) {
      case 'running':
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

  void _checkForPreSelectedService() {
    // Check if there's a service key selected from status monitoring
    final selectedKey = ServiceKeyManager.getSelectedServiceKey();
    if (selectedKey != null) {
      _selectedServiceKey = selectedKey;
      _serviceKeyInputController.text = selectedKey;
      ServiceKeyManager.clearSelectedServiceKey();

      // Show a helpful message
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text(
                'Service key "$selectedKey" auto-selected from Status page',
              ),
              backgroundColor: Colors.green,
              duration: const Duration(seconds: 3),
            ),
          );
        }
      });
    }
  }

  @override
  void dispose() {
    _localAddressController.dispose();
    _serviceKeyInputController.dispose();
    super.dispose();
  }

  String get _effectiveServiceKey {
    if (_selectedServiceKey != null && _selectedServiceKey!.isNotEmpty) {
      return _selectedServiceKey!;
    }
    return _serviceKeyInputController.text.trim();
  }

  void _connectService() {
    final serviceKey = _effectiveServiceKey;
    if (serviceKey.isEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Please select or enter a service key')),
      );
      return;
    }

    // Check if client already exists
    final existingClient = _clientConfigs.firstWhere(
      (client) => client.serviceKey == serviceKey,
      orElse: () => ClientConfig(
        serviceKey: '',
        localAddress: '',
        protocol: '',
        enableKeepAlive: false,
      ),
    );

    if (existingClient.serviceKey.isNotEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('Client for "$serviceKey" already exists'),
          backgroundColor: Colors.orange,
          duration: const Duration(seconds: 3),
        ),
      );
      return;
    }

    _api
        .connectService(
          serviceKey: serviceKey,
          localAddress: _localAddressController.text,
          protocol: _selectedProtocol,
          enableKeepAlive: _isKeepAliveEnabled,
        )
        .then((result) {
          if (!mounted) return;
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text(result.message),
              backgroundColor: result.success ? Colors.green : Colors.red,
              duration: const Duration(seconds: 3),
            ),
          );
          _loadClientConfigs();
          if (result.success) {
            _pollClientStatusUntilStable(serviceKey);
          }
        });
  }

  void _handleClientConnect(ClientConfig config) {
    _api
        .connectService(
          serviceKey: config.serviceKey,
          localAddress: config.localAddress,
          protocol: config.protocol,
          enableKeepAlive: config.enableKeepAlive,
        )
        .then((result) {
          if (!mounted) return;
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text(result.message),
              backgroundColor: result.success ? Colors.green : Colors.red,
              duration: const Duration(seconds: 2),
            ),
          );
          _loadClientConfigs();
          if (result.success) {
            _pollClientStatusUntilStable(config.serviceKey);
          }
        });
  }

  // Poll the native status cache for a short time so the UI reflects state changes quickly.
  Future<void> _pollClientStatusUntilStable(String serviceKey) async {
    for (int i = 0; i < 10; i++) {
      await Future.delayed(const Duration(seconds: 1));
      if (!mounted) return;

      await _loadClientConfigs();

      final config = _clientConfigs.firstWhere(
        (c) => c.serviceKey == serviceKey,
        orElse: () => ClientConfig(
          serviceKey: '',
          localAddress: '',
          protocol: '',
          enableKeepAlive: false,
        ),
      );

      if (config.serviceKey.isEmpty) {
        continue;
      }

      if (config.status != ClientStatus.retrying) {
        return;
      }
    }
  }

  void _handleClientDisconnect(ClientConfig config) {
    _api.disconnectService(config.serviceKey).then((result) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(result.message),
          backgroundColor: result.success ? Colors.green : Colors.red,
          duration: const Duration(seconds: 2),
        ),
      );
      _loadClientConfigs();
    });
  }

  void _handleClientDelete(ClientConfig config) {
    showDialog(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Delete Client Configuration'),
        content: Text(
          'Are you sure you want to delete the client configuration for "${config.serviceKey}"?\n\nThis will permanently remove the configuration.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('Cancel'),
          ),
          TextButton(
            onPressed: () {
              Navigator.of(context).pop();
              final messenger = ScaffoldMessenger.of(context);
              _api.deleteClientConfig(config.serviceKey).then((result) {
                if (!mounted) return;
                messenger.showSnackBar(
                  SnackBar(
                    content: Text(result.message),
                    backgroundColor: result.success ? Colors.green : Colors.red,
                  ),
                );
                _loadClientConfigs();
              });
            },
            style: TextButton.styleFrom(foregroundColor: Colors.red),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
  }

  void _handleClientRefresh(ClientConfig config) {
    _api.getClientStatus(config.serviceKey).then((status) {
      if (!mounted) return;
      final configIndex = _clientConfigs.indexWhere(
        (c) => c.serviceKey == status.serviceKey,
      );
      if (configIndex != -1) {
        setState(() {
          _clientConfigs[configIndex].updateStatus(
            _parseClientStatus(status.status),
            status.message,
          );
        });
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16.0),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16.0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Connect to Service',
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
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Expanded(
                          child: _availableServices.isNotEmpty
                              ? DropdownButtonFormField<String>(
                                  initialValue:
                                      _availableServices.contains(_selectedServiceKey)
                                      ? _selectedServiceKey
                                      : null,
                                  items: _availableServices.map((serviceKey) {
                                    return DropdownMenuItem(
                                      value: serviceKey,
                                      child: Text(serviceKey),
                                    );
                                  }).toList(),
                                  onChanged: (value) {
                                    setState(() {
                                      _selectedServiceKey = value;
                                      _serviceKeyInputController.text = value ?? '';
                                    });
                                  },
                                  decoration: const InputDecoration(
                                    labelText: 'Service Key',
                                    hintText: 'Select a service',
                                    border: OutlineInputBorder(),
                                    prefixIcon: Icon(Icons.vpn_key),
                                  ),
                                )
                              : TextField(
                                  controller: _serviceKeyInputController,
                                  onChanged: (value) {
                                    setState(() => _selectedServiceKey = value.trim().isEmpty ? null : value.trim());
                                  },
                                  decoration: const InputDecoration(
                                    labelText: 'Service Key',
                                    hintText: 'Enter service key',
                                    border: OutlineInputBorder(),
                                    prefixIcon: Icon(Icons.vpn_key),
                                  ),
                                ),
                        ),
                        const SizedBox(width: 8),
                        IconButton(
                          onPressed: _loadAvailableServices,
                          icon: const Icon(Icons.refresh),
                          tooltip: 'Refresh service list',
                        ),
                      ],
                    ),
                    const SizedBox(height: 16),
                    TextField(
                      controller: _localAddressController,
                      decoration: const InputDecoration(
                        labelText: 'Local Address',
                        hintText: '127.0.0.1:9090',
                        border: OutlineInputBorder(),
                      ),
                    ),
                    const SizedBox(height: 16),
                    SwitchListTile(
                      title: const Text('Enable TCP Keep-Alive'),
                      value: _isKeepAliveEnabled,
                      onChanged: (value) {
                        setState(() => _isKeepAliveEnabled = value);
                      },
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
                onPressed: _effectiveServiceKey.isNotEmpty
                    ? _connectService
                    : null,
                style: ElevatedButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(24),
                  ),
                ),
                child: const Text(
                  'Connect',
                  style: TextStyle(fontSize: 16),
                ),
              ),
            ),
            const SizedBox(height: 16),
            if (_isLoading) ...[
              const SizedBox(height: 24),
              const Center(child: CircularProgressIndicator()),
            ] else if (_clientConfigs.isEmpty) ...[
              const SizedBox(height: 24),
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(24),
                  child: Column(
                    children: [
                      Icon(
                        Icons.link_off,
                        size: 48,
                        color: Colors.grey[400],
                      ),
                      const SizedBox(height: 16),
                      Text(
                        'No client configurations',
                        style: Theme.of(context).textTheme.titleMedium
                            ?.copyWith(color: Colors.grey[600]),
                      ),
                      const SizedBox(height: 8),
                      Text(
                        'Create a new connection above to get started',
                        style: Theme.of(context).textTheme.bodySmall
                            ?.copyWith(color: Colors.grey[500]),
                      ),
                    ],
                  ),
                ),
              ),
            ] else ...[
              const SizedBox(height: 24),
              Row(
                children: [
                  Text(
                    'Active Connections',
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                  const Spacer(),
                  IconButton(
                    onPressed: _loadClientConfigs,
                    icon: const Icon(Icons.refresh),
                    tooltip: 'Refresh All Status',
                  ),
                ],
              ),
              const SizedBox(height: 8),
              ..._clientConfigs.map(
                (config) => ClientCard(
                  key: Key(config.serviceKey),
                  config: config,
                  onConnectDisconnect: () =>
                      config.status == ClientStatus.running ||
                              config.status == ClientStatus.retrying
                      ? _handleClientDisconnect(config)
                      : _handleClientConnect(config),
                  onDelete: () => _handleClientDelete(config),
                  onRefresh: () => _handleClientRefresh(config),
                  onStatusChanged: (updatedConfig) {
                    final index = _clientConfigs.indexWhere(
                      (c) => c.serviceKey == updatedConfig.serviceKey,
                    );
                    if (index != -1) {
                      setState(() {
                        _clientConfigs[index] = updatedConfig;
                      });
                    }
                  },
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
