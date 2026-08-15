import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/polling.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/views/log_view_page.dart';
import 'package:pb_mapper_ui/src/views/status_monitoring_view.dart';
import 'package:pb_mapper_ui/src/models/client_config.dart';
import 'package:pb_mapper_ui/src/widgets/client_card.dart';
import 'package:pb_mapper_ui/src/widgets/list_card.dart';

class ClientConnectionView extends StatefulWidget {
  /// Defaults to the real FFI-backed client; tests pass a fake.
  final PbMapperApiClient? api;

  const ClientConnectionView({
    this.api,
    super.key,
    this.pane = WorkspacePane.form,
    this.onCount,
  });

  /// The form or the list of existing connections. They are separate sidebar
  /// destinations, so only one is built at a time.
  final WorkspacePane pane;

  /// Reports how many connections exist, so the sidebar entry can show the
  /// count without fetching the list a second time.
  final ValueChanged<int>? onCount;

  @override
  State<ClientConnectionView> createState() => _ClientConnectionViewState();
}

class _ClientConnectionViewState extends State<ClientConnectionView> {
  late final PbMapperApiClient _api = widget.api ?? PbMapperApi();
  final _localAddressController = TextEditingController(text: '127.0.0.1:9090');
  final _serviceKeyInputController = TextEditingController();
  bool _isKeepAliveEnabled = true;
  String _selectedProtocol = 'TCP';
  String _serverAddress = 'localhost:7666'; // Will be updated from config
  String? _selectedServiceKey;
  List<String> _availableServices = [];
  List<ClientConfig> _clientConfigs = [];
  bool _isLoading = true;

  ChangeSubscription? _changes;


  @override

  void initState() {

    super.initState();

    // Reload when anything changes this list, including a change made

    // from a terminal while this window was open.

    _changes = ChangeSubscription.listen(

      PbMapperService.changeStream,

      {StateChangeKind.clients},

      (_) { if (mounted) _loadClientConfigs(); },

    );
    _loadClientConfigs();
    _loadConfig();
    _loadAvailableServices();
    _checkForPreSelectedService();
  }

  Future<void> _loadConfig() async {
    try {
      final config = await _api.fetchConfig();
      if (!mounted) return;
      setState(() {
        _serverAddress = config.serverAddress;
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _serverAddress = 'localhost:7666';
      });
    }
  }

  Future<void> _loadAvailableServices() async {
    try {
      final status = await _api.forceRefreshServerStatus();
      if (!mounted) return;
      setState(() {
        _availableServices = status.registeredServices;
        if (_selectedServiceKey != null &&
            !_availableServices.contains(_selectedServiceKey)) {
          _selectedServiceKey = null;
        }
        if (_selectedServiceKey == null && _availableServices.isNotEmpty) {
          _selectedServiceKey = _availableServices.first;
        }
      });
    } catch (e) {
      // Silently fail — user can still type service key manually
      debugPrint('Failed to load available services: $e');
    }
  }

