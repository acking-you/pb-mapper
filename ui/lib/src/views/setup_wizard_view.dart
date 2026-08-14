import 'dart:async' show unawaited;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';

/// Why the wizard is open.
enum WizardMode {
  /// Nothing is configured: ask everything, and set up the first service.
  firstRun,

  /// Someone chose to change where the server is. That is the whole job, so it
  /// stops after the server step instead of asking for another service.
  serverOnly,
}

/// A guided form, one question per screen.
///
/// On a first run it does not stop at the server address: someone new does not
/// yet know what a service key is for, so it carries them through registering
/// or connecting their first service and leaves them with something that works.
/// Reopened to change the server, it asks only that.
class SetupWizardView extends StatefulWidget {
  const SetupWizardView({
    super.key,
    required this.onFinished,
    required this.onSkip,
    this.mode = WizardMode.firstRun,
    this.api,
  });

  /// Called with the zone to open once setup succeeds.
  final ValueChanged<AppSection> onFinished;
  final VoidCallback onSkip;
  final WizardMode mode;

  /// Defaults to the real FFI-backed client; tests pass a fake.
  final PbMapperApiClient? api;

  @override
  State<SetupWizardView> createState() => _SetupWizardViewState();
}

enum _Step {
  server,
  role,
  details,

  /// Polling until the thing we just created is actually up.
  verify,

  /// The hub: what is working, and the choice to add more or stop.
  hub,
}

/// What a tunnel we just created is doing.
enum _Health { waiting, running, retrying, failed, stopped }

/// One thing the wizard set up, and how it is doing.
class _Outcome {
  _Outcome({
    required this.serviceKey,
    required this.isRegister,
    required this.health,
  });

  final String serviceKey;
  final bool isRegister;
  _Health health;
  String message = '';
}

class _SetupWizardViewState extends State<SetupWizardView> {
  late final PbMapperApiClient _api = widget.api ?? PbMapperApi();

  final _serverController = TextEditingController();
  final _keyController = TextEditingController();
  final _serviceKeyController = TextEditingController();
  final _localAddressController = TextEditingController();
  final _serviceKeyFocusNode = FocusNode();

  _Step _step = _Step.server;
  bool _isRegisterRole = true;
  String _protocol = 'TCP';
  bool _busy = false;
  String? _error;
  String? _serverNote;
  bool? _serverReachable;
  List<String> _availableServices = const [];

  /// Everything set up in this session, newest last.
  final List<_Outcome> _outcomes = [];

  /// The one being watched on the verify step.
  _Outcome? _current;

  /// Both modes can end up asking all three questions, since the hub lets a
  /// server-only visit carry on into setting up a service.
  int get _totalSteps => 3;

  @override
  void initState() {
    super.initState();
    _loadExisting();
  }

  /// Prefill from the saved config. Reopening the wizard to change the server
  /// must not present empty fields, or confirming would wipe a working setup.
  Future<void> _loadExisting() async {
    try {
      final config = await _api.fetchConfig();
      if (!mounted) return;
      setState(() {
        if (SetupState.isServerConfigured(config.serverAddress)) {
          _serverController.text = config.serverAddress;
        }
        _keyController.text = config.msgHeaderKey;
      });
    } catch (_) {
      // An empty form is the right fallback for a first run.
    }
  }

