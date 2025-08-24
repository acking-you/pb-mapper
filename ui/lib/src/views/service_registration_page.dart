import 'package:flutter/material.dart';
import 'package:ui/src/views/service_registration_view.dart';
import 'package:ui/src/views/status_monitoring_view.dart';

class ServiceRegistrationPage extends StatefulWidget {
  final VoidCallback onBack;
  final VoidCallback onToggleTheme;

  const ServiceRegistrationPage({
    super.key,
    required this.onBack,
    required this.onToggleTheme,
  });

  @override
  State<ServiceRegistrationPage> createState() => _ServiceRegistrationPageState();
}

class _ServiceRegistrationPageState extends State<ServiceRegistrationPage> {
  int _currentIndex = 0;

  final List<Widget> _views = [
    ServiceRegistrationView(),
    StatusMonitoringView(),
  ];

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Service Registration'),
        leading: IconButton(
          icon: const Icon(Icons.arrow_back),
          onPressed: widget.onBack,
        ),
        actions: [
          IconButton(
            icon: Icon(
              Theme.of(context).brightness == Brightness.dark
                  ? Icons.light_mode
                  : Icons.dark_mode,
            ),
            onPressed: widget.onToggleTheme,
          ),
        ],
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
          BottomNavigationBarItem(
            icon: Icon(Icons.add_circle),
            label: 'Register',
          ),
          BottomNavigationBarItem(
            icon: Icon(Icons.list),
            label: 'Status',
          ),
        ],
      ),
    );
  }
}