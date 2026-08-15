import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';

import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart'
    show ServiceKeyManager, AppNavigationManager;
import 'package:pb_mapper_ui/src/widgets/connection_view.dart';
import 'package:pb_mapper_ui/src/widgets/list_card.dart';

/// What the server has registered, and what it is holding for each.
///
/// This shared the status page, which made one screen answer two questions and
/// squeezed the longer of the two — the list you actually scroll — into half a
/// window. It is its own destination now.
///
/// Expanding a service asks the server what connections it holds for that key.
/// That is the protocol's structured `Service` query, the same data the status
/// page used to show as a `format!("{map:?}")` dump of the whole map.
class RegisteredServicesView extends StatefulWidget {
  const RegisteredServicesView({super.key, this.api});

  /// Defaults to the real FFI-backed client; tests pass a fake.
  final PbMapperApiClient? api;

  @override
  State<RegisteredServicesView> createState() => _RegisteredServicesViewState();
}

class _RegisteredServicesViewState extends State<RegisteredServicesView> {
  late final PbMapperApiClient _api = widget.api ?? PbMapperApi();

  List<String> _services = [];
  bool _loading = true;

  /// Connections per key, fetched when a service is first expanded. Absent
  /// means "not asked yet", which is not the same as "none held".
  final Map<String, List<ServiceConnInfo>> _conns = {};
  final Set<String> _loadingConns = {};

  ChangeSubscription? _changes;


  @override

  void initState() {

    super.initState();

    // Reload when anything changes this list, including a change made

    // from a terminal while this window was open.

    _changes = ChangeSubscription.listen(

      PbMapperService.changeStream,

      {StateChangeKind.services, StateChangeKind.server},

      (_) { if (mounted) _load(); },

    );
    _load();
  }

  @override

  void dispose() {

    _changes?.cancel();

    super.dispose();

  }


  Future<void> _load() async {
    setState(() => _loading = true);
    try {
      final status = await _api.forceRefreshServerStatus();
      if (!mounted) return;
      setState(() {
        _services = status.registeredServices;
        _loading = false;
        // A key that has gone away should not keep showing its old
        // connections behind an expander.
        _conns.removeWhere((key, _) => !_services.contains(key));
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _services = [];
        _loading = false;
      });
    }
  }

  Future<void> _loadConns(String key) async {
    if (_loadingConns.contains(key)) return;
    setState(() => _loadingConns.add(key));
    try {
      final conns = await _api.getServiceConns(key);
      if (!mounted) return;
      setState(() {
        _conns[key] = conns;
        _loadingConns.remove(key);
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _conns[key] = const [];
        _loadingConns.remove(key);
      });
    }
  }

  void _connectTo(String key) {
    ServiceKeyManager.setSelectedServiceKey(key);
    AppNavigationManager.navigateToConnectPage();
    showToast(context, context.l10n.navigatedToConnect(key));
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }

    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          ListPaneHeader(
            title: context.l10n.registeredServicesTitle,
            count: _services.length,
            onRefresh: _load,
            refreshTooltip: context.l10n.refreshServiceList,
          ),
          Expanded(
            child: _services.isEmpty
                ? ListView(
                    children: [
                      ListPaneEmpty(
                        icon: Icons.dns_outlined,
                        title: context.l10n.noServicesRegistered,
                        hint: context.l10n.noServicesHint,
                      ),
                    ],
                  )
                // Selection lives here rather than in each Text. SelectableText
                // takes the tap for its own cursor, which would kill the
                // tap-to-expand on the row; SelectionArea leaves taps alone and
                // still lets a drag select across the whole list.
                : SelectionArea(
                    child: ListView.builder(
                      itemCount: _services.length,
                      itemBuilder: (context, i) => _ServiceTile(
                        serviceKey: _services[i],
                        conns: _conns[_services[i]],
                        loading: _loadingConns.contains(_services[i]),
                        onExpand: () => _loadConns(_services[i]),
                        onConnect: () => _connectTo(_services[i]),
                      ),
                    ),
                  ),
          ),
        ],
      ),
    );
  }
}

/// One registered key, and on demand what the server holds for it.
///
/// Expansion is handled here rather than with an ExpansionTile, because that
/// widget wants the whole header as its tap target and the header's main
/// content is a key you need to be able to select. Selecting text and
/// expanding a row cannot share one gesture, so the chevron owns the
/// expansion and the key keeps its selection.
class _ServiceTile extends StatefulWidget {
  const _ServiceTile({
    required this.serviceKey,
    required this.conns,
    required this.loading,
    required this.onExpand,
    required this.onConnect,
  });

  final String serviceKey;
  final List<ServiceConnInfo>? conns;
  final bool loading;
  final VoidCallback onExpand;
  final VoidCallback onConnect;

  @override
  State<_ServiceTile> createState() => _ServiceTileState();
}

class _ServiceTileState extends State<_ServiceTile> {
  bool _expanded = false;

  void _toggle() {
    setState(() => _expanded = !_expanded);
    if (_expanded && widget.conns == null) widget.onExpand();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final conns = widget.conns;

    return ListCardShell(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          // The whole row opens it, not just the chevron. The key stays
          // selectable because the list is wrapped in a SelectionArea rather
          // than built from SelectableText — that widget would take the tap
          // for itself and leave the row dead where the text is.
          InkWell(
            onTap: _toggle,
            borderRadius: BorderRadius.circular(8),
            child: Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(
                        widget.serviceKey,
                        style: theme.textTheme.titleSmall?.copyWith(
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                      if (conns != null) ...[
                        const SizedBox(height: 2),
                        Text(
                          context.l10n.connCount(conns.length),
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                      ],
                    ],
                  ),
                ),
                const SizedBox(width: 8),
                CopyIconAction(value: widget.serviceKey),
                ListCardIconAction(
                  icon: Icons.link_rounded,
                  tooltip: context.l10n.tapToConnect,
                  onPressed: widget.onConnect,
                ),
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 6),
                  child: AnimatedRotation(
                    turns: _expanded ? 0.5 : 0,
                    duration: const Duration(milliseconds: 180),
                    child: Icon(
                      Icons.expand_more_rounded,
                      size: 20,
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                ),
              ],
            ),
          ),
          AnimatedCrossFade(
            duration: const Duration(milliseconds: 180),
            sizeCurve: Curves.easeOutCubic,
            crossFadeState: _expanded
                ? CrossFadeState.showSecond
                : CrossFadeState.showFirst,
            firstChild: const SizedBox(width: double.infinity),
            secondChild: Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (widget.loading)
                    const Padding(
                      padding: EdgeInsets.symmetric(vertical: 10),
                      child: SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      ),
                    )
                  else if ((conns ?? const []).isEmpty)
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 6),
                      child: Text(
                        context.l10n.serverHoldsNothing,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    )
                  else
                    for (final conn in conns!) ConnectionRow(conn: conn),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}