  @override
  void dispose() {
    _serverController.dispose();
    _keyController.dispose();
    _serviceKeyController.dispose();
    _localAddressController.dispose();
    _serviceKeyFocusNode.dispose();
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
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _serviceKeyFocusNode.requestFocus();
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
      _availableServices = const [];
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
      _availableServices = status.serverAvailable
          ? List.unmodifiable(status.registeredServices)
          : const [];
      _serverNote = status.serverAvailable
          ? l10n.setupServerOk
          : l10n.setupServerFailed;
      // Changing the server address is the whole task in serverOnly mode.
      _step = widget.mode == WizardMode.serverOnly ? _Step.hub : _Step.role;
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

    // Submitting is not the same as working. Watch the real status so the
    // wizard only claims success once the tunnel is actually up.
    final outcome = _Outcome(
      serviceKey: serviceKey,
      isRegister: _isRegisterRole,
      health: _Health.waiting,
    );
    setState(() {
      _busy = false;
      if (_isRegisterRole) {
        _availableServices = List.unmodifiable({
          ..._availableServices,
          serviceKey,
        });
      }
      _outcomes.add(outcome);
      _current = outcome;
      _step = _Step.verify;
    });
    unawaited(_watch(outcome));
  }

  /// Polls until the tunnel reports a settled state, then stops.
  ///
  /// Retrying is settled too: a server that is not up yet will never turn
  /// running, and making someone watch a spinner for that is worse than saying
  /// so plainly.
  Future<void> _watch(_Outcome outcome) async {
    for (var attempt = 0; attempt < 20; attempt++) {
      await Future<void>.delayed(const Duration(milliseconds: 900));
      if (!mounted) return;

      final health = await _healthOf(outcome);
      if (!mounted) return;
      setState(() {
        outcome.health = health.$1;
        outcome.message = health.$2;
      });
      if (health.$1 == _Health.running ||
          health.$1 == _Health.failed ||
          health.$1 == _Health.retrying) {
        return;
      }
    }
  }

  Future<(_Health, String)> _healthOf(_Outcome outcome) async {
    try {
      if (outcome.isRegister) {
        final services = await _api.getServiceConfigs();
        final match = services
            .where((s) => s.serviceKey == outcome.serviceKey)
            .firstOrNull;
        if (match == null) return (_Health.waiting, '');
        return (_healthFrom(match.status), match.statusMessage);
      }
      final clients = await _api.getClientConfigs();
      final match = clients
          .where((c) => c.serviceKey == outcome.serviceKey)
          .firstOrNull;
      if (match == null) return (_Health.waiting, '');
      return (_healthFrom(match.status), match.statusMessage);
    } catch (_) {
      return (_Health.waiting, '');
    }
  }

  static _Health _healthFrom(String status) => switch (status.toLowerCase()) {
    'running' => _Health.running,
    'retrying' => _Health.retrying,
    'failed' => _Health.failed,
    _ => _Health.stopped,
  };

  /// Back to the role question, keeping the server and everything already set
  /// up, so adding a second tunnel does not mean starting over.
  /// Where to leave the user.
  ///
  /// The workspace for what they just set up, so they can see it in the page
  /// that manages it. The last one wins when several were added, since that is
  /// the one still fresh in mind. Only a visit that set nothing up goes home.
  AppSection _landingSection() {
    if (_outcomes.isEmpty) return AppSection.home;
    return _outcomes.last.isRegister ? AppSection.register : AppSection.connect;
  }

  void _addAnother({required bool register}) {
    setState(() {
      _error = null;
      _serviceKeyController.clear();
      _current = null;
    });
    // The hub already asked which role, so go straight to its details.
    _pickRole(register: register);
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
              mode: widget.mode,
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
              serviceKeyFocusNode: _serviceKeyFocusNode,
              availableServices: _availableServices,
              localAddressController: _localAddressController,
              protocol: _protocol,
              onProtocolChanged: (value) => setState(() => _protocol = value),
              onSaveServer: _saveServer,
              onPickRole: _pickRole,
              onFinishDetails: _finishDetails,
              onBack: () => setState(() {
                _error = null;
                // Going back from a failed attempt drops it, so the hub does
                // not list a tunnel the user chose to abandon and redo.
                if (_step == _Step.verify && _current != null) {
                  _outcomes.remove(_current);
                  _current = null;
                }
                _step = switch (_step) {
                  _Step.details => _Step.role,
                  _Step.role => _Step.server,
                  // Retrying a failed tunnel means editing what was entered.
                  _Step.verify => _Step.details,
                  _ => _Step.server,
                };
              }),
              outcomes: _outcomes,
              current: _current,
              onAddAnother: _addAnother,
              onContinue: () => setState(() => _step = _Step.hub),
              onSkip: widget.onSkip,
              onDone: () => widget.onFinished(_landingSection()),
            ),
          ),
        ),
      ),
    );
  }
}

