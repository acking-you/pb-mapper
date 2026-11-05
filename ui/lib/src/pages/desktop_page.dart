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
  bool _relayOnly = false, _viewerOpen = false;
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
      final background = store.snapshot.background;
      final helper = store.capabilities.helper;
      final fullyEnabled =
          background?.enabled == true &&
          background?.autoStart == true &&
          store.snapshot.host?.controlAllowed == true;
      return Scaffold(
        appBar: AppBar(title: Text(l.desktopTitle)),
        body: SafeArea(
          child: Align(
            alignment: Alignment.topCenter,
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 800),
              child: ListView(
                padding: const EdgeInsets.all(20),
                children: [
                  if (store.error != null)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 12),
                      child: Text(
                        store.error!,
                        style: TextStyle(
                          color: Theme.of(context).colorScheme.error,
                        ),
                      ),
                    ),
                  if (store.capabilities.host) ...[
                    Text(
                      l.desktopShare,
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 12),
                    Text(
                      fullyEnabled ? l.desktopEnabledHelp : l.desktopEnableHelp,
                    ),
                    const SizedBox(height: 12),
                    if (store.sharing) ...[
                      if (store.snapshot.host?.message.isNotEmpty == true)
                        Text(store.snapshot.host!.message),
                      if (background?.message.isNotEmpty == true &&
                          background?.message != store.snapshot.host?.message)
                        Text(background!.message),
                      Wrap(
                        spacing: 8,
                        runSpacing: 8,
                        children: [
                          OutlinedButton.icon(
                            onPressed: store.invitation == null
                                ? null
                                : () => Clipboard.setData(
                                    ClipboardData(text: store.invitation!),
                                  ),
                            icon: const Icon(Icons.copy),
                            label: Text(l.desktopCopy),
                          ),
                          TextButton.icon(
                            key: const Key('desktop-stop'),
                            onPressed: store.stopHost,
                            icon: const Icon(Icons.stop_circle_outlined),
                            label: Text(l.desktopStop),
                          ),
                        ],
                      ),
                    ],
                    if (!fullyEnabled)
                      FilledButton.icon(
                        key: const Key('desktop-share'),
                        onPressed:
                            store.busy || store.capabilities.displays.isEmpty
                            ? null
                            : () => store.enableHost(
                                background?.service ??
                                    store.trust.saved?.service ??
                                    _service.text.trim(),
                                _display ??
                                    store.snapshot.host?.display?.id ??
                                    store.trust.saved?.displayId ??
                                    store.capabilities.displays.first.id,
                              ),
                        icon: store.busy
                            ? const SizedBox.square(
                                dimension: 18,
                                child: CircularProgressIndicator(
                                  strokeWidth: 2,
                                ),
                              )
                            : const Icon(Icons.screen_share_outlined),
                        label: Text(l.desktopEnable),
                      ),
                    const Divider(height: 40),
                  ],
                  Text(
                    l.desktopConnect,
                    style: Theme.of(context).textTheme.titleLarge,
                  ),
                  const SizedBox(height: 12),
                  if (store.connected)
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
                    )
                  else ...[
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
                              final serial = _viewerOpenSerial;
                              await store.connect(
                                _invitation.text.trim(),
                                _relayOnly,
                              );
                              _invitation.clear();
                              if (context.mounted &&
                                  store.connected &&
                                  serial == _viewerOpenSerial) {
                                _viewer(context);
                              }
                            },
                      icon: const Icon(Icons.desktop_windows_outlined),
                      label: Text(l.desktopConnect),
                    ),
                  ],
                  const SizedBox(height: 20),
                  ExpansionTile(
                    key: const Key('desktop-helper-maintenance'),
                    tilePadding: EdgeInsets.zero,
                    title: Text(l.desktopAdvanced),
                    children: [
                      if (store.capabilities.host) ...[
                        if (!store.sharing) ...[
                          TextField(
                            controller: _service,
                            decoration: InputDecoration(
                              labelText: l.desktopService,
                            ),
                          ),
                          DropdownButtonFormField<int>(
                            isExpanded: true,
                            initialValue:
                                _display ??
                                store.capabilities.displays.firstOrNull?.id,
                            decoration: InputDecoration(
                              labelText: l.desktopDisplay,
                            ),
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
                        ],
                        if (store.sharing)
                          SwitchListTile(
                            contentPadding: EdgeInsets.zero,
                            title: Text(l.desktopAllowControl),
                            value: store.snapshot.host?.controlAllowed == true,
                            onChanged: store.allowControl,
                          ),
                        Text(l.desktopStopHelp),
                        if (background?.configured == true ||
                            store.trust.saved != null)
                          TextButton(
                            onPressed: store.busy ? null : store.revokeTrust,
                            child: Text(l.desktopTrustRevoke),
                          ),
                        if (helper != null)
                          Wrap(
                            spacing: 8,
                            children: [
                              TextButton(
                                onPressed: store.busy
                                    ? null
                                    : () => store.helperAction('install'),
                                child: Text(l.desktopHelperInstall),
                              ),
                              if (helper.installed) ...[
                                TextButton(
                                  key: const Key('desktop-helper-disable'),
                                  onPressed: store.busy
                                      ? null
                                      : () => store.helperAction('disable'),
                                  child: Text(l.desktopHelperDisable),
                                ),
                                TextButton(
                                  key: const Key('desktop-helper-uninstall'),
                                  onPressed: store.busy
                                      ? null
                                      : () => store.helperAction('uninstall'),
                                  child: Text(l.desktopHelperUninstall),
                                ),
                              ],
                            ],
                          ),
                      ],
                      SwitchListTile(
                        contentPadding: EdgeInsets.zero,
                        title: Text(l.desktopRelayOnly),
                        value: _relayOnly,
                        onChanged: store.busy
                            ? null
                            : (v) => setState(() => _relayOnly = v),
                      ),
                    ],
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
