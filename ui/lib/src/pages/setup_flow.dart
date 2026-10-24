import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_editor.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/widgets/brand_mark.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

enum _Step { relay, choose, tunnel }

/// First run: where the relay is, then what to do first, then that one thing
/// — and the app opens on the galaxy with it already there. Every step can be
/// left for later; nothing here is a gate.
class SetupFlow extends StatefulWidget {
  const SetupFlow({super.key, required this.store, required this.onDone});

  final AppStore store;

  /// Called when setup ends, whether or not anything was set up.
  final VoidCallback onDone;

  @override
  State<SetupFlow> createState() => _SetupFlowState();
}

class _SetupFlowState extends State<SetupFlow> {
  _Step _step = _Step.relay;
  TunnelKind _kind = TunnelKind.share;
  final TextEditingController _address = TextEditingController();
  final TextEditingController _credential = TextEditingController();
  bool _busy = false;
  String? _addressError;
  String? _credentialError;

  @override
  void initState() {
    super.initState();
    final config = widget.store.config;
    // The Rust side writes this when it has no config file; it is a default,
    // not an answer the user gave.
    if (config.serverAddress.trim() != 'localhost:7666') {
      _address.text = config.serverAddress;
    }
    _credential.text = config.msgHeaderKey;
  }

  @override
  void dispose() {
    _address.dispose();
    _credential.dispose();
    super.dispose();
  }

  Future<void> _saveRelay() async {
    final l10n = context.l10n;
    final address = _address.text.trim();
    final credential = _credential.text.trim();
    setState(() {
      _addressError = looksLikeHostPort(address) ? null : l10n.errorAddress;
      _credentialError = looksLikeCredential(credential)
          ? null
          : l10n.errorCredential;
    });
    if (_addressError != null || _credentialError != null) return;
    setState(() => _busy = true);
    final result = await widget.store.saveRelay(
      address: address,
      credential: credential,
      keepAlive: true,
    );
    if (!mounted) return;
    setState(() => _busy = false);
    if (!result.success) {
      setState(() => _addressError = result.message);
      return;
    }
    setState(() => _step = _Step.choose);
  }

