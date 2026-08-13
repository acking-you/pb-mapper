import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/models/service_config.dart';

class EditServiceDialog extends StatefulWidget {
  final ServiceConfig config;

  const EditServiceDialog({super.key, required this.config});

  @override
  State<EditServiceDialog> createState() => _EditServiceDialogState();
}

class _EditServiceDialogState extends State<EditServiceDialog> {
  late TextEditingController _serviceKeyController;
  late TextEditingController _localAddressController;
  late String _selectedProtocol;
  late bool _enableEncryption;
  late bool _enableKeepAlive;

  @override
  void initState() {
    super.initState();
    _serviceKeyController = TextEditingController(
      text: widget.config.serviceKey,
    );
    _localAddressController = TextEditingController(
      text: widget.config.localAddress,
    );
    _selectedProtocol = widget.config.protocol;
    _enableEncryption = widget.config.enableEncryption;
    _enableKeepAlive = widget.config.enableKeepAlive;
  }

  @override
  void dispose() {
    _serviceKeyController.dispose();
    _localAddressController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Edit service'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: _serviceKeyController,
              decoration: InputDecoration(
                labelText: context.l10n.serviceKey,
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 16),
            TextField(
              controller: _localAddressController,
              decoration: InputDecoration(
                labelText: context.l10n.localAddress,
                hintText: '127.0.0.1:8080',
                border: OutlineInputBorder(),
              ),
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
              decoration: InputDecoration(
                labelText: context.l10n.protocol,
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 16),
            SwitchListTile(
              title: Text(context.l10n.enableEncryption),
              value: _enableEncryption,
              onChanged: (value) => setState(() => _enableEncryption = value),
            ),
            SwitchListTile(
              title: const Text('Enable Keep-Alive'),
              value: _enableKeepAlive,
              onChanged: (value) => setState(() => _enableKeepAlive = value),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(context.l10n.cancel),
        ),
        ElevatedButton(onPressed: _saveChanges, child: const Text('Save')),
      ],
    );
  }

  void _saveChanges() {
    if (_serviceKeyController.text.isEmpty ||
        _localAddressController.text.isEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('Service key and local address are required'),
        ),
      );
      return;
    }

    final updatedConfig = widget.config.copyWith(
      serviceKey: _serviceKeyController.text,
      localAddress: _localAddressController.text,
      protocol: _selectedProtocol,
      enableEncryption: _enableEncryption,
      enableKeepAlive: _enableKeepAlive,
    );

    Navigator.of(context).pop(updatedConfig);
  }
}
