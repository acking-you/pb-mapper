import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';
import 'package:ui/src/common/responsive_layout.dart';
import 'package:ui/src/views/log_view_button.dart';

class ServerManagementView extends StatefulWidget {
  const ServerManagementView({super.key});

  @override
  State<ServerManagementView> createState() => _ServerManagementViewState();
}

class _ServerManagementViewState extends State<ServerManagementView> {
  final _portController = TextEditingController(text: '7666');
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
      padding: ResponsiveLayout.getScreenPadding(context),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Card(
              child: Padding(
                padding: EdgeInsets.all(
                  ResponsiveLayout.getCardPadding(context),
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Server Configuration',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    TextField(
                      controller: _portController,
                      decoration: const InputDecoration(
                        labelText: 'Server Port',
                        hintText: '7666',
                      ),
                      keyboardType: TextInputType.number,
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
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
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
            ResponsiveLayout.buildResponsiveRow(
              context: context,
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
                if (!ResponsiveLayout.isMobile(context))
                  SizedBox(
                    width: ResponsiveLayout.getHorizontalPadding(context),
                  ),
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
            SizedBox(
              height: ResponsiveLayout.getVerticalSpacing(context) * 1.5,
            ),
            // Replace the server status card with log view button
            const LogViewButton(),
          ],
        ),
      ),
    );
  }
}
