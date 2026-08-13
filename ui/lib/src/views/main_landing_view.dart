import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/theme_change_button.dart';
import 'package:url_launcher/url_launcher.dart';

const String kRepositoryUrl = 'https://github.com/ACking-you/pb-mapper';

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

  static Future<void> _launchGitHub() async {
    final uri = Uri.parse(kRepositoryUrl);
    if (await canLaunchUrl(uri)) {
      await launchUrl(uri, mode: LaunchMode.externalApplication);
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
              title: Text(context.l10n.appTitle),
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
              const SizedBox(height: 24),
              const _StarFooter(),
            ],
          ),
        ),
      ),
    );
  }
}

/// The one place the project asks for a star. It sits below the two roles, so
/// it never stands between a user and the thing they came to do.
class _StarFooter extends StatelessWidget {
  const _StarFooter();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Column(
      children: [
        Divider(color: scheme.outlineVariant.withValues(alpha: 0.4)),
        const SizedBox(height: 12),
        Wrap(
          alignment: WrapAlignment.center,
          crossAxisAlignment: WrapCrossAlignment.center,
          spacing: 12,
          runSpacing: 8,
          children: [
            Text(
              context.l10n.starPitch,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
            FilledButton.tonalIcon(
              onPressed: MainLandingView._launchGitHub,
              icon: const Icon(Icons.star_rounded, size: 18),
              label: Text(context.l10n.starOnGitHub),
            ),
          ],
        ),
      ],
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
            context.l10n.quickStart,
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
                  context.l10n.quickStartStep1,
                  style: theme.textTheme.bodyMedium,
                ),
              ),
              TextButton(
                onPressed: onConfiguration,
                child: Text(context.l10n.quickStartStep1Action),
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
                  context.l10n.quickStartStep2,
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
    required this.isRegister,
    required this.onPressed,
  });

  factory _RoleCard.register({required VoidCallback onPressed}) => _RoleCard(
    icon: Icons.upload_rounded,
    isRegister: true,
    onPressed: onPressed,
  );

  factory _RoleCard.connect({required VoidCallback onPressed}) => _RoleCard(
    icon: Icons.download_rounded,
    isRegister: false,
    onPressed: onPressed,
  );

  final IconData icon;
  final bool isRegister;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final l10n = context.l10n;
    final title = isRegister ? l10n.navRegister : l10n.navConnect;
    final summary = isRegister
        ? l10n.roleRegisterSummary
        : l10n.roleConnectSummary;
    final detail = isRegister
        ? l10n.roleRegisterDetail
        : l10n.roleConnectDetail;

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
