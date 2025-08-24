import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';

class ConfigurationView extends StatefulWidget {
  final VoidCallback? onToggleTheme;

  const ConfigurationView({super.key, this.onToggleTheme});

  @override
  State<ConfigurationView> createState() => _ConfigurationViewState();
}

class _ConfigurationViewState extends State<ConfigurationView> {
  final _serverAddressController = TextEditingController(
    text: 'localhost:7666',
  );
  bool _isKeepAliveEnabled = true;

  @override
  void initState() {
    super.initState();
    // Request current configuration from Rust
    RequestConfig().sendSignalToRust();
  }

  @override
  void dispose() {
    _serverAddressController.dispose();
    super.dispose();
  }

  void _saveConfiguration() {
    UpdateConfigRequest(
      serverAddress: _serverAddressController.text,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();

    ScaffoldMessenger.of(
      context,
    ).showSnackBar(const SnackBar(content: Text('Configuration updated')));
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
                      'Environment Configuration',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    TextField(
                      controller: _serverAddressController,
                      decoration: const InputDecoration(
                        labelText: 'PB_MAPPER_SERVER',
                        hintText: 'localhost:7666',
                        border: OutlineInputBorder(),
                      ),
                    ),
                    const SizedBox(height: 16),
                    SwitchListTile(
                      title: const Text('PB_MAPPER_KEEP_ALIVE'),
                      value: _isKeepAliveEnabled,
                      onChanged: (value) {
                        setState(() => _isKeepAliveEnabled = value);
                      },
                    ),
                    const SizedBox(height: 16),
                    SwitchListTile(
                      title: const Text('Enable Dark Mode'),
                      value: Theme.of(context).brightness == Brightness.dark,
                      onChanged: widget.onToggleTheme != null
                          ? (value) {
                              widget.onToggleTheme!();
                            }
                          : null,
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
                onPressed: _saveConfiguration,
                style: ElevatedButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(24),
                  ),
                ),
                child: const Text(
                  'Save Configuration',
                  style: TextStyle(fontSize: 16),
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
                      'Current Configuration',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: ConfigStatusUpdate.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final config = snapshot.data!.message;
                          return Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text('Server Address: ${config.serverAddress}'),
                              Text(
                                'Keep-Alive Enabled: ${config.keepAliveEnabled ? 'Yes' : 'No'}',
                              ),
                            ],
                          );
                        }
                        return const Text('Fetching current configuration...');
                      },
                    ),
                    const SizedBox(height: 16),
                    Text(
                      'Note: Changes will apply after restarting the server.',
                      style: Theme.of(context).textTheme.bodySmall,
                      textAlign: TextAlign.center,
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
