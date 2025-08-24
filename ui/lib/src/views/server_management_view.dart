import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';

class ServerManagementView extends StatefulWidget {
  const ServerManagementView({super.key});

  @override
  State<ServerManagementView> createState() => _ServerManagementViewState();
}

class _ServerManagementViewState extends State<ServerManagementView> {
  final _portController = TextEditingController(text: '7666');
  bool _isIPv6Enabled = false;
  bool _isKeepAliveEnabled = true;
  bool _isServerRunning = false;

  @override
  void dispose() {
    _portController.dispose();
    super.dispose();
  }

  void _startServer() {
    final port = int.tryParse(_portController.text) ?? 7666;
    StartServerRequest(
      port: port,
      enableIpv6: _isIPv6Enabled,
      enableKeepAlive: _isKeepAliveEnabled,
    ).sendSignalToRust();
    setState(() => _isServerRunning = true);
  }

  void _stopServer() {
    StopServerRequest().sendSignalToRust();
    setState(() => _isServerRunning = false);
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
                      'Server Configuration',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    TextField(
                      controller: _portController,
                      decoration: const InputDecoration(
                        labelText: 'Server Port',
                        hintText: '7666',
                      ),
                      keyboardType: TextInputType.number,
                    ),
                    const SizedBox(height: 16),
                    SwitchListTile(
                      title: const Text('Enable IPv6'),
                      value: _isIPv6Enabled,
                      onChanged: (value) {
                        setState(() => _isIPv6Enabled = value);
                      },
                    ),
                    SwitchListTile(
                      title: const Text('Enable TCP Keep-Alive'),
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
            Row(
              children: [
                Expanded(
                  child: SizedBox(
                    height: 48,
                    child: ElevatedButton(
                      onPressed: _isServerRunning ? null : _startServer,
                      style: ElevatedButton.styleFrom(
                        shape: RoundedRectangleBorder(
                          borderRadius: BorderRadius.circular(24),
                        ),
                      ),
                      child: Text(
                        _isServerRunning ? 'Server Running' : 'Start Server',
                        style: const TextStyle(fontSize: 16),
                      ),
                    ),
                  ),
                ),
                const SizedBox(width: 16),
                Expanded(
                  child: SizedBox(
                    height: 48,
                    child: ElevatedButton(
                      onPressed: _isServerRunning ? _stopServer : null,
                      style: ElevatedButton.styleFrom(
                        shape: RoundedRectangleBorder(
                          borderRadius: BorderRadius.circular(24),
                        ),
                      ),
                      child: const Text(
                        'Stop Server',
                        style: TextStyle(fontSize: 16),
                      ),
                    ),
                  ),
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
                      'Server Status',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: ServerStatusUpdate.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final status = snapshot.data!.message;
                          return Text('Status: ${status.status}');
                        }
                        return const Text('Status: Not running');
                      },
                    ),
                    const SizedBox(height: 16),
                    const Text(
                      'Log Output:',
                      style: TextStyle(fontWeight: FontWeight.bold),
                    ),
                    const SizedBox(height: 8),
                    const SingleChildScrollView(
                      child: Text(
                        'Server logs will appear here...',
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
