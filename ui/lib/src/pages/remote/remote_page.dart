import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/remote/add_computer.dart';
import 'package:pb_mapper_ui/src/pages/remote/computer_list.dart';
import 'package:pb_mapper_ui/src/pages/remote/computer_list_compact.dart';
import 'package:pb_mapper_ui/src/pages/remote/host_card.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';

/// Remote desktop: this computer as a host, and the computers it views
/// (Main, HostSetup, MobileHome). The page itself rebuilds only for its
/// layout, whether this device can host, and whether any computer is
/// remembered; each card listens to its own sources.
class RemotePage extends StatefulWidget {
  const RemotePage({super.key});

  @override
  State<RemotePage> createState() => _RemotePageState();
}

class _RemotePageState extends State<RemotePage> {
  AppStore? _store;

  /// The store polls the host for this page while it is mounted.
  DesktopDemandHandle? _demand;
  bool _hostUnsupported = false, _hasComputers = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final store = AppScope.of(context).store;
    if (identical(store, _store)) return;
    _detach();
    _store = store;
    _demand = store.desktop.demand(DesktopDemand.page);
    store.desktop.hostView.addListener(_changed);
    store.computers.addListener(_changed);
    _changed();
    // Fresh online dots (D22); the relay check is not forced.
    unawaited(store.checkRelay());
  }

  void _detach() {
    final store = _store;
    if (store == null) return;
    store.desktop.hostView.removeListener(_changed);
    store.computers.removeListener(_changed);
    _demand?.dispose();
    _demand = null;
  }

  void _changed() {
    final store = _store;
    if (store == null) return;
    final host = store.desktop.hostView.value;
    final unsupported = host.loaded && !host.supported;
    final any = store.computers.computers.isNotEmpty;
    if (unsupported == _hostUnsupported && any == _hasComputers) return;
    if (!mounted) return;
    setState(() {
      _hostUnsupported = unsupported;
      _hasComputers = any;
    });
  }

  @override
  void dispose() {
    _detach();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final wide = ShellLayout.wideOf(context);
    final store = AppScope.of(context).store;
    return PageFrame(
      title: wide ? l10n.navRemoteDesktop : l10n.navRemote,
      // The host card's viewer panel offers the same controls (D15).
      sharingIndicator: false,
      actions: [
        if (wide && _hasComputers)
          IconButton(
            key: const Key('remote-refresh'),
            tooltip: l10n.refresh,
            onPressed: () => unawaited(store.checkRelay(force: true)),
            icon: const Icon(AppIcons.refresh),
          ),
      ],
      primary: _hasComputers || !wide
          ? PrimaryAction(
              key: const Key('remote-add'),
              icon: AppIcons.add,
              label: l10n.remoteAddComputer,
              onPressed: () => unawaited(showAddComputer(context)),
            )
          : null,
      body: wide ? _wide() : _compact(),
    );
  }

  Widget _wide() => LayoutBuilder(
    builder: (context, box) {
      final twoColumns = box.maxWidth - 64 >= 1000;
      final List<Widget> content;
      if (_hostUnsupported) {
        content = [
          const RemoteComputerList(),
          const SizedBox(height: 14),
          const _HostUnsupportedNote(),
        ];
      } else if (twoColumns) {
        content = [
          const Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SizedBox(width: 560, child: HostCard()),
              SizedBox(width: 20),
              Expanded(child: RemoteComputerList(emptyMinHeight: 520)),
            ],
          ),
        ];
      } else {
        content = [
          Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 760),
              child: const Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  HostCard(),
                  SizedBox(height: 20),
                  RemoteComputerList(),
                ],
              ),
            ),
          ),
        ];
      }
      return ListView(
        key: const PageStorageKey('remote-page'),
        padding: const EdgeInsets.fromLTRB(32, 4, 32, 32),
        children: content,
      );
    },
  );

  Widget _compact() => ListView(
    key: const PageStorageKey('remote-page'),
    padding: const EdgeInsets.fromLTRB(16, 4, 16, 24),
    children: [
      if (!_hostUnsupported) ...[const HostCard(), const SizedBox(height: 12)],
      const CompactComputerList(),
      if (_hostUnsupported) ...[
        const SizedBox(height: 12),
        const _HostUnsupportedNote(),
      ],
    ],
  );
}

/// Why there is no host card: phones view and control but are not
/// controlled; macOS and Linux cannot share yet; Windows shares only where
/// Media Foundation is installed.
class _HostUnsupportedNote extends StatelessWidget {
  const _HostUnsupportedNote();

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final phone =
        defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    final note = phone
        ? l10n.remoteMobileHostNote
        : defaultTargetPlatform == TargetPlatform.windows
        ? l10n.hostNeedsMediaFoundation
        : l10n.hostUnsupported;
    final style = TextStyle(fontSize: 12.5, color: quietText(scheme));
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (phone) ...[
            Icon(
              AppIcons.phone,
              size: AppIconSize.inline,
              color: quietText(scheme),
            ),
            const SizedBox(width: 8),
          ],
          Expanded(
            child: Text(
              note,
              key: const Key('remote-host-note'),
              style: style,
            ),
          ),
        ],
      ),
    );
  }
}
