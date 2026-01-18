import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';

class ConfigurationView extends StatefulWidget {
  final VoidCallback? onToggleTheme;

  const ConfigurationView({super.key, this.onToggleTheme});

  @override
  State<ConfigurationView> createState() => _ConfigurationViewState();
}

class _ConfigurationViewState extends State<ConfigurationView> {
  final PbMapperApi _api = PbMapperApi();
  final _serverAddressController = TextEditingController(
    text: 'localhost:7666',
  );
  bool _isKeepAliveEnabled = true;
  bool _isSaving = false;
  ConfigStatus? _currentConfig;

  @override
  void initState() {
    super.initState();
    _loadConfig();
  }

  @override
  void dispose() {
    _serverAddressController.dispose();
    super.dispose();
  }

  Future<void> _loadConfig() async {
    final config = await _api.fetchConfig();
    if (!mounted) return;
    setState(() {
      _currentConfig = config;
      _serverAddressController.text = config.serverAddress;
      _isKeepAliveEnabled = config.keepAliveEnabled;
    });
  }

  Future<void> _saveConfiguration() async {
    if (_isSaving) return; // Prevent multiple simultaneous saves

    setState(() {
      _isSaving = true;
    });

    final result = await _api.updateConfig(
      serverAddress: _serverAddressController.text,
      keepAlive: _isKeepAliveEnabled,
    );

    if (!mounted) return;
    setState(() {
      _isSaving = false;
    });
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(result.message),
        backgroundColor: result.success ? Colors.green : Colors.red,
      ),
    );

    await _loadConfig();
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
                      'PB-Mapper Configuration',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    TextField(
                      controller: _serverAddressController,
                      decoration: const InputDecoration(
                        labelText: 'PB_MAPPER_SERVER',
                        hintText: 'localhost:7666',
                        border: OutlineInputBorder(),
                        helperText:
                            'Address of the pb-mapper server to connect to',
                      ),
                    ),
                    const SizedBox(height: 16),
                    SwitchListTile(
                      title: const Text('PB_MAPPER_KEEP_ALIVE'),
                      subtitle: const Text(
                        'Enable TCP keep-alive for connections',
                      ),
                      value: _isKeepAliveEnabled,
                      onChanged: (value) {
                        setState(() => _isKeepAliveEnabled = value);
                      },
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
                onPressed: _isSaving ? null : _saveConfiguration,
                style: ElevatedButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(24),
                  ),
                ),
                child: _isSaving
                    ? const Row(
                        mainAxisAlignment: MainAxisAlignment.center,
                        children: [
                          SizedBox(
                            width: 20,
                            height: 20,
                            child: CircularProgressIndicator(
                              strokeWidth: 2,
                            ),
                          ),
                          SizedBox(width: 12),
                          Text(
                            'Saving...',
                            style: TextStyle(fontSize: 16),
                          ),
                        ],
                      )
                    : const Text(
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
                    _currentConfig != null
                        ? Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                'Server Address: ${_currentConfig!.serverAddress}',
                              ),
                              Text(
                                'Keep-Alive Enabled: ${_currentConfig!.keepAliveEnabled ? 'Yes' : 'No'}',
                              ),
                            ],
                          )
                        : const Text('Fetching current configuration...'),
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
