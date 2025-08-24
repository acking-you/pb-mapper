import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';

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
  String _serverAddress = 'PB_MAPPER_SERVER';

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
      serverAddress: _serverAddress,
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
                    value: _selectedProtocol,
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
                    decoration: const InputDecoration(
                      labelText: 'Service Key',
                      hintText: 'unique-service-key',
                      border: OutlineInputBorder(),
                    ),
                  ),
                  const SizedBox(height: 16),
                  TextField(
                    controller: _localAddressController,
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
                    onChanged: (value) {
                      setState(() => _isEncryptionEnabled = value);
                    },
                  ),
                  SwitchListTile(
                    title: const Text('Enable TCP Keep-Alive'),
                    value: _isKeepAliveEnabled,
                    onChanged: (value) {
                      setState(() => _isKeepAliveEnabled = value);
                    },
                  ),
                  const SizedBox(height: 16),
                  TextField(
                    decoration: InputDecoration(
                      labelText: 'Server Address',
                      hintText: _serverAddress,
                      border: const OutlineInputBorder(),
                      suffixIcon: IconButton(
                        icon: const Icon(Icons.edit),
                        onPressed: () {
                          // TODO: Implement server address configuration
                        },
                      ),
                    ),
                    readOnly: true,
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 16),
          ElevatedButton(
            onPressed: _registerService,
            child: const Text('Register Service'),
          ),
          const SizedBox(height: 24),
          Expanded(
            child: Card(
              child: Padding(
                padding: const EdgeInsets.all(16.0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Registered Services',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: ServiceStatusUpdate.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final status = snapshot.data!.message;
                          return Text('Status: ${status.message}');
                        }
                        return const Text('No services registered');
                      },
                    ),
                    const SizedBox(height: 16),
                    Expanded(
                      child: ListView(
                        children: [
                          // This will be populated by the StreamBuilder
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
