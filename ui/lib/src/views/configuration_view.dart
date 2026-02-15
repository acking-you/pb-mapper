import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';

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
  final _msgHeaderKeyController = TextEditingController();
  bool _isKeepAliveEnabled = true;
  bool _isSaving = false;
  bool _isCheckingServer = false;
  bool? _serverReachable;
  String _serverCheckMessage = '';
  ConfigStatus? _currentConfig;

  @override
  void initState() {
    super.initState();
    _loadConfig();
  }

  @override
  void dispose() {
    _serverAddressController.dispose();
    _msgHeaderKeyController.dispose();
    super.dispose();
  }

  Future<void> _loadConfig() async {
    try {
      final config = await _api.fetchConfig();
      if (!mounted) return;
      setState(() {
        _currentConfig = config;
        _serverAddressController.text = config.serverAddress;
        _isKeepAliveEnabled = config.keepAliveEnabled;
        _msgHeaderKeyController.text = config.msgHeaderKey;
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _currentConfig = null;
        _serverAddressController.text = 'localhost:7666';
        _msgHeaderKeyController.clear();
      });
    }
  }

  Future<void> _saveConfiguration() async {
    if (_isSaving) return; // Prevent multiple simultaneous saves
    final msgHeaderKey = _msgHeaderKeyController.text.trim();
    if (msgHeaderKey.isNotEmpty && msgHeaderKey.length != 32) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('MSG_HEADER_KEY must be exactly 32 characters'),
          backgroundColor: Colors.red,
        ),
      );
      return;
    }

    setState(() {
      _isSaving = true;
    });

    try {
      final result = await _api.updateConfig(
        serverAddress: _serverAddressController.text,
        keepAlive: _isKeepAliveEnabled,
        msgHeaderKey: msgHeaderKey,
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
      await _checkServerConnection();
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _isSaving = false;
      });
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('Failed to save configuration'),
          backgroundColor: Colors.red,
        ),
      );
    }
  }

  Future<void> _checkServerConnection() async {
    if (_isCheckingServer) return;
    setState(() {
      _isCheckingServer = true;
    });

    try {
      final status = await _api.getServerStatusDetail();
      if (!mounted) return;
      setState(() {
        _isCheckingServer = false;
        _serverReachable = status.serverAvailable;
        _serverCheckMessage = status.serverAvailable
            ? 'Server is reachable'
            : 'Server is not reachable yet';
      });

      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(_serverCheckMessage),
          backgroundColor: status.serverAvailable
              ? Colors.green
              : Colors.orange,
        ),
      );
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _isCheckingServer = false;
        _serverReachable = false;
        _serverCheckMessage = 'Server check failed';
      });
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('Server check failed'),
          backgroundColor: Colors.red,
        ),
      );
    }
  }

  Map<String, dynamic> _buildConfigExportPayload() {
    return <String, dynamic>{
      'version': 1,
      'serverAddress': _serverAddressController.text.trim(),
      'keepAliveEnabled': _isKeepAliveEnabled,
      'msgHeaderKey': _msgHeaderKeyController.text.trim(),
      'exportedAt': DateTime.now().toIso8601String(),
    };
  }

  Future<void> _exportConfiguration() async {
    final payload = _buildConfigExportPayload();
    final encoded = base64Encode(utf8.encode(jsonEncode(payload)));
    if (!mounted) return;

    await showDialog<void>(
      context: context,
      builder: (dialogContext) {
        return AlertDialog(
          title: const Text('Export Configuration'),
          content: SizedBox(
            width: 560,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                  'Copy the Base64 string below. It contains JSON configuration data.',
                ),
                const SizedBox(height: 12),
                SelectableText(encoded),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () {
                Navigator.of(dialogContext).pop();
              },
              child: const Text('Close'),
            ),
            FilledButton.icon(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: encoded));
                if (!mounted) return;
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(
                    content: Text('Configuration export copied to clipboard'),
                    backgroundColor: Colors.green,
                  ),
                );
              },
              icon: const Icon(Icons.copy),
              label: const Text('Copy'),
            ),
          ],
        );
      },
    );
  }

  Future<String?> _showImportDialog() async {
    final controller = TextEditingController();
    final imported = await showDialog<String>(
      context: context,
      builder: (dialogContext) {
        return AlertDialog(
          title: const Text('Import Configuration'),
          content: SizedBox(
            width: 560,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text('Paste Base64-encoded JSON configuration here.'),
                const SizedBox(height: 12),
                TextField(
                  controller: controller,
                  maxLines: 6,
                  decoration: const InputDecoration(
                    border: OutlineInputBorder(),
                    hintText: 'Base64 string',
                  ),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () {
                Navigator.of(dialogContext).pop();
              },
              child: const Text('Cancel'),
            ),
            FilledButton.icon(
              onPressed: () {
                Navigator.of(dialogContext).pop(controller.text.trim());
              },
              icon: const Icon(Icons.download),
              label: const Text('Import'),
            ),
          ],
        );
      },
    );
    controller.dispose();
    return imported;
  }

  bool _parseKeepAliveFlag(dynamic value) {
    if (value is bool) return value;
    if (value is num) return value != 0;
    if (value is String) {
      final normalized = value.trim().toLowerCase();
      if (normalized == 'true' || normalized == '1') return true;
      if (normalized == 'false' || normalized == '0') return false;
    }
    throw const FormatException('Invalid keepAliveEnabled value');
  }

  Map<String, dynamic> _decodeImportedPayload(String rawBase64) {
    final normalized = rawBase64.replaceAll(RegExp(r'\s+'), '');
    if (normalized.isEmpty) {
      throw const FormatException('Import content is empty');
    }

    List<int> bytes;
    try {
      bytes = base64Decode(normalized);
    } catch (_) {
      bytes = base64Url.decode(base64Url.normalize(normalized));
    }

    final decodedJson = jsonDecode(utf8.decode(bytes));
    if (decodedJson is! Map) {
      throw const FormatException('Import payload must be a JSON object');
    }
    return Map<String, dynamic>.from(decodedJson);
  }

  Future<void> _importConfiguration() async {
    if (_isSaving) return;
    final importedRaw = await _showImportDialog();
    if (importedRaw == null) return;

    try {
      final payload = _decodeImportedPayload(importedRaw);

      final serverAddress = (payload['serverAddress'] ?? '').toString().trim();
      final msgHeaderKey = (payload['msgHeaderKey'] ?? '').toString().trim();
      final keepAliveEnabled = _parseKeepAliveFlag(payload['keepAliveEnabled']);

      if (serverAddress.isEmpty) {
        throw const FormatException('serverAddress is required');
      }
      if (msgHeaderKey.isNotEmpty && msgHeaderKey.length != 32) {
        throw const FormatException(
          'MSG_HEADER_KEY must be exactly 32 characters',
        );
      }

      setState(() {
        _serverAddressController.text = serverAddress;
        _msgHeaderKeyController.text = msgHeaderKey;
        _isKeepAliveEnabled = keepAliveEnabled;
      });

      await _saveConfiguration();
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('Import failed: $e'),
          backgroundColor: Colors.red,
        ),
      );
    }
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
                    TextField(
                      controller: _msgHeaderKeyController,
                      decoration: const InputDecoration(
                        labelText: 'MSG_HEADER_KEY',
                        hintText: '32-byte key, leave empty for default',
                        border: OutlineInputBorder(),
                        helperText:
                            'Used for message checksum/encryption handshake. Must be 32 chars when set.',
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
                            child: CircularProgressIndicator(strokeWidth: 2),
                          ),
                          SizedBox(width: 12),
                          Text('Saving...', style: TextStyle(fontSize: 16)),
                        ],
                      )
                    : const Text(
                        'Save Configuration',
                        style: TextStyle(fontSize: 16),
                      ),
              ),
            ),
            const SizedBox(height: 12),
            Row(
              children: [
                Expanded(
                  child: SizedBox(
                    height: 44,
                    child: OutlinedButton.icon(
                      onPressed: _isSaving ? null : _exportConfiguration,
                      icon: const Icon(Icons.upload_file),
                      label: const Text('Export (Base64)'),
                    ),
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: SizedBox(
                    height: 44,
                    child: OutlinedButton.icon(
                      onPressed: _isSaving ? null : _importConfiguration,
                      icon: const Icon(Icons.download_for_offline),
                      label: const Text('Import (Base64)'),
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            SizedBox(
              height: 44,
              width: double.infinity,
              child: OutlinedButton.icon(
                onPressed: _isCheckingServer ? null : _checkServerConnection,
                icon: _isCheckingServer
                    ? const SizedBox(
                        width: 16,
                        height: 16,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : Icon(
                        (_serverReachable ?? false)
                            ? Icons.cloud_done
                            : Icons.cloud_outlined,
                      ),
                label: Text(
                  _isCheckingServer
                      ? 'Checking Server...'
                      : (_serverReachable == null
                            ? 'Check Server Connectivity'
                            : _serverCheckMessage),
                ),
              ),
            ),
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                ElevatedButton.icon(
                  onPressed: AppNavigationManager.navigateToConnectPage,
                  icon: const Icon(Icons.cable),
                  label: const Text('Open Connect'),
                ),
                OutlinedButton.icon(
                  onPressed: AppNavigationManager.navigateToRegisterPage,
                  icon: const Icon(Icons.app_registration),
                  label: const Text('Open Register'),
                ),
              ],
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
                              Text(
                                'MSG_HEADER_KEY Configured: ${_currentConfig!.msgHeaderKey.isNotEmpty ? 'Yes' : 'No'}',
                              ),
                            ],
                          )
                        : const Text('Fetching current configuration...'),
                    const SizedBox(height: 16),
                    Text(
                      'Note: Changes apply to subsequent register/connect operations.',
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
