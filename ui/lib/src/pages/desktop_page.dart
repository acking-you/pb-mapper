import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import '../common/l10n_extension.dart';
import '../shell/app_scope.dart';
import '../state/desktop_store.dart';
import 'desktop_viewer.dart';

void openDesktop(BuildContext context) {
  final store = AppScope.of(context).store.desktop;
  Navigator.of(
    context,
  ).push(MaterialPageRoute<void>(builder: (_) => DesktopPage(store: store)));
}

class DesktopPage extends StatefulWidget {
  const DesktopPage({super.key, required this.store});
  final DesktopStore store;
  @override
  State<DesktopPage> createState() => _DesktopPageState();
}

class _DesktopPageState extends State<DesktopPage> {
  final _service = TextEditingController(text: 'desktop:my-computer');
  final _invitation = TextEditingController();
  int? _display;
  bool _control = false, _relayOnly = false, _showInvitation = false;
  // Full desktop access is the default once the user has explicitly installed
  // and authorized the protected application. Limited sharing is a deliberate
  // choice, never a silent fallback when UAC becomes inaccessible.
  bool _ordinaryOnly = false;
  bool _remember = false, _autoStart = false;
  bool _background = false;
  bool _viewerOpen = false;
  int _viewerOpenSerial = 0;
  @override
  void initState() {
    super.initState();
    scheduleMicrotask(() {
      if (mounted) unawaited(widget.store.load());
    });
  }