  Future<void> _loadClientConfigs() async {
    try {
      final configs = await _api.getClientConfigs();
      if (!mounted) return;
      _updateClientConfigsFromSignal(configs);
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _clientConfigs = [];
        _isLoading = false;
      });
      widget.onCount?.call(0);
    }
  }

  void _updateClientConfigsFromSignal(List<ClientConfigInfo> configs) {
    final clientConfigs = configs.map((config) {
      final status = _parseClientStatus(config.status);
      return ClientConfig(
        serviceKey: config.serviceKey,
        localAddress: config.localAddress,
        protocol: config.protocol,
        enableKeepAlive: config.enableKeepAlive,
        createdAt: DateTime.fromMillisecondsSinceEpoch(
          config.createdAtMs.toInt(),
        ),
        updatedAt: DateTime.fromMillisecondsSinceEpoch(
          config.updatedAtMs.toInt(),
        ),
        status: status,
        statusMessage: config.statusMessage,
      );
    }).toList();

    setState(() {
      _clientConfigs = clientConfigs;
      _isLoading = false;
    });
    widget.onCount?.call(clientConfigs.length);
  }

  ClientStatus _parseClientStatus(String statusString) {
    switch (statusString.toLowerCase()) {
      case 'running':
        return ClientStatus.running;
      case 'retrying':
        return ClientStatus.retrying;
      case 'failed':
        return ClientStatus.failed;
      case 'stopped':
      default:
        return ClientStatus.stopped;
    }
  }

  void _checkForPreSelectedService() {
    // Check if there's a service key selected from status monitoring
    final selectedKey = ServiceKeyManager.getSelectedServiceKey();
    if (selectedKey != null) {
      _selectedServiceKey = selectedKey;
      _serviceKeyInputController.text = selectedKey;
      ServiceKeyManager.clearSelectedServiceKey();

      // Show a helpful message
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) {
          showToast(
            context,
            context.l10n.keyAutoSelected(selectedKey),
            kind: ToastKind.success,
          );
        }
      });
    }
  }

  @override
  void dispose() {

    _changes?.cancel();
    _localAddressController.dispose();
    _serviceKeyInputController.dispose();
    super.dispose();
  }

  String get _effectiveServiceKey {
    if (_selectedServiceKey != null && _selectedServiceKey!.isNotEmpty) {
      return _selectedServiceKey!;
    }
    return _serviceKeyInputController.text.trim();
  }

  void _connectService() {
    final serviceKey = _effectiveServiceKey;
    if (serviceKey.isEmpty) {
      showToast(context, context.l10n.serviceKeyNeeded);
      return;
    }

    // Check if client already exists
    final existingClient = _clientConfigs.firstWhereOrNull(
      (client) => client.serviceKey == serviceKey,
    );

    if (existingClient != null) {
      showToast(
        context,
        context.l10n.clientExists(serviceKey),
        kind: ToastKind.warning,
      );
      return;
    }

    _api
        .connectService(
          serviceKey: serviceKey,
          localAddress: _localAddressController.text,
          protocol: _selectedProtocol,
          enableKeepAlive: _isKeepAliveEnabled,
        )
        .then((result) {
          if (!mounted) return;
          showToast(
            context,
            result.message,
            kind: result.success ? ToastKind.success : ToastKind.error,
          );
          _loadClientConfigs();
          if (result.success) {
            _pollClientStatusUntilStable(serviceKey);
          }
        });
  }

  void _handleClientConnect(ClientConfig config) {
    _api
        .connectService(
          serviceKey: config.serviceKey,
          localAddress: config.localAddress,
          protocol: config.protocol,
          enableKeepAlive: config.enableKeepAlive,
        )
        .then((result) {
          if (!mounted) return;
          showToast(
            context,
            result.message,
            kind: result.success ? ToastKind.success : ToastKind.error,
          );
          _loadClientConfigs();
          if (result.success) {
            _pollClientStatusUntilStable(config.serviceKey);
          }
        });
  }

  /// Waits out the retry loop, then says how it went.
  ///
  /// `connectService` returning success only means the request was accepted;
  /// whether the tunnel came up shows a moment later. Reporting the first
  /// answer as the outcome is what put a green "client connection started"
  /// on screen while the client was failing to reach the server, so the
  /// settled state gets its own message.
  Future<void> _pollClientStatusUntilStable(String serviceKey) async {
    await waitUntilSettled(
      attempt: () async {
        if (!mounted) return true;
        await _loadClientConfigs();
        if (!mounted) return true;

        final config = _clientConfigs.firstWhereOrNull(
          (c) => c.serviceKey == serviceKey,
        );
        return config != null && config.status != ClientStatus.retrying;
      },
    );

    if (!mounted) return;
    final settled = _clientConfigs.firstWhereOrNull(
      (c) => c.serviceKey == serviceKey,
    );
    if (settled == null || settled.status != ClientStatus.failed) return;

    showToast(
      context,
      context.l10n.connectFailed(serviceKey),
      kind: ToastKind.error,
      description: settled.statusMessage.isEmpty ? null : settled.statusMessage,
    );
  }

  void _handleClientDisconnect(ClientConfig config) {
    _api.disconnectService(config.serviceKey).then((result) {
      if (!mounted) return;
      showToast(
        context,
        result.message,
        kind: result.success ? ToastKind.success : ToastKind.error,
      );
      _loadClientConfigs();
    });
  }

  void _handleClientDelete(ClientConfig config) {
    showDialog(
      context: context,
      // Named, so the toast below cannot accidentally use it: the dialog is
      // popped before the delete finishes, and its context goes with it.
      builder: (dialogContext) => AlertDialog(
        title: Text(dialogContext.l10n.deleteClientConfig),
        content: Text(
          dialogContext.l10n.deleteClientConfirm(config.serviceKey),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(),
            child: Text(dialogContext.l10n.cancel),
          ),
          TextButton(
            onPressed: () {
              Navigator.of(dialogContext).pop();
              _api.deleteClientConfig(config.serviceKey).then((result) {
                if (!mounted) return;
                // This view's context, guarded by `mounted`. Toasts go to an
                // overlay rather than the nearest Scaffold, so nothing has to
                // capture a messenger before the dialog closes.
                showToast(
                  context,
                  result.message,
                  kind: result.success ? ToastKind.success : ToastKind.error,
                );
                _loadClientConfigs();
              });
            },
            style: TextButton.styleFrom(foregroundColor: Colors.red),
            child: Text(context.l10n.delete),
          ),
        ],
      ),
    );
  }

  void _handleClientRefresh(ClientConfig config) {
    _api.getClientStatus(config.serviceKey).then((status) {
      if (!mounted) return;
      final configIndex = _clientConfigs.indexWhere(
        (c) => c.serviceKey == status.serviceKey,
      );
      if (configIndex != -1) {
        setState(() {
          _clientConfigs[configIndex].updateStatus(
            _parseClientStatus(status.status),
            status.message,
          );
        });
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    // The log pane replaces the body but not this State, so a peek at the logs
    // does not empty a half-typed form. It also has to sit outside the scroll
    // view below: it scrolls its own list and needs a bounded height.
    if (widget.pane == WorkspacePane.logs) {
      return const LogViewPage(showScaffold: false);
    }

    return Padding(
      padding: const EdgeInsets.all(16.0),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (widget.pane == WorkspacePane.form)
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(16.0),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        context.l10n.connectTitle,
                        style: Theme.of(context).textTheme.titleLarge,
                      ),
                      const SizedBox(height: 16),
                      DropdownButtonFormField<String>(
                        initialValue: _selectedProtocol,
                        items: ['TCP', 'UDP']
                            .map(
                              (protocol) => DropdownMenuItem(
                                value: protocol,
                                child: Text(protocol),
                              ),
                            )
                            .toList(),
                        onChanged: (value) {
                          setState(() => _selectedProtocol = value!);
                        },
                        decoration: InputDecoration(
                          labelText: context.l10n.protocol,
                          border: OutlineInputBorder(),
                        ),
                      ),
                      const SizedBox(height: 16),
                      Row(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Expanded(
                            child: _availableServices.isNotEmpty
                                ? DropdownButtonFormField<String>(
                                    initialValue:
                                        _availableServices.contains(
                                          _selectedServiceKey,
                                        )
                                        ? _selectedServiceKey
                                        : null,
                                    items: _availableServices.map((serviceKey) {
                                      return DropdownMenuItem(
                                        value: serviceKey,
                                        child: Text(serviceKey),
                                      );
                                    }).toList(),
                                    onChanged: (value) {
                                      setState(() {
                                        _selectedServiceKey = value;
                                        _serviceKeyInputController.text =
                                            value ?? '';
                                      });
                                    },
                                    decoration: InputDecoration(
                                      labelText: context.l10n.serviceKey,
                                      hintText: context.l10n.selectService,
                                      border: OutlineInputBorder(),
                                      prefixIcon: Icon(Icons.vpn_key),
                                    ),
                                  )
                                : TextField(
                                    controller: _serviceKeyInputController,
                                    onChanged: (value) {
                                      setState(
                                        () => _selectedServiceKey =
                                            value.trim().isEmpty
                                            ? null
                                            : value.trim(),
                                      );
                                    },
                                    decoration: InputDecoration(
                                      labelText: context.l10n.serviceKey,
                                      hintText: context.l10n.enterServiceKey,
                                      border: OutlineInputBorder(),
                                      prefixIcon: Icon(Icons.vpn_key),
                                    ),
                                  ),
                          ),
                          const SizedBox(width: 8),
                          IconButton(
                            onPressed: _loadAvailableServices,
                            icon: const Icon(Icons.refresh),
                            tooltip: context.l10n.refreshServiceList,
                          ),
                        ],
                      ),
                      const SizedBox(height: 16),
                      TextField(
                        controller: _localAddressController,
                        decoration: InputDecoration(
                          labelText: context.l10n.localAddress,
                          hintText: '127.0.0.1:9090',
                          border: OutlineInputBorder(),
                        ),
                      ),
                      const SizedBox(height: 16),
                      SwitchListTile(
                        title: Text(context.l10n.enableKeepAlive),
                        value: _isKeepAliveEnabled,
                        onChanged: (value) {
                          setState(() => _isKeepAliveEnabled = value);
                        },
                      ),
                      const SizedBox(height: 16),
                      Container(
                        padding: const EdgeInsets.all(12),
                        decoration: BoxDecoration(
                          border: Border.all(color: Colors.grey),
                          borderRadius: BorderRadius.circular(8),
                        ),
                        child: Row(
                          children: [
                            const Icon(Icons.dns, color: Colors.blue),
                            const SizedBox(width: 12),
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    context.l10n.serverAddress,
                                    style: const TextStyle(
                                      fontSize: 12,
                                      color: Colors.grey,
                                    ),
                                  ),
                                  const SizedBox(height: 4),
                                  Text(
                                    _serverAddress,
                                    style: const TextStyle(fontSize: 16),
                                  ),
                                ],
                              ),
                            ),
                            TextButton(
                              onPressed: () {
                                AppNavigationManager.navigateToConfigPage();
                              },
                              child: Text(
                                context.l10n.quickStartStep1Action,
                                style: const TextStyle(fontSize: 12),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            if (widget.pane == WorkspacePane.form) const SizedBox(height: 16),
            if (widget.pane == WorkspacePane.form)
              SizedBox(
                height: 48,
                width: double.infinity,
                child: ElevatedButton(
                  onPressed: _effectiveServiceKey.isNotEmpty
                      ? _connectService
                      : null,
                  style: ElevatedButton.styleFrom(
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(24),
                    ),
                  ),
                  child: const Text('Connect', style: TextStyle(fontSize: 16)),
                ),
              ),
            if (widget.pane == WorkspacePane.list && _isLoading) ...[
              const Padding(
                padding: EdgeInsets.only(top: 40),
                child: Center(child: CircularProgressIndicator()),
              ),
            ] else if (widget.pane == WorkspacePane.list &&
                _clientConfigs.isEmpty) ...[
              ListPaneHeader(
                title: context.l10n.activeConnections,
                count: 0,
                onRefresh: _loadClientConfigs,
                refreshTooltip: context.l10n.refreshAllStatus,
              ),
              ListPaneEmpty(
                icon: Icons.cable_outlined,
                title: context.l10n.noClientConfigs,
                hint: context.l10n.noClientConfigsHint,
              ),
            ] else if (widget.pane == WorkspacePane.list) ...[
              ListPaneHeader(
                title: context.l10n.activeConnections,
                count: _clientConfigs.length,
                onRefresh: _loadClientConfigs,
                refreshTooltip: context.l10n.refreshAllStatus,
              ),
              ..._clientConfigs.map(
                (config) => ClientCard(
                  key: Key(config.serviceKey),
                  config: config,
                  onConnectDisconnect: () => config.isLive
                      ? _handleClientDisconnect(config)
                      : _handleClientConnect(config),
                  onDelete: () => _handleClientDelete(config),
                  onRefresh: () => _handleClientRefresh(config),
                  onStatusChanged: (updatedConfig) {
                    final index = _clientConfigs.indexWhere(
                      (c) => c.serviceKey == updatedConfig.serviceKey,
                    );
                    if (index != -1) {
                      setState(() {
                        _clientConfigs[index] = updatedConfig;
                      });
                    }
                  },
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
