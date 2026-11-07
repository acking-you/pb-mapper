import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/desktop_page.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';

/// Remote desktop: this computer as a host, and the computers it views.
///
/// Transitional: the previous desktop page in the shell, until the host card
/// and the computer list replace it. The title bar carries no capsule, as
/// the host card's viewer panel will offer the same controls; meanwhile the
/// fallback layer does.
class RemotePage extends StatelessWidget {
  const RemotePage({super.key});

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    return PageFrame(
      title: ShellLayout.wideOf(context)
          ? l10n.navRemoteDesktop
          : l10n.navRemote,
      sharingIndicator: false,
      body: DesktopPage(store: AppScope.of(context).store.desktop),
    );
  }
}
