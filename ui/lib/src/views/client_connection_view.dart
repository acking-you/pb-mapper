import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';
import 'package:ui/src/views/status_monitoring_view.dart';
import 'package:ui/src/views/log_view_button.dart';

class ClientConnectionView extends StatefulWidget {
  const ClientConnectionView({super.key});

  @override
  State<ClientConnectionView> createState() => _ClientConnectionViewState();
}

class _ClientConnectionViewState extends State<ClientConnectionView> {
  final _serviceKeyController = TextEditingController();
  final _localAddressController = TextEditingController(text: '127.0.0.1:9090');
  bool _isKeepAliveEnabled = true;
  String _selectedProtocol = 'TCP';
  String _serverAddress = 'localhost:7666'; // Will be updated from config
  bool _isConnected = false;
  String? _selectedServiceKey;
  List<String> _availableServices = [];
  bool _serverAvailable = false;

  @override
  void initState() {
    super.initState();
    // Request current configuration to get server address
    RequestConfig().sendSignalToRust();
    
    // Request server status to get available services
    RequestServerStatus().sendSignalToRust();
    
    // Check if there's a pre-selected service key from status monitoring
    _checkForPreSelectedService();
    
    // Listen for config updates
    ConfigStatusUpdate.rustSignalStream.listen((signal) {
      if (mounted) {
        setState(() {
          _serverAddress = signal.message.serverAddress;
        });
      }
    });
    
    // Listen for server status updates
    ServerStatusDetailUpdate.rustSignalStream.listen((signal) {
      if (mounted) {
        setState(() {
          _serverAvailable = signal.message.serverAvailable;
          _availableServices = List<String>.from(signal.message.registeredServices);
          
          // Clear selected service if it's no longer available
          if (_selectedServiceKey != null && !_availableServices.contains(_selectedServiceKey)) {
            _selectedServiceKey = null;
          }
        });
      }
    });
  }

  void _checkForPreSelectedService() {
    // Check if there's a service key selected from status monitoring
    final selectedKey = ServiceKeyManager.getSelectedServiceKey();
    if (selectedKey != null) {
      _selectedServiceKey = selectedKey;
      ServiceKeyManager.clearSelectedServiceKey();
      
      // Show a helpful message
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text('Service key "$selectedKey" auto-selected from Status page'),
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
    _serviceKeyController.dispose();
    _localAddressController.dispose();
    super.dispose();
  }

  void _connectService() {
    if (_selectedServiceKey == null || _selectedServiceKey!.isEmpty) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(const SnackBar(content: Text('Please select a service key')));
      return;
    }

    ConnectServiceRequest(
      serviceKey: _selectedServiceKey!,
      localAddress: _localAddressController.text,
      protocol: _selectedProtocol,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();

    setState(() => _isConnected = true);
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text('Connecting to $_selectedServiceKey...')),
    );
  }

  void _disconnectService() {
    DisconnectServiceRequest(
      serviceKey: _selectedServiceKey ?? '',
    ).sendSignalToRust();

    setState(() => _isConnected = false);
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(const SnackBar(content: Text('Disconnected')));
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
                    DropdownButtonFormField<String>(
                      value: _availableServices.contains(_selectedServiceKey) ? _selectedServiceKey : null,
                      items: _availableServices.isEmpty 
                          ? [
                              const DropdownMenuItem(
                                value: null,
                                child: Text('No services available'),
                              )
                            ]
                          : _availableServices.map((serviceKey) {
                              return DropdownMenuItem(
                                value: serviceKey,
                                child: Text(serviceKey),
                              );
                            }).toList(),
                      onChanged: _serverAvailable && _availableServices.isNotEmpty 
                          ? (value) {
                              setState(() => _selectedServiceKey = value);
                            }
                          : null,
                      decoration: InputDecoration(
                        labelText: 'Service Key',
                        hintText: _serverAvailable
                            ? (_availableServices.isEmpty 
                                ? 'No registered services'
                                : 'Select a service')
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
                              crossAxisAlignment: CrossAxisAlignment.start,
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
                              style: TextStyle(
                                fontSize: 12,
                              ),
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
                onPressed: (_serverAvailable && _selectedServiceKey != null && _availableServices.isNotEmpty)
                    ? (_isConnected ? _disconnectService : _connectService)
                    : null,
                style: ElevatedButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(24),
                  ),
                  backgroundColor: !_serverAvailable || _availableServices.isEmpty
                      ? Colors.grey
                      : null,
                ),
                child: Text(
                  _isConnected 
                      ? 'Disconnect' 
                      : (!_serverAvailable 
                          ? 'Server Unavailable' 
                          : (_availableServices.isEmpty 
                              ? 'No Services Available'
                              : 'Connect')),
                  style: const TextStyle(fontSize: 16),
                ),
              ),
            ),
            const SizedBox(height: 24),
            // Replace the connection status card with log view button
            const LogViewButton(),
          ],
        ),
      ),
    );
  }
}
