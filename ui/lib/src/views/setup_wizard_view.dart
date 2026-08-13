import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';

/// The first-run wizard: fills in what a new user needs, one question a screen.
///
/// It does not stop at the server address. Someone new does not yet know what a
/// service key is for, so the wizard carries them through registering or
/// connecting their first service and leaves them with something that works.
class SetupWizardView extends StatefulWidget {
  const SetupWizardView({
    super.key,
    required this.onFinished,
    required this.onSkip,
  });

  /// Called with the zone to open once setup succeeds.
  final ValueChanged<AppSection> onFinished;
  final VoidCallback onSkip;

  @override
  State<SetupWizardView> createState() => _SetupWizardViewState();
}

enum _Step { server, role, details, done }

class _SetupWizardViewState extends State<SetupWizardView> {
  final PbMapperApi _api = PbMapperApi();

  final _serverController = TextEditingController();
  final _keyController = TextEditingController();
  final _serviceKeyController = TextEditingController();
  final _localAddressController = TextEditingController();

  _Step _step = _Step.server;
  bool _isRegisterRole = true;
  String _protocol = 'TCP';
  bool _busy = false;
  String? _error;
  String? _serverNote;
  bool? _serverReachable;

  static const int _totalSteps = 3;

  @override
  void dispose() {
    _serverController.dispose();
    _keyController.dispose();
    _serviceKeyController.dispose();
    _localAddressController.dispose();
    super.dispose();
  }

  /// Prefill the local address with the default for the chosen role, so the
  /// common case is one keystroke instead of remembering a format.
  void _pickRole({required bool register}) {
    setState(() {
      _isRegisterRole = register;
      _localAddressController.text = register
          ? '127.0.0.1:8080'
          : '127.0.0.1:9090';
      _step = _Step.details;
    });
  }

  Future<void> _saveServer() async {
    final address = _serverController.text.trim();
    final key = _keyController.text.trim();
    final l10n = context.l10n;

    if (!_looksLikeHostPort(address)) {
      setState(() => _error = l10n.setupServerInvalid);
      return;
    }
    if (key.isNotEmpty && key.length != 32) {
      setState(() => _error = l10n.setupKeyInvalid);
      return;
    }

    setState(() {
      _busy = true;
      _error = null;
      _serverNote = l10n.setupCheckingServer;
    });

    final result = await _api.updateConfig(
      serverAddress: address,
      keepAlive: true,
      msgHeaderKey: key,
    );
    if (!mounted) return;
    if (!result.success) {
      setState(() {
        _busy = false;
        _serverNote = null;
        _error = result.message;
      });
      return;
    }

    // A reachable server is reassuring but not required: the server may not be
    // up yet, and the registration retries on its own.
    final status = await _api.forceRefreshServerStatus();
    if (!mounted) return;
    setState(() {
      _busy = false;
      _serverReachable = status.serverAvailable;
      _serverNote = status.serverAvailable
          ? l10n.setupServerOk
          : l10n.setupServerFailed;
      _step = _Step.role;
    });
  }

  Future<void> _finishDetails() async {
    final serviceKey = _serviceKeyController.text.trim();
    final localAddress = _localAddressController.text.trim();
    final l10n = context.l10n;

    if (serviceKey.isEmpty) {
      setState(() => _error = l10n.serviceKeyRequired);
      return;
    }
    if (!_looksLikeHostPort(localAddress)) {
      setState(() => _error = l10n.setupServerInvalid);
      return;
    }

    setState(() {
      _busy = true;
      _error = null;
    });

    final result = _isRegisterRole
        ? await _api.registerService(
            serviceKey: serviceKey,
            localAddress: localAddress,
            protocol: _protocol,
            enableEncryption: true,
            enableKeepAlive: true,
          )
        : await _api.connectService(
            serviceKey: serviceKey,
            localAddress: localAddress,
            protocol: _protocol,
            enableKeepAlive: true,
          );

    if (!mounted) return;
    if (!result.success) {
      setState(() {
        _busy = false;
        _error = l10n.setupFailed(result.message);
      });
      return;
    }
    setState(() {
      _busy = false;
      _step = _Step.done;
    });
  }

  static bool _looksLikeHostPort(String value) {
    final parts = value.split(':');
    if (parts.length != 2 || parts[0].trim().isEmpty) return false;
    final port = int.tryParse(parts[1].trim());
    return port != null && port > 0 && port <= 65535;
  }

