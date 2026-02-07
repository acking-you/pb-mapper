import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/theme_change_button.dart';
import 'package:url_launcher/url_launcher.dart';

class MainLandingView extends StatelessWidget {
  final VoidCallback onConfiguration;
  final VoidCallback onServiceRegistration;
  final VoidCallback onClientConnection;
  final VoidCallback onStatusMonitoring;
  final VoidCallback onLogs;
  final VoidCallback onToggleTheme;

  const MainLandingView({
    super.key,
    required this.onConfiguration,
    required this.onServiceRegistration,
    required this.onClientConnection,
    required this.onStatusMonitoring,
    required this.onLogs,
    required this.onToggleTheme,
  });

  Future<void> _launchGitHub() async {
    const url = 'https://github.com/ACking-you/pb-mapper';
    final uri = Uri.parse(url);
    if (await canLaunchUrl(uri)) {
      await launchUrl(uri);
    }
  }

  Widget _buildFeatureCard({
    required BuildContext context,
    required VoidCallback onPressed,
    required String title,
    required String description,
    required IconData icon,
    required Color color,
  }) {
    return Card(
      elevation: 4,
      child: InkWell(
        onTap: onPressed,
        borderRadius: BorderRadius.circular(12),
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            children: [
              Icon(icon, size: 44, color: color),
              const SizedBox(height: 12),
              Text(
                title,
                style: const TextStyle(
                  fontSize: 18,
                  fontWeight: FontWeight.bold,
                ),
                textAlign: TextAlign.center,
              ),
              const SizedBox(height: 8),
              Text(
                description,
                style: TextStyle(fontSize: 14, color: Colors.grey[600]),
                textAlign: TextAlign.center,
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildQuickStartCard(BuildContext context) {
    return Card(
      color: Theme.of(context).brightness == Brightness.dark
          ? Colors.blueGrey.shade900
          : Colors.blue.shade50,
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  Icons.route_outlined,
                  color: Theme.of(context).colorScheme.primary,
                ),
                const SizedBox(width: 10),
                Text(
                  'Quick Start',
                  style: Theme.of(context).textTheme.titleMedium?.copyWith(
                    fontWeight: FontWeight.bold,
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            const Text('1) Set `PB_MAPPER_SERVER` in Configuration'),
            const SizedBox(height: 4),
            const Text('2) Register local services'),
            const SizedBox(height: 4),
            const Text('3) Connect clients to registered services'),
            const SizedBox(height: 14),
            Wrap(
              spacing: 10,
              runSpacing: 10,
              children: [
                ElevatedButton.icon(
                  onPressed: onConfiguration,
                  icon: const Icon(Icons.settings),
                  label: const Text('Go to Config'),
                ),
                OutlinedButton.icon(
                  onPressed: onServiceRegistration,
                  icon: const Icon(Icons.app_registration),
                  label: const Text('Go to Register'),
                ),
                OutlinedButton.icon(
                  onPressed: onClientConnection,
                  icon: const Icon(Icons.cable),
                  label: const Text('Go to Connect'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final showAppBar = ResponsiveLayout.isMobile(context);
    final isMobile = ResponsiveLayout.isMobile(context);

    final cards = <Widget>[
      _buildFeatureCard(
        context: context,
        onPressed: onConfiguration,
        title: 'Configuration',
        description: 'Step 1: Configure `PB_MAPPER_SERVER` and runtime options',
        icon: Icons.settings,
        color: Colors.blueGrey,
      ),
      _buildFeatureCard(
        context: context,
        onPressed: onServiceRegistration,
        title: 'Register',
        description: 'Publish local services to your pb-mapper server',
        icon: Icons.app_registration,
        color: Colors.green,
      ),
      _buildFeatureCard(
        context: context,
        onPressed: onClientConnection,
        title: 'Connect',
        description: 'Create local client tunnels to registered services',
        icon: Icons.cable,
        color: Colors.orange,
      ),
      _buildFeatureCard(
        context: context,
        onPressed: onStatusMonitoring,
        title: 'Status',
        description: 'Inspect currently registered services and connections',
        icon: Icons.monitor,
        color: Colors.teal,
      ),
      _buildFeatureCard(
        context: context,
        onPressed: onLogs,
        title: 'Logs',
        description: 'Open the dedicated runtime log viewer',
        icon: Icons.terminal,
        color: Colors.indigo,
      ),
    ];

    return Scaffold(
      appBar: showAppBar
          ? AppBar(
              title: const Text('pb-mapper'),
              elevation: 0,
              actions: [getThemeChangeButton(onToggleTheme, context)],
            )
          : null,
      body: ResponsiveLayout.wrapWithMaxWidth(
        context: context,
        child: Padding(
          padding: ResponsiveLayout.getScreenPadding(context),
          child: SingleChildScrollView(
            child: Column(
              children: [
                const SizedBox(height: 20),
                GestureDetector(
                  onTap: _launchGitHub,
                  child: Text(
                    'pb-mapper',
                    style: TextStyle(
                      fontSize: isMobile ? 56 : 72,
                      fontWeight: FontWeight.bold,
                      color: Theme.of(context).brightness == Brightness.dark
                          ? Colors.white
                          : Theme.of(context).primaryColor,
                    ),
                  ),
                ),
                const SizedBox(height: 12),
                Text(
                  'Network Tunneling & Proxy Client',
                  style: TextStyle(
                    fontSize: ResponsiveLayout.getFontSize(context, 22),
                    color: Colors.grey[600],
                    fontWeight: FontWeight.w500,
                  ),
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 20),
                _buildQuickStartCard(context),
                const SizedBox(height: 20),
                isMobile
                    ? Column(
                        children:
                            cards
                                .expand(
                                  (card) => [card, const SizedBox(height: 16)],
                                )
                                .toList()
                              ..removeLast(),
                      )
                    : GridView.count(
                        shrinkWrap: true,
                        physics: const NeverScrollableScrollPhysics(),
                        crossAxisCount: 2,
                        crossAxisSpacing: 16,
                        mainAxisSpacing: 16,
                        childAspectRatio: 1.25,
                        children: cards,
                      ),
                const SizedBox(height: 28),
                Text(
                  'Start with Configuration, then Register or Connect.',
                  style: TextStyle(
                    fontSize: 16,
                    color: Colors.grey[600],
                    fontStyle: FontStyle.italic,
                  ),
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 20),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
