import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/theme_change_button.dart';
import 'package:url_launcher/url_launcher.dart';

/// The landing page: how to get started, then the two roles to pick from.
///
/// Status, Config and Logs are reachable from the sidebar. Repeating them here
/// only asked the same question twice, so this page answers the one question a
/// new user actually has: which end of the tunnel am I on?
class MainLandingView extends StatelessWidget {
  final VoidCallback onConfiguration;
  final VoidCallback onServiceRegistration;
  final VoidCallback onClientConnection;
  final VoidCallback onToggleTheme;

  const MainLandingView({
    super.key,
    required this.onConfiguration,
    required this.onServiceRegistration,
    required this.onClientConnection,
    required this.onToggleTheme,
  });

  Future<void> _launchGitHub() async {
    const url = 'https://github.com/ACking-you/pb-mapper';
    final uri = Uri.parse(url);
    if (await canLaunchUrl(uri)) {
      await launchUrl(uri);
    }
  }

  @override
  Widget build(BuildContext context) {
    final isMobile = ResponsiveLayout.isMobile(context);
    final theme = Theme.of(context);

    return Scaffold(
      // Transparent on desktop so the shell's content panel shows through;
      // on mobile this view is the whole screen and keeps its own surface.
      backgroundColor: isMobile ? null : Colors.transparent,
      appBar: isMobile
          ? AppBar(
              title: const Text('pb-mapper'),
              elevation: 0,
              actions: [getThemeChangeButton(onToggleTheme, context)],
            )
          : null,
      body: ResponsiveLayout.wrapWithMaxWidth(
        context: context,
        child: SingleChildScrollView(
          padding: EdgeInsets.symmetric(
            horizontal: ResponsiveLayout.getHorizontalPadding(context),
            vertical: isMobile ? 20 : 36,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Center(
                child: GestureDetector(
                  onTap: _launchGitHub,
                  child: Text(
                    'pb-mapper',
                    style: theme.textTheme.displaySmall?.copyWith(
                      fontSize: isMobile ? 34 : 40,
                      fontWeight: FontWeight.w700,
                      letterSpacing: -0.5,
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 28),
              _QuickStart(onConfiguration: onConfiguration),
              const SizedBox(height: 20),
              if (isMobile)
                Column(
                  children: [
                    _RoleCard.register(onPressed: onServiceRegistration),
                    const SizedBox(height: 12),
                    _RoleCard.connect(onPressed: onClientConnection),
                  ],
                )
              else
                // IntrinsicHeight so both cards match the taller one. Plain
                // stretch would ask for infinite height here, because inside a
                // scroll view the cross axis is unbounded.
                IntrinsicHeight(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Expanded(
                        child: _RoleCard.register(
                          onPressed: onServiceRegistration,
                        ),
                      ),
                      const SizedBox(width: 12),
                      Expanded(
                        child: _RoleCard.connect(onPressed: onClientConnection),
                      ),
                    ],
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _QuickStart extends StatelessWidget {
  const _QuickStart({required this.onConfiguration});

  final VoidCallback onConfiguration;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      padding: const EdgeInsets.all(18),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest.withValues(alpha: 0.45),
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'Quick Start',
            style: theme.textTheme.titleSmall?.copyWith(
              fontWeight: FontWeight.w700,
            ),
          ),
          const SizedBox(height: 12),
          Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              _StepNumber(1),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  'Point pb-mapper at your server',
                  style: theme.textTheme.bodyMedium,
                ),
              ),
              TextButton(
                onPressed: onConfiguration,
                child: const Text('Configure'),
              ),
            ],
          ),
          const SizedBox(height: 4),
          Row(
            children: [
              _StepNumber(2),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  'Pick the role that matches this machine',
                  style: theme.textTheme.bodyMedium,
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _StepNumber extends StatelessWidget {
  const _StepNumber(this.value);

  final int value;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: 20,
      height: 20,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: scheme.primary.withValues(alpha: 0.14),
        shape: BoxShape.circle,
      ),
      child: Text(
        '$value',
        style: TextStyle(
          fontSize: 11,
          fontWeight: FontWeight.w700,
          color: scheme.primary,
        ),
      ),
    );
  }
}

/// One of the two ends of a tunnel. The wording is the whole point of this
/// card: which side you are on decides everything else in the app.
class _RoleCard extends StatelessWidget {
  const _RoleCard({
    required this.icon,
    required this.title,
    required this.summary,
    required this.detail,
    required this.onPressed,
  });

  factory _RoleCard.register({required VoidCallback onPressed}) => _RoleCard(
    icon: Icons.upload_rounded,
    title: 'Register',
    summary: 'This machine has a service to share',
    detail: 'Publish a local TCP or UDP port under a key, so others can '
        'reach it through the server.',
    onPressed: onPressed,
  );

  factory _RoleCard.connect({required VoidCallback onPressed}) => _RoleCard(
    icon: Icons.download_rounded,
    title: 'Connect',
    summary: 'This machine wants to reach a shared service',
    detail: 'Subscribe to a key and expose it as a local port, as if the '
        'remote service were running here.',
    onPressed: onPressed,
  );

  final IconData icon;
  final String title;
  final String summary;
  final String detail;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Material(
      color: scheme.surfaceContainerHighest.withValues(alpha: 0.32),
      borderRadius: BorderRadius.circular(12),
      child: InkWell(
        onTap: onPressed,
        borderRadius: BorderRadius.circular(12),
        child: Padding(
          padding: const EdgeInsets.all(18),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(icon, size: 20, color: scheme.primary),
                  const SizedBox(width: 8),
                  Text(
                    title,
                    style: theme.textTheme.titleMedium?.copyWith(
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              Text(
                summary,
                style: theme.textTheme.bodyMedium?.copyWith(
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 6),
              Text(
                detail,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                  height: 1.4,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