  @override
  void dispose() {
    _service.dispose();
    _invitation.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.store,
    builder: (context, _) {
      final store = widget.store;
      final l = context.l10n;
      final hostHelper = store.capabilities.helper;
      final helperReady =
          hostHelper?.installed == true &&
          hostHelper?.running == true &&
          hostHelper?.authorized == true;
      final setupAction = hostHelper == null || helperReady
          ? null
          : !hostHelper.installed ||
                !hostHelper.canOpenApplication && !hostHelper.authorized
          ? 'install'
          : !hostHelper.running
          ? 'enable'
          : 'open';
      final needsSecureSetup =
          hostHelper != null && !_ordinaryOnly && !helperReady;
      return Scaffold(
        appBar: AppBar(
          title: Text(l.desktopTitle),
          actions: [
            if (store.busy)
              const Padding(
                padding: EdgeInsets.all(16),
                child: SizedBox.square(
                  dimension: 20,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
              ),
          ],
        ),
        body: SafeArea(
          child: Align(
            alignment: Alignment.topCenter,
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 800),
              child: ListView(
                padding: const EdgeInsets.all(20),
                children: [
                  Text(l.desktopPreview),
                  if (store.error != null)
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 12),
                      child: Text(
                        store.error!,
                        style: TextStyle(
                          color: Theme.of(context).colorScheme.error,
                        ),
                      ),
                    ),
                  const SizedBox(height: 20),
                  if (store.snapshot.background case final background?
                      when background.configured) ...[
                    Text(
                      l.desktopBackgroundTitle,
                      style: Theme.of(context).textTheme.titleMedium,
                    ),
                    Text(
                      '${background.service ?? ''} · ${background.server ?? ''}',
                    ),
                    Text(
                      background.enabled
                          ? l.desktopBackgroundActive
                          : l.desktopBackgroundStopped,
                    ),
                    if (background.message.isNotEmpty) Text(background.message),
                    if (background.autoStart) Text(l.desktopBackgroundStartup),
                    Wrap(
                      spacing: 8,
                      children: [
                        if (!background.enabled && !store.sharing)
                          TextButton(
                            onPressed: store.busy
                                ? null
                                : () => store.backgroundAction('start'),
                            child: Text(l.desktopTrustRestore),
                          ),
                        TextButton(
                          onPressed: store.busy
                              ? null
                              : () => store.backgroundAction('revoke'),
                          child: Text(l.desktopTrustRevoke),
                        ),
                      ],
                    ),
                    const Divider(),
                  ],
                  if (store.trust.saved case final saved?
                      when store.snapshot.background?.configured != true) ...[
                    Text(
                      l.desktopTrustTitle,
                      style: Theme.of(context).textTheme.titleMedium,
                    ),
                    Text('${saved.service} · ${saved.relay}'),
                    Text(
                      saved.allowControl
                          ? l.desktopTrustControl
                          : l.desktopTrustView,
                    ),
                    if (saved.autoStart) Text(l.desktopTrustStartupEnabled),
                    if (!store.trust.matchesRelay)
                      Text(l.desktopTrustOtherRelay),
                    Wrap(
                      spacing: 8,
                      children: [
                        if (!store.sharing)
                          TextButton(
                            onPressed: store.busy || !store.trust.matchesRelay
                                ? null
                                : () async {
                                    await store.restoreHost();
                                    if (mounted) {
                                      setState(() => _showInvitation = true);
                                    }
                                  },
                            child: Text(l.desktopTrustRestore),
                          ),
                        TextButton(
                          onPressed: store.revokeTrust,
                          child: Text(l.desktopTrustRevoke),
                        ),
                      ],
                    ),
                    const Divider(),
                  ],
                  if (store.capabilities.helper case final helper?) ...[
                    Card(
                      margin: EdgeInsets.zero,
                      child: Padding(
                        padding: const EdgeInsets.all(16),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Row(
                              children: [
                                Icon(
                                  helperReady
                                      ? Icons.verified_user_outlined
                                      : Icons.admin_panel_settings_outlined,
                                ),
                                const SizedBox(width: 8),
                                Expanded(
                                  child: Text(
                                    l.desktopSecureSetupTitle,
                                    style: Theme.of(
                                      context,
                                    ).textTheme.titleMedium,
                                  ),
                                ),
                              ],
                            ),
                            const SizedBox(height: 8),
                            Text(switch (setupAction) {
                              'install' => l.desktopSecureSetupRequired,
                              'enable' => l.desktopSecureSetupStopped,
                              'open' => l.desktopSecureSetupOpen,
                              _ => l.desktopSecureSetupReady,
                            }),
                            if (!helperReady) ...[
                              const SizedBox(height: 12),
                              FilledButton.icon(
                                key: const Key('desktop-secure-setup-action'),
                                // A cached background grant must not disable
                                // opening its approved app or restarting an
                                // unavailable service. FFI still serializes
                                // install/enable against a local host.
                                onPressed:
                                    store.busy ||
                                        (setupAction != 'open' &&
                                            store.sharing &&
                                            store
                                                    .snapshot
                                                    .background
                                                    ?.enabled !=
                                                true)
                                    ? null
                                    : () => store.helperAction(setupAction!),
                                icon: const Icon(Icons.shield_outlined),
                                label: Text(switch (setupAction) {
                                  'install' => l.desktopHelperInstall,
                                  'enable' => l.desktopHelperEnable,
                                  _ => l.desktopHelperOpen,
                                }),
                              ),
                            ],
                            if (!store.sharing)
                              SwitchListTile(
                                key: const Key('desktop-ordinary-only'),
                                contentPadding: EdgeInsets.zero,
                                title: Text(l.desktopOrdinaryOnly),
                                subtitle: Text(l.desktopOrdinaryOnlyHelp),
                                value: _ordinaryOnly,
                                onChanged: store.busy
                                    ? null
                                    : (value) => setState(() {
                                        _ordinaryOnly = value;
                                        if (value) _background = false;
                                      }),
                              ),
                            ExpansionTile(
                              key: const Key('desktop-helper-maintenance'),
                              tilePadding: EdgeInsets.zero,
                              title: Text(l.desktopHelperMaintenance),
                              children: [
                                Wrap(
                                  spacing: 8,
                                  children: [
                                    TextButton(
                                      onPressed: store.busy || store.sharing
                                          ? null
                                          : () => store.helperAction('install'),
                                      child: Text(l.desktopHelperInstall),
                                    ),
                                    if (helper.installed) ...[
                                      TextButton(
                                        key: const Key(
                                          'desktop-helper-disable',
                                        ),
                                        // An unavailable management pipe must
                                        // never hide the OS service stop path.
                                        onPressed: store.busy
                                            ? null
                                            : () =>
                                                  store.helperAction('disable'),
                                        child: Text(l.desktopHelperDisable),
                                      ),
                                      TextButton(
                                        key: const Key(
                                          'desktop-helper-uninstall',
                                        ),
                                        onPressed: store.busy
                                            ? null
                                            : () => store.helperAction(
                                                'uninstall',
                                              ),
                                        child: Text(l.desktopHelperUninstall),
                                      ),
                                    ],
                                  ],
                                ),
                              ],
                            ),
                          ],
                        ),
                      ),
                    ),
                    const Divider(),
                  ],
                  Text(
                    l.desktopShare,
                    style: Theme.of(context).textTheme.titleLarge,
                  ),
                  const SizedBox(height: 12),
                  if (!store.capabilities.host)
                    Text(l.desktopHostUnsupported)
                  else if (store.sharing) ...[
                    Text(l.desktopSharing),
                    Text(store.snapshot.host?.message ?? ''),
                    SwitchListTile(
                      contentPadding: EdgeInsets.zero,
                      title: Text(l.desktopAllowControl),
                      value: store.snapshot.host?.controlAllowed == true,
                      onChanged:
                          store.busy &&
                              store.snapshot.host?.controlAllowed != true
                          ? null
                          : store.allowControl,
                    ),
                    Wrap(
                      spacing: 8,
                      runSpacing: 8,
                      children: [
                        FilledButton.icon(
                          key: const Key('desktop-stop'),
                          onPressed: store.stopHost,
                          icon: const Icon(Icons.stop_circle_outlined),
                          label: Text(l.desktopStop),
                        ),
                        TextButton(
                          onPressed: () => setState(
                            () => _showInvitation = !_showInvitation,
                          ),
                          child: Text(l.desktopShowInvitation),
                        ),
                      ],
                    ),
                    if (_showInvitation && store.invitation != null) ...[
                      const SizedBox(height: 12),
                      Text(l.desktopInvitationHelp),
                      SelectableText(store.invitation!, maxLines: 4),
                      TextButton.icon(
                        onPressed: () => Clipboard.setData(
                          ClipboardData(text: store.invitation!),
                        ),
                        icon: const Icon(Icons.copy),
                        label: Text(l.desktopCopy),
                      ),
                    ],
                  ] else ...[
                    if (store.capabilities.message.isNotEmpty)
                      Text(store.capabilities.message),
                    TextField(
                      controller: _service,
                      decoration: InputDecoration(labelText: l.desktopService),
                    ),
                    const SizedBox(height: 12),
                    DropdownButtonFormField<int>(
                      isExpanded: true,
                      initialValue:
                          _display ??
                          store.capabilities.displays.firstOrNull?.id,
                      decoration: InputDecoration(labelText: l.desktopDisplay),
                      items: [
                        for (final d in store.capabilities.displays)
                          DropdownMenuItem(
                            value: d.id,
                            child: Text(
                              '${d.name} · ${d.width} × ${d.height}',
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                      ],
                      onChanged: (id) => setState(() => _display = id),
                    ),
                    SwitchListTile(
                      contentPadding: EdgeInsets.zero,
                      title: Text(l.desktopAllowControl),
                      value: _control,
                      onChanged: (value) => setState(() => _control = value),
                    ),
                    if (helperReady)
                      SwitchListTile(
                        key: const Key('desktop-background'),
                        contentPadding: EdgeInsets.zero,
                        title: Text(l.desktopBackgroundTitle),
                        subtitle: Text(l.desktopBackgroundHelp),
                        value: _background,
                        onChanged: (value) => setState(() {
                          _background = value;
                          if (value) {
                            _ordinaryOnly = false;
                            _remember = true;
                            _autoStart = true;
                          }
                        }),
                      ),
                    if (store.trust.supported && !_background) ...[
                      SwitchListTile(
                        contentPadding: EdgeInsets.zero,
                        title: Text(l.desktopTrustRemember),
                        subtitle: Text(l.desktopTrustRememberHelp),
                        value: _remember,
                        onChanged: (value) => setState(() {
                          _remember = value;
                          if (!value) _autoStart = false;
                        }),
                      ),
                      if (_remember)
                        SwitchListTile(
                          contentPadding: EdgeInsets.zero,
                          title: Text(l.desktopTrustStartup),
                          subtitle: Text(l.desktopTrustStartupHelp),
                          value: _autoStart,
                          onChanged: (value) =>
                              setState(() => _autoStart = value),
                        ),
                    ],
                    FilledButton.icon(
                      key: const Key('desktop-share'),
                      onPressed:
                          store.busy ||
                              store.capabilities.displays.isEmpty ||
                              needsSecureSetup
                          ? null
                          : () async {
                              await store.startHost(
                                _service.text.trim(),
                                _display ??
                                    store.capabilities.displays.first.id,
                                _control,
                                _relayOnly,
                                privileged: !_ordinaryOnly && helperReady,
                                remember: _remember,
                                autoStart: _autoStart,
                                background: _background,
                              );
                              if (mounted) {
                                setState(() => _showInvitation = true);
                              }
                            },
                      icon: const Icon(Icons.screen_share_outlined),
                      label: Text(l.desktopShare),
                    ),
                    if (needsSecureSetup)
                      Padding(
                        padding: const EdgeInsets.only(top: 8),
                        child: Text(l.desktopShareNeedsSetup),
                      ),
                  ],
                  const Divider(height: 40),
                  Text(
                    l.desktopConnect,
                    style: Theme.of(context).textTheme.titleLarge,
                  ),
                  const SizedBox(height: 12),
                  if (store.connected) ...[
                    Text(store.snapshot.viewer?.message ?? ''),
                    Wrap(
                      spacing: 8,
                      children: [
                        FilledButton(
                          onPressed: () => _viewer(context),
                          child: Text(l.desktopOpen),
                        ),
                        TextButton(
                          onPressed: store.disconnect,
                          child: Text(l.desktopDisconnect),
                        ),
                      ],
                    ),
                  ] else ...[
                    TextField(
                      key: const Key('desktop-invitation'),
                      controller: _invitation,
                      minLines: 2,
                      maxLines: 4,
                      autocorrect: false,
                      enableSuggestions: false,
                      decoration: InputDecoration(labelText: l.desktopPaste),
                    ),
                    const SizedBox(height: 12),
                    FilledButton.icon(
                      key: const Key('desktop-connect'),
                      onPressed: store.busy
                          ? null
                          : () async {
                              final viewerSerial = _viewerOpenSerial;
                              await store.connect(
                                _invitation.text.trim(),
                                _relayOnly,
                              );
                              _invitation.clear();
                              if (context.mounted &&
                                  store.connected &&
                                  viewerSerial == _viewerOpenSerial) {
                                _viewer(context);
                              }
                            },
                      icon: const Icon(Icons.desktop_windows_outlined),
                      label: Text(l.desktopConnect),
                    ),
                  ],
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: Text(l.desktopRelayOnly),
                    value: _relayOnly,
                    onChanged: store.busy
                        ? null
                        : (value) => setState(() => _relayOnly = value),
                  ),
                ],
              ),
            ),
          ),
        ),
      );
    },
  );
  Future<void> _viewer(BuildContext context) async {
    if (_viewerOpen) return;
    _viewerOpen = true;
    _viewerOpenSerial++;
    final route = MaterialPageRoute<void>(
      builder: (_) => DesktopViewer(store: widget.store),
    );
    try {
      await Navigator.of(context).push(route);
      // Keep one foreground owner until the popped viewer is fully disposed.
      await route.completed;
    } finally {
      _viewerOpen = false;
    }
  }
}

/// Persistent local stop, including while another page is open.
class DesktopSharingBanner extends StatelessWidget {
  const DesktopSharingBanner({super.key, required this.store});
  final DesktopStore store;
  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: store,
    builder: (context, _) => store.sharing
        ? Material(
            color: Theme.of(context).colorScheme.secondaryContainer,
            child: SafeArea(
              bottom: false,
              child: Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 4,
                ),
                child: Row(
                  children: [
                    const Icon(Icons.screen_share_outlined),
                    const SizedBox(width: 8),
                    Expanded(child: Text(context.l10n.desktopSharing)),
                    TextButton(
                      onPressed: store.stopHost,
                      child: Text(context.l10n.desktopStop),
                    ),
                  ],
                ),
              ),
            ),
          )
        : const SizedBox.shrink(),
  );
}
