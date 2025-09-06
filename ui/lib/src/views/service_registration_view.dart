import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/bindings/bindings.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/views/log_view_button.dart';
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
  final _serviceKeyController = TextEditingController();
  final _localAddressController = TextEditingController(text: '127.0.0.1:8080');
  bool _isEncryptionEnabled = true;
  bool _isKeepAliveEnabled = true;
  String _selectedProtocol = 'TCP';
  String _serverAddress = 'localhost:7666'; // Will be updated from config
  bool _serverAvailable = false;
  List<ServiceConfig> _serviceConfigs = [];
  bool _isRegistering = false;
  // Prevent duplicate error popups when the same failed status re-builds.
  String? _lastRegistrationErrorKey;

  @override
  void initState() {
    super.initState();
    _loadServiceConfigs();

    // Request current configuration to get server address
    const RequestConfig().sendSignalToRust();

    // Request server status to check availability
    const RequestServerStatus().sendSignalToRust();
  }

  Future<void> _loadServiceConfigs() async {
    // Request service configs from Rust backend
    RequestServiceConfigs().sendSignalToRust();
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

    RegisterServiceRequest(
      serviceKey: serviceKey,
      localAddress: _localAddressController.text,
      protocol: _selectedProtocol,
      enableEncryption: _isEncryptionEnabled,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();

    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text('Registering $serviceKey...')));

    // Poll for registration status
    _pollRegistrationStatus(serviceKey);
  }

  void _pollRegistrationStatus(String serviceKey) async {
    // Poll for up to 30 seconds to check registration status
    for (int i = 0; i < 30; i++) {
      await Future.delayed(Duration(seconds: 1));

      if (!mounted) return;

      // Check if service was successfully registered by loading configs
      _loadServiceConfigs();

      // Wait a bit for the response
      await Future.delayed(Duration(milliseconds: 500));

      final config = _serviceConfigs.firstWhere(
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
        UnregisterServiceRequest(
          serviceKey: config.serviceKey,
        ).sendSignalToRust();

        // Wait a moment for unregister to complete
        await Future.delayed(Duration(milliseconds: 500));
      }

      // Register with new configuration
      RegisterServiceRequest(
        serviceKey: updatedConfig.serviceKey,
        localAddress: updatedConfig.localAddress,
        protocol: updatedConfig.protocol,
        enableEncryption: updatedConfig.enableEncryption,
        enableKeepAlive: updatedConfig.enableKeepAlive,
      ).sendSignalToRust();

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
      _loadServiceConfigs();
      await Future.delayed(Duration(milliseconds: 500));

      final testConfig = _serviceConfigs.firstWhere(
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
          UnregisterServiceRequest(
            serviceKey: oldConfig.serviceKey,
          ).sendSignalToRust();
          DeleteServiceConfigRequest(
            serviceKey: oldConfig.serviceKey,
          ).sendSignalToRust();
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
    UnregisterServiceRequest(
      serviceKey: newConfig.serviceKey,
    ).sendSignalToRust();
    DeleteServiceConfigRequest(
      serviceKey: newConfig.serviceKey,
    ).sendSignalToRust();

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
      DeleteServiceConfigRequest(
        serviceKey: config.serviceKey,
      ).sendSignalToRust();

      _loadServiceConfigs();

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
      UnregisterServiceRequest(
        serviceKey: config.serviceKey,
      ).sendSignalToRust();
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Stopping service "${config.serviceKey}"...')),
        );
      }
    } else {
      // Start the service
      RegisterServiceRequest(
        serviceKey: config.serviceKey,
        localAddress: config.localAddress,
        protocol: config.protocol,
        enableEncryption: config.enableEncryption,
        enableKeepAlive: config.enableKeepAlive,
      ).sendSignalToRust();
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Starting service "${config.serviceKey}"...')),
        );
      }
    }

    // Refresh status after a delay
    Future.delayed(Duration(seconds: 1), () {
      _loadServiceConfigs();
    });
  }

  void _refreshServiceStatus(ServiceConfig config) {
    // Request individual service status
    RequestServiceStatus(serviceKey: config.serviceKey).sendSignalToRust();

    // Also refresh the whole list
    _loadServiceConfigs();

    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Refreshing status for "${config.serviceKey}"')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16.0),
      child: SingleChildScrollView(
        child: StreamBuilder(
          stream: ConfigStatusUpdate.rustSignalStream,
          builder: (context, configSnapshot) {
            // Update server address when config data is available
            if (configSnapshot.hasData) {
              final config = configSnapshot.data!.message;
              WidgetsBinding.instance.addPostFrameCallback((_) {
                if (mounted && _serverAddress != config.serverAddress) {
                  setState(() {
                    _serverAddress = config.serverAddress;
                  });
                }
              });
            }

            return StreamBuilder(
              stream: ServerStatusDetailUpdate.rustSignalStream,
              builder: (context, serverSnapshot) {
                // Update server availability when server status data is available
                if (serverSnapshot.hasData) {
                  final status = serverSnapshot.data!.message;
                  WidgetsBinding.instance.addPostFrameCallback((_) {
                    if (mounted && _serverAvailable != status.serverAvailable) {
                      setState(() {
                        _serverAvailable = status.serverAvailable;
                      });
                    }
                  });
                }

                return StreamBuilder(
                  stream: ServiceConfigsUpdate.rustSignalStream,
                  builder: (context, serviceSnapshot) {
                    // Update service configs when service data is available
                    if (serviceSnapshot.hasData) {
                      final configs = serviceSnapshot.data!.message.services
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
                                service.createdAtMs.toInt(),
                              ),
                              updatedAt: DateTime.fromMillisecondsSinceEpoch(
                                service.updatedAtMs.toInt(),
                              ),
                            ),
                          )
                          .toList();

                      // Sort configs by creation time to ensure consistent ordering
                      configs.sort(
                        (a, b) => a.createdAt.compareTo(b.createdAt),
                      );

                      WidgetsBinding.instance.addPostFrameCallback((_) {
                        if (mounted) {
                          setState(() {
                            _serviceConfigs = configs;
                          });
                        }
                      });
                    }

                    return StreamBuilder(
                      stream: ServiceRegistrationStatusUpdate.rustSignalStream,
                      builder: (context, registrationSnapshot) {
                        // Handle service registration status updates (including errors)
                        if (registrationSnapshot.hasData) {
                          final signal = registrationSnapshot.data!.message;
                          if (signal.status == 'failed') {
                            final errorKey =
                                '${signal.serviceKey}|${signal.message}';
                            if (_lastRegistrationErrorKey != errorKey) {
                              _lastRegistrationErrorKey = errorKey;
                              WidgetsBinding.instance.addPostFrameCallback((_) {
                                if (mounted) {
                                  ScaffoldMessenger.of(context).showSnackBar(
                                    SnackBar(
                                      content: Text(
                                        '${signal.serviceKey}: ${signal.message}',
                                      ),
                                      backgroundColor: Colors.red,
                                    ),
                                  );
                                }
                              });
                            }
                          }
                        }

                        return Column(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            Card(
                              child: Padding(
                                padding: const EdgeInsets.all(16.0),
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      'Register Service',
                                      style: Theme.of(
                                        context,
                                      ).textTheme.titleLarge,
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
                                        setState(
                                          () => _selectedProtocol = value!,
                                        );
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
                                                () => _isEncryptionEnabled =
                                                    value,
                                              );
                                            }
                                          : null,
                                    ),
                                    SwitchListTile(
                                      title: const Text(
                                        'Enable TCP Keep-Alive',
                                      ),
                                      value: _isKeepAliveEnabled,
                                      onChanged: _serverAvailable
                                          ? (value) {
                                              setState(
                                                () =>
                                                    _isKeepAliveEnabled = value,
                                              );
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
                                          const Icon(
                                            Icons.dns,
                                            color: Colors.blue,
                                          ),
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
                                                  style: const TextStyle(
                                                    fontSize: 16,
                                                  ),
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
                                  backgroundColor: !_serverAvailable
                                      ? Colors.grey
                                      : null,
                                ),
                                child: _isRegistering
                                    ? const Row(
                                        mainAxisAlignment:
                                            MainAxisAlignment.center,
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
                                            ? 'Register Service'
                                            : 'Server Unavailable',
                                        style: const TextStyle(fontSize: 16),
                                      ),
                              ),
                            ),
                            const SizedBox(height: 24),

                            // Service Cards Section
                            if (_serviceConfigs.isNotEmpty) ...[
                              Card(
                                child: Padding(
                                  padding: const EdgeInsets.all(16.0),
                                  child: Column(
                                    crossAxisAlignment:
                                        CrossAxisAlignment.start,
                                    children: [
                                      Row(
                                        children: [
                                          const Icon(
                                            Icons.dns,
                                            color: Colors.blue,
                                          ),
                                          const SizedBox(width: 8),
                                          Text(
                                            'Registered Services (${_serviceConfigs.length})',
                                            style: Theme.of(context)
                                                .textTheme
                                                .titleMedium
                                                ?.copyWith(
                                                  fontWeight: FontWeight.bold,
                                                ),
                                          ),
                                        ],
                                      ),
                                      const SizedBox(height: 16),
                                      ..._serviceConfigs.map(
                                        (config) => ServiceCard(
                                          key: Key(
                                            config.serviceKey,
                                          ), // Add unique key for each card
                                          config: config,
                                          onEdit: () => _editService(config),
                                          onDelete: () =>
                                              _deleteService(config),
                                          onStartStop: () =>
                                              _startStopService(config),
                                          onRefresh: () =>
                                              _refreshServiceStatus(config),
                                          onStatusChanged:
                                              _onServiceStatusChanged,
                                        ),
                                      ),
                                    ],
                                  ),
                                ),
                              ),
                              const SizedBox(height: 16),
                            ],

                            // Log view button
                            const LogViewButton(),
                          ],
                        );
                      },
                    );
                  },
                );
              },
            );
          },
        ),
      ),
    );
  }
}
