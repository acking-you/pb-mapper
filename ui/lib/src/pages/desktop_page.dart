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
  late final DesktopDemandHandle _demand;
  int? _display;
  bool _relayOnly = false, _viewerOpen = false;

  @override
  void initState() {
    super.initState();
    _demand = widget.store.demand(DesktopDemand.page);
  }

  @override
  void dispose() {
    _demand.dispose();
    _service.dispose();
    _invitation.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([
      widget.store.hostView,
      widget.store.session,
      _invitation,
    ]),
    builder: (context, _) {
      final store = widget.store;
      final host = store.hostView.value;
      final session = store.session.value;
      final l = context.l10n;
      final error = host.error ?? session.failure?.toString();
      final fullyEnabled =
          host.sharing && host.background && host.controlAllowed;
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
                  if (error != null)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 12),
                      child: Text(
                        error,
                        style: TextStyle(
                          color: Theme.of(context).colorScheme.error,
                        ),
                      ),
                    ),
                  if (host.supported) ...[
                    Text(
                      l.desktopShare,
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 12),
                    Text(
                      fullyEnabled ? l.desktopEnabledHelp : l.desktopEnableHelp,
                    ),
                    const SizedBox(height: 12),
                    if (host.sharing)
                      Wrap(
                        spacing: 8,
                        runSpacing: 8,
                        children: [
                          OutlinedButton.icon(
                            onPressed: host.invitation == null
                                ? null
                                : () => Clipboard.setData(
                                    ClipboardData(text: host.invitation!),
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
                    if (!fullyEnabled)
                      FilledButton.icon(
                        key: const Key('desktop-share'),
                        onPressed: host.enabling || host.displays.isEmpty
                            ? null
                            : () => store.enableHost(
                                displayId:
                                    _display ??
                                    host.displayId ??
                                    host.displays.first.id,
                                allowControl: true,
                                service: host.service ?? _service.text.trim(),
                              ),
                        icon: host.enabling
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
                  if (session.target != null)
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
                      smartQuotesType: SmartQuotesType.disabled,
                      smartDashesType: SmartDashesType.disabled,
                      decoration: InputDecoration(labelText: l.desktopPaste),
                    ),
                    const SizedBox(height: 12),
                    FilledButton.icon(
                      key: const Key('desktop-connect'),
                      onPressed: _invitation.text.trim().isEmpty
                          ? null
                          : () {
                              // The viewer holds the session's demand from
                              // the start, so it never pauses in between.
                              unawaited(
                                store.connect(
                                  DesktopTarget(
                                    invitation: _invitation.text.trim(),
                                    name: l.desktopTitle,
                                    relayOnly: _relayOnly,
                                  ),
                                ),
                              );
                              _viewer(context);
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
                      if (host.supported) ..._advanced(context, host),
                      SwitchListTile(
                        contentPadding: EdgeInsets.zero,
                        title: Text(l.desktopRelayOnly),
                        value: _relayOnly,
                        onChanged: (v) => setState(() => _relayOnly = v),
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

  List<Widget> _advanced(BuildContext context, DesktopHostView host) {
    final store = widget.store;
    final l = context.l10n;
    return [
      if (!host.sharing) ...[
        TextField(
          controller: _service,
          decoration: InputDecoration(labelText: l.desktopService),
        ),
        DropdownButtonFormField<int>(
          isExpanded: true,
          initialValue: _display ?? host.displays.firstOrNull?.id,
          decoration: InputDecoration(labelText: l.desktopDisplay),
          items: [
            for (final d in host.displays)
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
      if (host.sharing)
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          title: Text(l.desktopAllowControl),
          value: host.controlAllowed,
          onChanged: store.allowControl,
        ),
      Text(l.desktopStopHelp),
      if (host.configured || host.sharing)
        TextButton(
          onPressed: host.enabling ? null : store.revokeTrust,
          child: Text(l.desktopTrustRevoke),
        ),
      Wrap(
        spacing: 8,
        children: [
          TextButton(
            onPressed: host.enabling
                ? null
                : () => store.helperAction('install'),
            child: Text(l.desktopHelperInstall),
          ),
          if (host.helper != DesktopHelperState.none) ...[
            TextButton(
              key: const Key('desktop-helper-disable'),
              onPressed: host.enabling
                  ? null
                  : () => store.helperAction('disable'),
              child: Text(l.desktopHelperDisable),
            ),
            TextButton(
              key: const Key('desktop-helper-uninstall'),
              onPressed: host.enabling
                  ? null
                  : () => store.helperAction('uninstall'),
              child: Text(l.desktopHelperUninstall),
            ),
          ],
        ],
      ),
    ];
  }

  Future<void> _viewer(BuildContext context) async {
    if (_viewerOpen) return;
    _viewerOpen = true;
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
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: store.hostView,
    builder: (context, host, _) => host.sharing
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