  void _choose(TunnelKind kind) => setState(() {
    _kind = kind;
    _step = _Step.tunnel;
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      backgroundColor: backdropBase(scheme),
      body: AppBackdrop(
        child: Stack(
          children: [
            SafeArea(
              child: Center(
                child: SingleChildScrollView(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 18,
                    vertical: 36,
                  ),
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 500),
                    child: _card(context),
                  ),
                ),
              ),
            ),
            // The page scrolls, so only a strip along the top drags the window.
            if (isFramelessDesktop)
              const Positioned(
                top: 0,
                left: 0,
                right: 0,
                height: 44,
                child: WindowDragArea(),
              ),
            if (drawsCaptionButtons)
              const Positioned(
                top: 6,
                right: 6,
                child: CaptionButtons(height: 34),
              ),
          ],
        ),
      ),
    );
  }

  Widget _card(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final index = _Step.values.indexOf(_step);
    final shape = RoundedRectangleBorder(
      borderRadius: BorderRadius.circular(kDialogRadius),
      side: BorderSide(color: raisedCardEdge(scheme)),
    );
    // A Material, not a coloured box, so the rows inside show their ink.
    return DecoratedBox(
      decoration: ShapeDecoration(shape: shape, shadows: sheetShadows(scheme)),
      child: Material(
        color: raisedCardColor(scheme),
        shape: shape,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(28, 26, 28, 24),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              _StepDots(current: index, total: _Step.values.length),
              const SizedBox(height: 18),
              AnimatedSwitcher(
                duration: Motion.of(context, Motion.medium),
                switchInCurve: Motion.emphasized,
                transitionBuilder: (child, animation) => FadeTransition(
                  opacity: animation,
                  child: SizeTransition(
                    sizeFactor: animation,
                    alignment: Alignment.topCenter,
                    child: child,
                  ),
                ),
                child: KeyedSubtree(
                  key: ValueKey(_step),
                  child: switch (_step) {
                    _Step.relay => _relayStep(context),
                    _Step.choose => _chooseStep(context),
                    _Step.tunnel => _tunnelStep(context),
                  },
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _relayStep(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        const Center(child: BrandMark(size: 64)),
        const SizedBox(height: 18),
        Text(
          l10n.setupWelcome,
          key: const Key('setup-title'),
          textAlign: TextAlign.center,
          style: theme.textTheme.headlineSmall,
        ),
        const SizedBox(height: 8),
        Text(
          l10n.setupWelcomeBody,
          textAlign: TextAlign.center,
          style: theme.textTheme.bodyMedium?.copyWith(
            color: theme.colorScheme.onSurfaceVariant,
          ),
        ),
        const SizedBox(height: 22),
        TextField(
          key: const Key('setup-address'),
          controller: _address,
          autofocus: true,
          autocorrect: false,
          style: mono(fontSize: 13.5),
          onSubmitted: (_) => _saveRelay(),
          decoration: InputDecoration(
            labelText: l10n.relayAddress,
            hintText: 'example.com:7666',
            errorText: _addressError,
            prefixIcon: const Icon(AppIcons.relay, size: 18),
          ),
        ),
        const SizedBox(height: 12),
        TextField(
          key: const Key('setup-credential'),
          controller: _credential,
          autocorrect: false,
          enableSuggestions: false,
          obscureText: true,
          style: mono(fontSize: 13.5),
          onSubmitted: (_) => _saveRelay(),
          decoration: InputDecoration(
            labelText: l10n.credential,
            helperText: l10n.credentialHelp,
            errorText: _credentialError,
            prefixIcon: const Icon(AppIcons.key, size: 18),
          ),
        ),
        const SizedBox(height: 22),
        FilledButton(
          key: const Key('setup-continue'),
          onPressed: _busy ? null : _saveRelay,
          style: FilledButton.styleFrom(minimumSize: const Size.fromHeight(46)),
          child: _busy
              ? const SizedBox.square(
                  dimension: 18,
                  child: CircularProgressIndicator(strokeWidth: 2),
                )
              : Text(l10n.setupContinue),
        ),
        const SizedBox(height: 6),
        TextButton(
          key: const Key('setup-skip'),
          onPressed: _busy ? null : widget.onDone,
          child: Text(l10n.setupSkip),
        ),
      ],
    );
  }

  Widget _chooseStep(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final online = widget.store.relayState == RelayState.online;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        Row(
          children: [
            RelayChip(state: widget.store.relayState),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                online ? l10n.setupRelayOnline : l10n.setupRelayOffline,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 22),
        Text(l10n.setupChooseTitle, style: theme.textTheme.titleLarge),
        const SizedBox(height: 14),
        _Choice(
          key: const Key('setup-choose-share'),
          icon: AppIcons.share,
          title: l10n.kindShareLong,
          body: l10n.kindShareHint,
          onTap: () => _choose(TunnelKind.share),
        ),
        const SizedBox(height: 10),
        _Choice(
          key: const Key('setup-choose-connect'),
          icon: AppIcons.connect,
          title: l10n.kindConnectLong,
          body: l10n.kindConnectHint,
          onTap: () => _choose(TunnelKind.connect),
        ),
        const SizedBox(height: 12),
        Row(
          children: [
            TextButton.icon(
              onPressed: () => setState(() => _step = _Step.relay),
              icon: const Icon(AppIcons.back, size: 16),
              label: Text(l10n.setupBack),
            ),
            const Spacer(),
            TextButton(
              key: const Key('setup-later'),
              onPressed: widget.onDone,
              child: Text(l10n.setupLater),
            ),
          ],
        ),
      ],
    );
  }

  Widget _tunnelStep(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final share = _kind == TunnelKind.share;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          share ? l10n.kindShareLong : l10n.kindConnectLong,
          style: theme.textTheme.titleLarge,
        ),
        const SizedBox(height: 10),
        TunnelForm(
          store: widget.store,
          kind: _kind,
          kindLocked: true,
          submitLabel: l10n.create,
          cancelLabel: l10n.setupBack,
          onCancel: () => setState(() => _step = _Step.choose),
          onDone: (draft) {
            followTunnel(context, widget.store, draft.kind, draft.key);
            widget.onDone();
          },
        ),
      ],
    );
  }
}

class _StepDots extends StatelessWidget {
  const _StepDots({required this.current, required this.total});

  final int current;
  final int total;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Semantics(
      label: context.l10n.setupStep(current + 1, total),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          for (var i = 0; i < total; i++)
            AnimatedContainer(
              duration: Motion.of(context, Motion.medium),
              curve: Motion.spring,
              margin: const EdgeInsets.symmetric(horizontal: 3),
              width: i == current ? 22 : 7,
              height: 7,
              decoration: BoxDecoration(
                color: i <= current
                    ? scheme.onSurface.withValues(alpha: 0.8)
                    : scheme.onSurface.withValues(alpha: 0.15),
                borderRadius: BorderRadius.circular(999),
              ),
            ),
        ],
      ),
    );
  }
}

/// One of the two things a new user might want, as a card to click.
class _Choice extends StatelessWidget {
  const _Choice({
    super.key,
    required this.icon,
    required this.title,
    required this.body,
    required this.onTap,
  });

  final IconData icon;
  final String title;
  final String body;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Glass(
      depth: GlassDepth.chrome,
      borderRadius: BorderRadius.circular(kRowRadius + 2),
      child: InkWell(
        mouseCursor: clickable,
        borderRadius: BorderRadius.circular(kRowRadius + 2),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 14, 12, 14),
          child: Row(
            children: [
              Container(
                width: 40,
                height: 40,
                decoration: BoxDecoration(
                  color: scheme.tertiaryContainer,
                  borderRadius: BorderRadius.circular(12),
                ),
                child: Icon(icon, size: 20, color: scheme.onTertiaryContainer),
              ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: theme.textTheme.titleSmall),
                    const SizedBox(height: 2),
                    Text(
                      body,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ),
              ),
              Icon(
                AppIcons.chevronRight,
                size: 18,
                color: onSurfaceMuted(scheme),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