  @override
  Widget build(BuildContext context) {
    final isMobile = ResponsiveLayout.isMobile(context);

    return Scaffold(
      backgroundColor: isMobile ? null : Colors.transparent,
      body: Center(
        child: SingleChildScrollView(
          padding: EdgeInsets.symmetric(
            horizontal: ResponsiveLayout.getHorizontalPadding(context),
            vertical: 28,
          ),
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 560),
            child: _StepCard(
              step: _step,
              totalSteps: _totalSteps,
              busy: _busy,
              error: _error,
              serverNote: _serverNote,
              serverReachable: _serverReachable,
              isRegisterRole: _isRegisterRole,
              serverController: _serverController,
              keyController: _keyController,
              serviceKeyController: _serviceKeyController,
              localAddressController: _localAddressController,
              protocol: _protocol,
              onProtocolChanged: (value) => setState(() => _protocol = value),
              onSaveServer: _saveServer,
              onPickRole: _pickRole,
              onFinishDetails: _finishDetails,
              onBack: () => setState(() {
                _error = null;
                _step = switch (_step) {
                  _Step.details => _Step.role,
                  _Step.role => _Step.server,
                  _ => _Step.server,
                };
              }),
              onSkip: widget.onSkip,
              onDone: () => widget.onFinished(
                _isRegisterRole ? AppSection.register : AppSection.connect,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _StepCard extends StatelessWidget {
  const _StepCard({
    required this.step,
    required this.totalSteps,
    required this.busy,
    required this.error,
    required this.serverNote,
    required this.serverReachable,
    required this.isRegisterRole,
    required this.serverController,
    required this.keyController,
    required this.serviceKeyController,
    required this.localAddressController,
    required this.protocol,
    required this.onProtocolChanged,
    required this.onSaveServer,
    required this.onPickRole,
    required this.onFinishDetails,
    required this.onBack,
    required this.onSkip,
    required this.onDone,
  });

  final _Step step;
  final int totalSteps;
  final bool busy;
  final String? error;
  final String? serverNote;
  final bool? serverReachable;
  final bool isRegisterRole;
  final TextEditingController serverController;
  final TextEditingController keyController;
  final TextEditingController serviceKeyController;
  final TextEditingController localAddressController;
  final String protocol;
  final ValueChanged<String> onProtocolChanged;
  final VoidCallback onSaveServer;
  final void Function({required bool register}) onPickRole;
  final VoidCallback onFinishDetails;
  final VoidCallback onBack;
  final VoidCallback onSkip;
  final VoidCallback onDone;

  int get _stepNumber => switch (step) {
    _Step.server => 1,
    _Step.role => 2,
    _Step.details => 3,
    _Step.done => 3,
  };

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final l10n = context.l10n;

    return Container(
      padding: const EdgeInsets.all(24),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest.withValues(alpha: 0.35),
        borderRadius: BorderRadius.circular(14),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (step != _Step.done)
            Text(
              l10n.setupStepOf(_stepNumber, totalSteps),
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant,
                letterSpacing: 0.4,
              ),
            ),
          const SizedBox(height: 10),
          Text(
            switch (step) {
              _Step.server => l10n.setupServerTitle,
              _Step.role => l10n.setupRoleTitle,
              _Step.details => isRegisterRole
                  ? l10n.setupDetailsTitleRegister
                  : l10n.setupDetailsTitleConnect,
              _Step.done => l10n.setupDoneTitle,
            },
            style: theme.textTheme.titleLarge?.copyWith(
              fontWeight: FontWeight.w700,
            ),
          ),
          const SizedBox(height: 18),
          ..._body(context),
          if (error != null) ...[
            const SizedBox(height: 14),
            Text(
              error!,
              style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
            ),
          ],
          const SizedBox(height: 22),
          _actions(context),
        ],
      ),
    );
  }

  List<Widget> _body(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final l10n = context.l10n;

    switch (step) {
      case _Step.server:
        return [
          Text(
            l10n.setupServerBody,
            style: theme.textTheme.bodyMedium?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
          const SizedBox(height: 16),
          TextField(
            controller: serverController,
            autofocus: true,
            decoration: InputDecoration(
              labelText: l10n.serverAddress,
              hintText: l10n.setupServerHint,
              border: const OutlineInputBorder(),
            ),
            onSubmitted: (_) => busy ? null : onSaveServer(),
          ),
          const SizedBox(height: 16),
          TextField(
            controller: keyController,
            decoration: InputDecoration(
              labelText: l10n.setupKeyLabel,
              helperText: l10n.setupKeyBody,
              helperMaxLines: 2,
              border: const OutlineInputBorder(),
            ),
          ),
        ];

      case _Step.role:
        return [
          if (serverNote != null)
            Padding(
              padding: const EdgeInsets.only(bottom: 16),
              child: Row(
                children: [
                  Icon(
                    serverReachable == true
                        ? Icons.check_circle_outline
                        : Icons.info_outline,
                    size: 16,
                    color: serverReachable == true
                        ? scheme.primary
                        : scheme.onSurfaceVariant,
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      serverNote!,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          _RoleChoice(
            icon: Icons.upload_rounded,
            title: l10n.navRegister,
            summary: l10n.roleRegisterSummary,
            onPressed: () => onPickRole(register: true),
          ),
          const SizedBox(height: 10),
          _RoleChoice(
            icon: Icons.download_rounded,
            title: l10n.navConnect,
            summary: l10n.roleConnectSummary,
            onPressed: () => onPickRole(register: false),
          ),
        ];

      case _Step.details:
        return [
          TextField(
            controller: serviceKeyController,
            autofocus: true,
            decoration: InputDecoration(
              labelText: l10n.serviceKey,
              helperText: l10n.setupServiceKeyBody,
              helperMaxLines: 2,
              border: const OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 16),
          TextField(
            controller: localAddressController,
            decoration: InputDecoration(
              labelText: l10n.localAddress,
              helperText: isRegisterRole
                  ? l10n.setupLocalAddressBodyRegister
                  : l10n.setupLocalAddressBodyConnect,
              helperMaxLines: 2,
              border: const OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 16),
          SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'TCP', label: Text('TCP')),
              ButtonSegment(value: 'UDP', label: Text('UDP')),
            ],
            selected: {protocol},
            onSelectionChanged: (values) => onProtocolChanged(values.first),
          ),
        ];

      case _Step.done:
        return [
          Row(
            children: [
              Icon(Icons.check_circle, size: 18, color: scheme.primary),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  isRegisterRole
                      ? l10n.setupDoneBodyRegister
                      : l10n.setupDoneBodyConnect,
                  style: theme.textTheme.bodyMedium,
                ),
              ),
            ],
          ),
        ];
    }
  }

  Widget _actions(BuildContext context) {
    final l10n = context.l10n;

    if (step == _Step.done) {
      return Align(
        alignment: Alignment.centerRight,
        child: FilledButton(onPressed: onDone, child: Text(l10n.setupFinish)),
      );
    }

    return Row(
      children: [
        if (step == _Step.server)
          TextButton(onPressed: onSkip, child: Text(l10n.setupSkip))
        else
          TextButton(onPressed: busy ? null : onBack, child: Text(l10n.setupBack)),
        const Spacer(),
        if (busy)
          const SizedBox(
            width: 20,
            height: 20,
            child: CircularProgressIndicator(strokeWidth: 2),
          )
        // The role step advances by picking a card, so it has no Next button.
        else if (step != _Step.role)
          FilledButton(
            onPressed: step == _Step.server ? onSaveServer : onFinishDetails,
            child: Text(
              step == _Step.details ? l10n.setupFinish : l10n.setupNext,
            ),
          ),
      ],
    );
  }
}

class _RoleChoice extends StatelessWidget {
  const _RoleChoice({
    required this.icon,
    required this.title,
    required this.summary,
    required this.onPressed,
  });

  final IconData icon;
  final String title;
  final String summary;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Material(
      color: scheme.surfaceContainerHighest.withValues(alpha: 0.4),
      borderRadius: BorderRadius.circular(10),
      child: InkWell(
        onTap: onPressed,
        borderRadius: BorderRadius.circular(10),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 14),
          child: Row(
            children: [
              Icon(icon, size: 20, color: scheme.primary),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      title,
                      style: theme.textTheme.titleSmall?.copyWith(
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Text(
                      summary,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ),
              ),
              Icon(
                Icons.chevron_right_rounded,
                size: 18,
                color: scheme.onSurfaceVariant,
              ),
            ],
          ),
        ),
      ),
    );
  }
}
