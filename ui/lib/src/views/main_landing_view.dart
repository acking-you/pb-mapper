import 'package:flutter/material.dart';
import 'package:ui/src/common/theme_change_button.dart';
import 'package:url_launcher/url_launcher.dart';

class MainLandingView extends StatelessWidget {
  final VoidCallback onServerManagement;
  final VoidCallback onServiceRegistration;
  final VoidCallback onClientConnection;
  final VoidCallback onToggleTheme;

  const MainLandingView({
    super.key,
    required this.onServerManagement,
    required this.onServiceRegistration,
    required this.onClientConnection,
    required this.onToggleTheme,
  });

  Future<void> _launchGitHub() async {
    const url = 'https://github.com/ACking-you/pb-mapper';
    if (await canLaunchUrl(Uri.parse(url))) {
      await launchUrl(Uri.parse(url));
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('pb-mapper UI'),
        elevation: 4,
        actions: [getThemeChangeButton(onToggleTheme, context)],
      ),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 600),
          child: Padding(
            padding: const EdgeInsets.all(24.0),
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.center,
              children: [
                MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: GestureDetector(
                    onTap: _launchGitHub,
                    child: const Text(
                      'pb-mapper',
                      style: TextStyle(
                        fontSize: 48,
                        fontWeight: FontWeight.bold,
                        fontFamily: 'Pacifico',
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: 16),
                const Text(
                  'Network Tunneling Solution',
                  style: TextStyle(fontSize: 20, color: Colors.grey),
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 64),
                SizedBox(
                  height: 70,
                  width: 300,
                  child: ElevatedButton(
                    onPressed: onServerManagement,
                    style: ElevatedButton.styleFrom(
                      shape: RoundedRectangleBorder(
                        borderRadius: BorderRadius.circular(35),
                      ),
                    ),
                    child: const Text(
                      'Server Management',
                      style: TextStyle(fontSize: 20),
                    ),
                  ),
                ),
                const SizedBox(height: 32),
                SizedBox(
                  height: 70,
                  width: 300,
                  child: ElevatedButton(
                    onPressed: onServiceRegistration,
                    style: ElevatedButton.styleFrom(
                      shape: RoundedRectangleBorder(
                        borderRadius: BorderRadius.circular(35),
                      ),
                    ),
                    child: const Text(
                      'Service Registration',
                      style: TextStyle(fontSize: 20),
                    ),
                  ),
                ),
                const SizedBox(height: 32),
                SizedBox(
                  height: 70,
                  width: 300,
                  child: ElevatedButton(
                    onPressed: onClientConnection,
                    style: ElevatedButton.styleFrom(
                      shape: RoundedRectangleBorder(
                        borderRadius: BorderRadius.circular(35),
                      ),
                    ),
                    child: const Text(
                      'Client Connection',
                      style: TextStyle(fontSize: 20),
                    ),
                  ),
                ),
                const SizedBox(height: 64),
                const Text(
                  '🌟 Welcome! Choose a function to unlock the power of pb-mapper 🚀',
                  style: TextStyle(fontSize: 16, color: Colors.grey),
                  textAlign: TextAlign.center,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
