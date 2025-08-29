import 'package:flutter/material.dart';
import 'package:ui/src/common/theme_change_button.dart';
import 'package:ui/src/common/responsive_layout.dart';
import 'package:url_launcher/url_launcher.dart';

class MainLandingView extends StatelessWidget {
  final VoidCallback onServerManagement;
  final VoidCallback onServiceRegistration;
  final VoidCallback onClientConnection;
  final VoidCallback? onStatusMonitoring;
  final VoidCallback onToggleTheme;

  const MainLandingView({
    super.key,
    required this.onServerManagement,
    required this.onServiceRegistration,
    required this.onClientConnection,
    this.onStatusMonitoring,
    required this.onToggleTheme,
  });

  Future<void> _launchGitHub() async {
    const url = 'https://github.com/ACking-you/pb-mapper';
    if (await canLaunchUrl(Uri.parse(url))) {
      await launchUrl(Uri.parse(url));
    }
  }

  Widget _buildNavigationButton({
    required BuildContext context,
    required VoidCallback onPressed,
    required String text,
  }) {
    return SizedBox(
      height: ResponsiveLayout.getButtonHeight(context),
      width: ResponsiveLayout.isMobile(context) ? double.infinity : 300,
      child: ElevatedButton(
        onPressed: onPressed,
        style: ElevatedButton.styleFrom(
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(
              ResponsiveLayout.isMobile(context) ? 24 : 35,
            ),
          ),
        ),
        child: Text(
          text,
          style: TextStyle(fontSize: ResponsiveLayout.getFontSize(context, 20)),
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
              title: const Text('pb-mapper UI'),
              elevation: 4,
              actions: [getThemeChangeButton(onToggleTheme, context)],
            )
          : null,
      body: ResponsiveLayout.wrapWithMaxWidth(
        context: context,
        child: Padding(
          padding: ResponsiveLayout.getScreenPadding(context),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              MouseRegion(
                cursor: SystemMouseCursors.click,
                child: GestureDetector(
                  onTap: _launchGitHub,
                  child: Text(
                    'pb-mapper',
                    style: TextStyle(
                      fontSize: ResponsiveLayout.isMobile(context) ? 36 : 48,
                      fontWeight: FontWeight.bold,
                      fontFamily: 'Pacifico',
                    ),
                  ),
                ),
              ),
              SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
              Text(
                'Network Tunneling Solution',
                style: TextStyle(
                  fontSize: ResponsiveLayout.getFontSize(context, 20),
                  color: Colors.grey,
                ),
                textAlign: TextAlign.center,
              ),
              SizedBox(
                height: ResponsiveLayout.getVerticalSpacing(context) * 2,
              ),
              if (ResponsiveLayout.isMobile(context))
                Column(
                  children: [
                    _buildNavigationButton(
                      context: context,
                      onPressed: onServerManagement,
                      text: 'Server Management',
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    _buildNavigationButton(
                      context: context,
                      onPressed: onServiceRegistration,
                      text: 'Service Registration',
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    _buildNavigationButton(
                      context: context,
                      onPressed: onClientConnection,
                      text: 'Client Connection',
                    ),
                    if (onStatusMonitoring != null) ...[
                      SizedBox(
                        height: ResponsiveLayout.getVerticalSpacing(context),
                      ),
                      _buildNavigationButton(
                        context: context,
                        onPressed: onStatusMonitoring!,
                        text: 'Status Monitoring',
                      ),
                    ],
                  ],
                )
              else
                Column(
                  children: [
                    _buildNavigationButton(
                      context: context,
                      onPressed: onServerManagement,
                      text: 'Server Management',
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    _buildNavigationButton(
                      context: context,
                      onPressed: onServiceRegistration,
                      text: 'Service Registration',
                    ),
                    SizedBox(
                      height: ResponsiveLayout.getVerticalSpacing(context),
                    ),
                    _buildNavigationButton(
                      context: context,
                      onPressed: onClientConnection,
                      text: 'Client Connection',
                    ),
                    if (onStatusMonitoring != null) ...[
                      SizedBox(
                        height: ResponsiveLayout.getVerticalSpacing(context),
                      ),
                      _buildNavigationButton(
                        context: context,
                        onPressed: onStatusMonitoring!,
                        text: 'Status Monitoring',
                      ),
                    ],
                  ],
                ),
              SizedBox(
                height: ResponsiveLayout.getVerticalSpacing(context) * 2,
              ),
              Text(
                '🌟 Welcome! Choose a function to unlock the power of pb-mapper 🚀',
                style: TextStyle(
                  fontSize: ResponsiveLayout.getFontSize(context, 16),
                  color: Colors.grey,
                ),
                textAlign: TextAlign.center,
              ),
            ],
          ),
        ),
      ),
    );
  }
}
