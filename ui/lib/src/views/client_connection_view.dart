import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';

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
  String _serverAddress = 'PB_MAPPER_SERVER';
  bool _isConnected = false;

  @override
  void dispose() {
    _serviceKeyController.dispose();
    _localAddressController.dispose();
    super.dispose();
  }

  void _connectService() {
    if (_serviceKeyController.text.isEmpty) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(const SnackBar(content: Text('Service key is required')));
      return;
    }

    ConnectServiceRequest(
      serviceKey: _serviceKeyController.text,
      localAddress: _localAddressController.text,
      protocol: _selectedProtocol,
      serverAddress: _serverAddress,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();

    setState(() => _isConnected = true);
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text('Connecting to ${_serviceKeyController.text}...')),
    );
  }

  void _disconnectService() {
    DisconnectServiceRequest(
      serviceKey: _serviceKeyController.text,
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
                        hintText: 'service-to-connect',
                        border: OutlineInputBorder(),
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
            SizedBox(
              height: 48,
              width: double.infinity,
              child: ElevatedButton(
                onPressed: _isConnected ? _disconnectService : _connectService,
                style: ElevatedButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(24),
                  ),
                ),
                child: Text(
                  _isConnected ? 'Disconnect' : 'Connect',
                  style: const TextStyle(fontSize: 16),
                ),
              ),
            ),
            const SizedBox(height: 24),
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16.0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Connection Status',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: ClientConnectionStatus.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final status = snapshot.data!.message;
                          return Text('Status: ${status.status}');
                        }
                        return const Text('Status: Not connected');
                      },
                    ),
                    const SizedBox(height: 16),
                    const Text(
                      'Connection Logs:',
                      style: TextStyle(fontWeight: FontWeight.bold),
                    ),
                    const SizedBox(height: 8),
                    const SingleChildScrollView(
                      child: Text(
                        'Connection logs will appear here...',
                        textAlign: TextAlign.left,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