class _StepCard extends StatelessWidget {
  const _StepCard({
    required this.mode,
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
    required this.serviceKeyFocusNode,
    required this.availableServices,
    required this.localAddressController,
    required this.protocol,
    required this.onProtocolChanged,
    required this.onSaveServer,
    required this.onPickRole,
    required this.onFinishDetails,
    required this.onBack,
    required this.onSkip,
    required this.onDone,
    required this.onAddAnother,
    required this.onContinue,
    required this.outcomes,
    required this.current,
  });

  final WizardMode mode;
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
  final FocusNode serviceKeyFocusNode;
  final List<String> availableServices;
  final TextEditingController localAddressController;
  final String protocol;
  final ValueChanged<String> onProtocolChanged;
  final VoidCallback onSaveServer;
  final void Function({required bool register}) onPickRole;
  final VoidCallback onFinishDetails;
  final VoidCallback onBack;
  final VoidCallback onSkip;
  final VoidCallback onDone;
  final void Function({required bool register}) onAddAnother;
  final VoidCallback onContinue;
  final List<_Outcome> outcomes;
  final _Outcome? current;

  int get _stepNumber => switch (step) {
    _Step.server => 1,
    _Step.role => 2,
    _Step.details => 3,
    _Step.verify => 3,
    _Step.hub => 3,
  };

