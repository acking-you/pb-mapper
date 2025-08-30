import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';
import 'package:ui/src/views/status_monitoring_view.dart';
import 'package:ui/src/views/log_view_button.dart';

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

  @override
  void initState() {
    super.initState();
    // Request current configuration to get server address
    RequestConfig().sendSignalToRust();
    
    // Request server status to check availability
    RequestServerStatus().sendSignalToRust();
    
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
        });
      }
    });
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

    RegisterServiceRequest(
      serviceKey: _serviceKeyController.text,
      localAddress: _localAddressController.text,
      protocol: _selectedProtocol,
      enableEncryption: _isEncryptionEnabled,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();

    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text('Registering ${_serviceKeyController.text}...')),
    );
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
                      'Register Service',
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
                      onChanged: _serverAvailable ? (value) {
                        setState(() => _isEncryptionEnabled = value);
                      } : null,
                    ),
                    SwitchListTile(
                      title: const Text('Enable TCP Keep-Alive'),
                      value: _isKeepAliveEnabled,
                      onChanged: _serverAvailable ? (value) {
                        setState(() => _isKeepAliveEnabled = value);
                      } : null,
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
                onPressed: _serverAvailable ? _registerService : null,
                style: ElevatedButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(24),
                  ),
                  backgroundColor: !_serverAvailable ? Colors.grey : null,
                ),
                child: Text(
                  _serverAvailable ? 'Register Service' : 'Server Unavailable',
                  style: const TextStyle(fontSize: 16),
                ),
              ),
            ),
            const SizedBox(height: 24),
            // Replace the status card with log view button
            const LogViewButton(),
          ],
        ),
      ),
    );
  }
}
