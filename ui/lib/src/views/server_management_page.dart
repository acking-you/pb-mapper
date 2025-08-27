import 'package:flutter/material.dart';
import 'package:ui/src/common/theme_change_button.dart';
import 'package:ui/src/views/server_management_view.dart';
import 'package:ui/src/views/status_monitoring_view.dart';

class ServerManagementPage extends StatefulWidget {
  final VoidCallback onBack;
  final VoidCallback onToggleTheme;

  const ServerManagementPage({
    super.key,
    required this.onBack,
    required this.onToggleTheme,
  });

  @override
  State<ServerManagementPage> createState() => _ServerManagementPageState();
}

class _ServerManagementPageState extends State<ServerManagementPage> {
  int _currentIndex = 0;

  final List<Widget> _views = [ServerManagementView(), StatusMonitoringView()];

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Server Management'),
        leading: IconButton(
          icon: const Icon(Icons.arrow_back),
          onPressed: widget.onBack,
        ),
        actions: [getThemeChangeButton(widget.onToggleTheme, context)],
        elevation: 4,
      ),
      body: _views[_currentIndex],
      bottomNavigationBar: BottomNavigationBar(
        currentIndex: _currentIndex,
        onTap: (index) => setState(() => _currentIndex = index),
        type: BottomNavigationBarType.fixed,
        showSelectedLabels: true,
        showUnselectedLabels: true,
        items: const [
          BottomNavigationBarItem(icon: Icon(Icons.settings), label: 'Server'),
          BottomNavigationBarItem(icon: Icon(Icons.list), label: 'Status'),
        ],
      ),
    );
  }
}