  /// The counter is for the questions. Verifying and the hub are not questions,
  /// so they do not carry one.
  bool get _showsCounter => step != _Step.verify && step != _Step.hub;

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
          if (_showsCounter)
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
              _Step.details =>
                isRegisterRole
                    ? l10n.setupDetailsTitleRegister
                    : l10n.setupDetailsTitleConnect,
              _Step.verify => l10n.setupVerifyTitle,
              _Step.hub => l10n.setupDoneTitle,
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
          SetupServiceKeyField(
            controller: serviceKeyController,
            focusNode: serviceKeyFocusNode,
            availableServices: isRegisterRole ? const [] : availableServices,
            labelText: l10n.serviceKey,
            helperText: isRegisterRole || availableServices.isEmpty
                ? l10n.setupServiceKeyBody
                : l10n.setupServiceKeyBodyConnect,
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

      case _Step.verify:
        final outcome = current;
        if (outcome == null) return const [];
        return [_HealthRow(outcome: outcome)];

      case _Step.hub:
        return [
          // Saving the server is worth confirming, but it is not the end of the
          // job: the hub still offers to set up a service either way.
          if (mode == WizardMode.serverOnly && outcomes.isEmpty) ...[
            Row(
              children: [
                Icon(Icons.check_circle, size: 18, color: scheme.primary),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    l10n.setupDoneBodyServer,
                    style: theme.textTheme.bodyMedium,
                  ),
                ),
              ],
            ),
            const SizedBox(height: 14),
          ] else ...[
            Text(
              outcomes.isEmpty ? l10n.setupNothingYet : l10n.setupWorking,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(height: 10),
            for (final outcome in outcomes)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: _HealthRow(outcome: outcome),
              ),
            const SizedBox(height: 6),
          ],
          // The choice the user came back for: set up the other end too, or
          // another tunnel entirely.
          _RoleChoice(
            icon: Icons.upload_rounded,
            title: l10n.setupAddRegister,
            summary: l10n.roleRegisterSummary,
            onPressed: () => onAddAnother(register: true),
          ),
          const SizedBox(height: 8),
          _RoleChoice(
            icon: Icons.download_rounded,
            title: l10n.setupAddConnect,
            summary: l10n.roleConnectSummary,
            onPressed: () => onAddAnother(register: false),
          ),
        ];
    }
  }

  Widget _actions(BuildContext context) {
    final l10n = context.l10n;

    if (step == _Step.hub) {
      return Align(
        alignment: Alignment.centerRight,
        child: FilledButton(onPressed: onDone, child: Text(l10n.setupDoneAll)),
      );
    }

    if (step == _Step.verify) {
      final settled = current?.health != _Health.waiting;
      return Row(
        children: [
          if (current?.health == _Health.failed)
            TextButton(onPressed: onBack, child: Text(l10n.setupRetry)),
          const Spacer(),
          // Only offer the way forward once the status has settled, so nobody
          // walks away from a tunnel that turned out to be broken.
          if (!settled)
            const SizedBox(
              width: 20,
              height: 20,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          else
            FilledButton(onPressed: onContinue, child: Text(l10n.setupNext)),
        ],
      );
    }

    return Row(
      children: [
        if (step == _Step.server)
          TextButton(onPressed: onSkip, child: Text(l10n.setupSkip))
        else
          TextButton(
            onPressed: busy ? null : onBack,
            child: Text(l10n.setupBack),
          ),
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

/// One tunnel and what it is doing, in the wizard's own words.
class _HealthRow extends StatelessWidget {
  const _HealthRow({required this.outcome});

  final _Outcome outcome;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final l10n = context.l10n;

    final (icon, color, text) = switch (outcome.health) {
      _Health.running => (
        Icons.check_circle,
        scheme.primary,
        l10n.setupVerifyRunning(outcome.serviceKey),
      ),
      _Health.retrying => (
        Icons.autorenew,
        scheme.onSurfaceVariant,
        l10n.setupVerifyRetrying(outcome.serviceKey),
      ),
      _Health.failed => (
        Icons.error_outline,
        scheme.error,
        l10n.setupVerifyFailed(outcome.serviceKey, outcome.message),
      ),
      _Health.stopped => (
        Icons.pause_circle_outline,
        scheme.onSurfaceVariant,
        l10n.setupVerifyStopped(outcome.serviceKey),
      ),
      _Health.waiting => (
        Icons.more_horiz,
        scheme.onSurfaceVariant,
        l10n.setupVerifyWaiting,
      ),
    };

    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(icon, size: 16, color: color),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            text,
            style: theme.textTheme.bodySmall?.copyWith(
              color: outcome.health == _Health.failed
                  ? scheme.error
                  : scheme.onSurface,
            ),
          ),
        ),
      ],
    );
  }
}

/// An editable service-key field that offers registered keys when available.
class SetupServiceKeyField extends StatelessWidget {
  const SetupServiceKeyField({
    super.key,
    required this.controller,
    required this.availableServices,
    required this.labelText,
    required this.helperText,
    this.focusNode,
  });

  final TextEditingController controller;
  final FocusNode? focusNode;
  final List<String> availableServices;
  final String labelText;
  final String helperText;

  @override
  Widget build(BuildContext context) {
    final options =
        availableServices
            .map((key) => key.trim())
            .where((key) => key.isNotEmpty)
            .toSet()
            .toList()
          ..sort();

    final optionsKey = options.map((key) => '${key.length}:$key').join();

    return DropdownMenu<String>(
      key: ValueKey('setup-service-key-input:$optionsKey'),
      controller: controller,
      focusNode: focusNode,
      requestFocusOnTap: true,
      enableFilter: true,
      enableSearch: true,
      expandedInsets: EdgeInsets.zero,
      menuHeight: 240,
      label: Text(labelText),
      helperText: helperText,
      dropdownMenuEntries: [
        for (final serviceKey in options)
          DropdownMenuEntry(value: serviceKey, label: serviceKey),
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
