import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/theme_change_button.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:url_launcher/url_launcher.dart';

class MainLandingView extends StatelessWidget {
  final VoidCallback onServerManagement;
  final VoidCallback onServiceRegistration;
  final VoidCallback onClientConnection;
  final VoidCallback? onStatusMonitoring;
  final VoidCallback? onConfiguration;
  final VoidCallback onToggleTheme;

  const MainLandingView({
    super.key,
    required this.onServerManagement,
    required this.onServiceRegistration,
    required this.onClientConnection,
    this.onStatusMonitoring,
    this.onConfiguration,
    required this.onToggleTheme,
  });

  Future<void> _launchGitHub() async {
    const url = 'https://github.com/ACking-you/pb-mapper';
    if (await canLaunchUrl(Uri.parse(url))) {
      await launchUrl(Uri.parse(url));
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
              Icon(icon, size: 48, color: color),
              const SizedBox(height: 12),
              Text(
                title,
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
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

  @override
  Widget build(BuildContext context) {
    final bool showAppBar = ResponsiveLayout.isMobile(context);

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
              crossAxisAlignment: CrossAxisAlignment.center,
              children: [
                const SizedBox(height: 20),
                // Large Project Title
                MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: GestureDetector(
                    onTap: _launchGitHub,
                    child: Text(
                      'pb-mapper',
                      style: TextStyle(
                        fontSize: ResponsiveLayout.isMobile(context) ? 56 : 72,
                        fontWeight: FontWeight.bold,
                        color: Theme.of(context).brightness == Brightness.dark
                            ? Colors.white
                            : Theme.of(context).primaryColor,
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: 12),
                Text(
                  'Network Tunneling & Proxy Solution',
                  style: TextStyle(
                    fontSize: ResponsiveLayout.getFontSize(context, 22),
                    color: Colors.grey[600],
                    fontWeight: FontWeight.w500,
                  ),
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 20),

                // Simplified Project Description with GitHub Star Call-to-Action
                Container(
                  padding: const EdgeInsets.all(20),
                  margin: const EdgeInsets.symmetric(horizontal: 16),
                  decoration: BoxDecoration(
                    color: Theme.of(context).brightness == Brightness.dark
                        ? Colors.grey[800]
                        : Colors.blue[50],
                    borderRadius: BorderRadius.circular(16),
                    border: Border.all(
                      color: Theme.of(context).brightness == Brightness.dark
                          ? Colors.grey[700]!
                          : Colors.blue[200]!,
                      width: 1,
                    ),
                  ),
                  child: Column(
                    children: [
                      Text(
                        'Access your local services from anywhere through secure tunneling.',
                        style: TextStyle(
                          fontSize: 18,
                          color: Theme.of(context).brightness == Brightness.dark
                              ? Colors.grey[300]
                              : Colors.blue[800],
                          fontWeight: FontWeight.w500,
                        ),
                        textAlign: TextAlign.center,
                      ),
                      const SizedBox(height: 16),
                      // GitHub Star Call-to-Action
                      MouseRegion(
                        cursor: SystemMouseCursors.click,
                        child: GestureDetector(
                          onTap: _launchGitHub,
                          child: Container(
                            padding: const EdgeInsets.symmetric(
                              horizontal: 16,
                              vertical: 12,
                            ),
                            decoration: BoxDecoration(
                              color:
                                  Theme.of(context).brightness ==
                                      Brightness.dark
                                  ? Colors.grey[700]
                                  : Colors.white,
                              borderRadius: BorderRadius.circular(12),
                              border: Border.all(
                                color:
                                    Theme.of(context).brightness ==
                                        Brightness.dark
                                    ? Colors.grey[600]!
                                    : Colors.grey[300]!,
                              ),
                            ),
                            child: Row(
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                Icon(
                                  Icons.star,
                                  color: Colors.orange,
                                  size: 20,
                                ),
                                const SizedBox(width: 8),
                                Text(
                                  'Like it? Give us a ⭐ on GitHub!',
                                  style: TextStyle(
                                    fontSize: 16,
                                    color:
                                        Theme.of(context).brightness ==
                                            Brightness.dark
                                        ? Colors.grey[300]
                                        : Colors.grey[700],
                                    fontWeight: FontWeight.w600,
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ],
                  ),
                ),

                const SizedBox(height: 24),

                // Feature Cards
                ResponsiveLayout.isMobile(context)
                    ? Column(
                        children: [
                          _buildFeatureCard(
                            context: context,
                            onPressed: onServerManagement,
                            title: 'Server Management',
                            description:
                                'Start and manage the central pb-mapper server',
                            icon: Icons.dns,
                            color: Colors.blue,
                          ),
                          const SizedBox(height: 16),
                          _buildFeatureCard(
                            context: context,
                            onPressed: onServiceRegistration,
                            title: 'Service Registration',
                            description:
                                'Register local services to make them accessible',
                            icon: Icons.app_registration,
                            color: Colors.green,
                          ),
                          const SizedBox(height: 16),
                          _buildFeatureCard(
                            context: context,
                            onPressed: onClientConnection,
                            title: 'Client Connection',
                            description:
                                'Connect to registered services remotely',
                            icon: Icons.cable,
                            color: Colors.orange,
                          ),
                          const SizedBox(height: 16),
                          if (onStatusMonitoring != null)
                            _buildFeatureCard(
                              context: context,
                              onPressed: onStatusMonitoring!,
                              title: 'Status Monitoring',
                              description:
                                  'Monitor server status and active connections',
                              icon: Icons.monitor,
                              color: Colors.purple,
                            ),
                          if (onConfiguration != null) ...[
                            const SizedBox(height: 16),
                            _buildFeatureCard(
                              context: context,
                              onPressed: onConfiguration!,
                              title: 'Configuration',
                              description:
                                  'Configure server settings and preferences',
                              icon: Icons.settings,
                              color: Colors.grey,
                            ),
                          ],
                        ],
                      )
                    : GridView.count(
                        shrinkWrap: true,
                        physics: const NeverScrollableScrollPhysics(),
                        crossAxisCount: 2,
                        crossAxisSpacing: 16,
                        mainAxisSpacing: 16,
                        childAspectRatio: 1.2,
                        children: [
                          _buildFeatureCard(
                            context: context,
                            onPressed: onServerManagement,
                            title: 'Server Management',
                            description:
                                'Start and manage the central pb-mapper server',
                            icon: Icons.dns,
                            color: Colors.blue,
                          ),
                          _buildFeatureCard(
                            context: context,
                            onPressed: onServiceRegistration,
                            title: 'Service Registration',
                            description:
                                'Register local services to make them accessible',
                            icon: Icons.app_registration,
                            color: Colors.green,
                          ),
                          _buildFeatureCard(
                            context: context,
                            onPressed: onClientConnection,
                            title: 'Client Connection',
                            description:
                                'Connect to registered services remotely',
                            icon: Icons.cable,
                            color: Colors.orange,
                          ),
                          if (onStatusMonitoring != null)
                            _buildFeatureCard(
                              context: context,
                              onPressed: onStatusMonitoring!,
                              title: 'Status Monitoring',
                              description:
                                  'Monitor server status and active connections',
                              icon: Icons.monitor,
                              color: Colors.purple,
                            ),
                        ],
                      ),

                const SizedBox(height: 32),

                // Footer
                Text(
                  'Click on any feature above to get started',
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
