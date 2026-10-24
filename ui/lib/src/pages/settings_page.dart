import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/setup_string.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';
import 'package:pb_mapper_ui/src/updates/update_dialog.dart';
import 'package:pb_mapper_ui/src/widgets/brand_mark.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:url_launcher/url_launcher.dart';

const String kRepositoryUrl = 'https://github.com/acking-you/pb-mapper';

class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key});

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    return PageFrame(
      title: l10n.navSettings,
      body: Align(
        alignment: Alignment.topCenter,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 720),
          child: ListView(
            key: const Key('settings-list'),
            padding: const EdgeInsets.fromLTRB(18, 4, 18, 28),
            children: [
              RelayCard(store: scope.store),
              const SizedBox(height: 16),
              _AppearanceCard(prefs: scope.prefs),
              const SizedBox(height: 16),
              _AboutCard(updates: scope.updates, prefs: scope.prefs),
            ],
          ),
        ),
      ),
    );
  }
}

/// Where the relay is and how this computer proves itself to it. Shared by
/// Settings and first-run setup.
class RelayCard extends StatefulWidget {
  const RelayCard({super.key, required this.store});

  final AppStore store;

  @override
  State<RelayCard> createState() => _RelayCardState();
}

class _RelayCardState extends State<RelayCard> {
  final TextEditingController _address = TextEditingController();
  final TextEditingController _credential = TextEditingController();
  bool _keepAlive = true;
  bool _showCredential = false;
  bool _busy = false;
  bool _testing = false;
  String? _addressError;
  String? _credentialError;

  /// The config the fields were last filled from. A change made elsewhere (a
  /// terminal, another window) refills them unless the user is mid-edit.
  ConfigStatus? _seed;
  bool _edited = false;

  AppStore get _store => widget.store;

  @override
  void initState() {
    super.initState();
    _store.addListener(_reseed);
    _reseed();
  }

  @override
  void dispose() {
    _store.removeListener(_reseed);
    _address.dispose();
    _credential.dispose();
    super.dispose();
  }

  void _reseed() {
    final config = _store.config;
    if (identical(config, _seed) || _edited) return;
    _seed = config;
    _address.text = config.serverAddress;
    _credential.text = config.msgHeaderKey;
    if (mounted) {
      setState(() => _keepAlive = config.keepAliveEnabled);
    } else {
      _keepAlive = config.keepAliveEnabled;
    }
  }

  bool _validate() {
    final l10n = context.l10n;
    setState(() {
      _addressError = looksLikeHostPort(_address.text)
          ? null
          : l10n.errorAddress;
      _credentialError = looksLikeCredential(_credential.text)
          ? null
          : l10n.errorCredential;
    });
    return _addressError == null && _credentialError == null;
  }

  Future<void> _save() async {
    if (_busy || !_validate()) return;
    final l10n = context.l10n;
    setState(() => _busy = true);
    final result = await _store.saveRelay(
      address: _address.text,
      credential: _credential.text,
      keepAlive: _keepAlive,
    );
    if (!mounted) return;
    setState(() {
      _busy = false;
      _edited = false;
    });
    _reseed();
    if (!result.success) {
      showToast(context, result.message, kind: ToastKind.error);
    } else if (_store.relayState == RelayState.online) {
      showToast(
        context,
        l10n.relaySaved,
        kind: ToastKind.success,
        description: _answered(),
      );
    } else {
      showToast(
        context,
        l10n.relaySavedOffline,
        kind: ToastKind.warning,
        description: l10n.relayTestFailed(
          _store.relay?.error ?? l10n.relayNoReason,
        ),
      );
    }
  }

  String _answered() => context.l10n.relayTestOk(
    _store.relay?.latencyMs ?? 0,
    _store.relayServices.length,
  );

  /// Ask the relay now, and say what it answered. Unsaved changes are saved
  /// first: testing the old values while new ones sit in the fields would
  /// answer a question nobody asked.
  Future<void> _test() async {
    if (_edited) return _save();
    final l10n = context.l10n;
    setState(() => _testing = true);
    await _store.checkRelay(force: true);
    if (!mounted) return;
    setState(() => _testing = false);
    if (_store.relayState == RelayState.online) {
      showToast(context, _answered(), kind: ToastKind.success);
    } else {
      showToast(
        context,
        l10n.relayTestFailed(_store.relay?.error ?? l10n.relayNoReason),
        kind: ToastKind.error,
      );
    }
  }

  Future<void> _copySetup() async {
    final payload = encodeSetupString(
      SetupPayload(
        address: _address.text,
        credential: _credential.text,
        keepAlive: _keepAlive,
      ),
    );
    await Clipboard.setData(ClipboardData(text: payload));
    if (!mounted) return;
    showToast(context, context.l10n.setupStringCopied, kind: ToastKind.success);
  }

  Future<void> _pasteSetup() async {
    final l10n = context.l10n;
    final raw = await _askForSetupString();
    if (raw == null || !mounted) return;
    try {
      final setup = decodeSetupString(raw);
      final address = setup.address;
      final credential = setup.credential;
      final keepAlive = setup.keepAlive;
      if (!looksLikeHostPort(address)) {
        throw FormatException(l10n.errorAddress);
      }
      if (!looksLikeCredential(credential)) {
        throw FormatException(l10n.errorCredential);
      }
      setState(() {
        _address.text = address;
        _credential.text = credential;
        _keepAlive = keepAlive;
        _edited = true;
      });
      await _save();
    } catch (e) {
      if (!mounted) return;
      final message = e is FormatException ? e.message : '$e';
      showToast(
        context,
        l10n.setupStringInvalid(message),
        kind: ToastKind.error,
      );
    }
  }

  Future<String?> _askForSetupString() => showDialog<String>(
    context: context,
    builder: (_) => const _SetupStringDialog(),
  );

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return GroupCard(
      title: l10n.settingsRelay,
      icon: AppIcons.relay,
      trailing: ListenableBuilder(
        listenable: _store,
        builder: (context, _) => RelayChip(state: _store.relayState),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(
            l10n.settingsRelayBody,
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
              fontSize: 13,
            ),
          ),
          const SizedBox(height: 14),
          TextField(
            key: const Key('settings-address'),
            controller: _address,
            autocorrect: false,
            style: mono(fontSize: 13.5),
            onChanged: (_) => _edited = true,
            onSubmitted: (_) => _save(),
            decoration: InputDecoration(
              labelText: l10n.relayAddress,
              hintText: 'example.com:7666',
              helperText: l10n.relayAddressHelp,
              errorText: _addressError,
              prefixIcon: const Icon(AppIcons.relay, size: 18),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            key: const Key('settings-credential'),
            controller: _credential,
            autocorrect: false,
            enableSuggestions: false,
            obscureText: !_showCredential,
            style: mono(fontSize: 13.5),
            onChanged: (_) => _edited = true,
            onSubmitted: (_) => _save(),
            decoration: InputDecoration(
              labelText: l10n.credential,
              helperText: l10n.credentialHelp,
              errorText: _credentialError,
              prefixIcon: const Icon(AppIcons.key, size: 18),
              suffixIcon: IconButton(
                tooltip: _showCredential ? l10n.hideSecret : l10n.showSecret,
                onPressed: () =>
                    setState(() => _showCredential = !_showCredential),
                icon: Icon(
                  _showCredential ? AppIcons.hide : AppIcons.show,
                  size: 17,
                ),
              ),
            ),
          ),
          const SizedBox(height: 4),
          SwitchListTile(
            key: const Key('settings-keepalive'),
            contentPadding: const EdgeInsets.symmetric(horizontal: 4),
            title: Text(l10n.keepAliveDefault),
            subtitle: Text(l10n.keepAliveHelp),
            value: _keepAlive,
            onChanged: (v) => setState(() {
              _keepAlive = v;
              _edited = true;
            }),
          ),
          const SizedBox(height: 10),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              FilledButton(
                key: const Key('settings-save'),
                onPressed: _busy ? null : _save,
                child: _busy
                    ? const SizedBox.square(
                        dimension: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : Text(l10n.save),
              ),
              OutlinedButton(
                key: const Key('settings-test'),
                onPressed: _testing || !_store.hasRelay ? null : _test,
                child: _testing
                    ? const SizedBox.square(
                        dimension: 16,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : Text(l10n.testRelay),
              ),
              TextButton.icon(
                key: const Key('settings-copy-setup'),
                onPressed: _copySetup,
                icon: const Icon(AppIcons.export, size: 16),
                label: Text(l10n.copySetup),
              ),
              TextButton.icon(
                key: const Key('settings-paste-setup'),
                onPressed: _pasteSetup,
                icon: const Icon(AppIcons.import, size: 16),
                label: Text(l10n.pasteSetup),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Asks for a setup string. Owns its controller, so the field keeps it for
/// as long as the dialog is on screen — including its closing animation,
/// which outlives the future the caller awaits.
class _SetupStringDialog extends StatefulWidget {
  const _SetupStringDialog();

  @override
  State<_SetupStringDialog> createState() => _SetupStringDialogState();
}

class _SetupStringDialogState extends State<_SetupStringDialog> {
  final TextEditingController _text = TextEditingController();

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    return AlertDialog(
      title: Text(l10n.pasteSetup),
      content: SizedBox(
        width: 480,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(l10n.pasteSetupHint),
            const SizedBox(height: 12),
            TextField(
              key: const Key('setup-string'),
              controller: _text,
              autofocus: true,
              maxLines: 4,
              style: mono(fontSize: 12.5),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          key: const Key('setup-string-apply'),
          onPressed: () => Navigator.of(context).pop(_text.text.trim()),
          child: Text(l10n.apply),
        ),
      ],
    );
  }
}

class _AppearanceCard extends StatelessWidget {
  const _AppearanceCard({required this.prefs});

  final AppPrefs prefs;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    return GroupCard(
      title: l10n.settingsAppearance,
      icon: AppIcons.themeSystem,
      child: ListenableBuilder(
        listenable: prefs,
        builder: (context, _) => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _ChoiceRow(
              label: l10n.theme,
              child: GlassSegmented<ThemeMode>(
                key: const Key('settings-theme'),
                expand: false,
                selected: prefs.themeMode,
                onChanged: prefs.setThemeMode,
                segments: [
                  GlassSegment(
                    value: ThemeMode.system,
                    label: l10n.themeSystem,
                    icon: AppIcons.themeSystem,
                  ),
                  GlassSegment(
                    value: ThemeMode.light,
                    label: l10n.themeLight,
                    icon: AppIcons.themeLight,
                  ),
                  GlassSegment(
                    key: const Key('settings-theme-dark'),
                    value: ThemeMode.dark,
                    label: l10n.themeDark,
                    icon: AppIcons.themeDark,
                  ),
                ],
              ),
            ),
            const SizedBox(height: 14),
            _ChoiceRow(
              label: l10n.language,
              child: GlassSegmented<String?>(
                key: const Key('settings-language'),
                expand: false,
                selected: prefs.locale?.languageCode,
                onChanged: (code) =>
                    prefs.setLocale(code == null ? null : Locale(code)),
                segments: [
                  GlassSegment(value: null, label: l10n.languageSystem),
                  const GlassSegment(value: 'en', label: 'English'),
                  const GlassSegment(
                    key: Key('settings-language-zh'),
                    value: 'zh',
                    label: '中文',
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A label with its control at the end, or beneath it when the row is too
/// narrow for both.
class _ChoiceRow extends StatelessWidget {
  const _ChoiceRow({required this.label, required this.child});

  final String label;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final text = Text(label, style: Theme.of(context).textTheme.bodyMedium);
    return LayoutBuilder(
      builder: (context, box) => box.maxWidth >= 480
          ? Row(
              children: [
                Expanded(child: text),
                child,
              ],
            )
          : Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                text,
                const SizedBox(height: 8),
                SingleChildScrollView(
                  scrollDirection: Axis.horizontal,
                  child: child,
                ),
              ],
            ),
    );
  }
}

/// What this is, which version, and the way to the next one.
class _AboutCard extends StatefulWidget {
  const _AboutCard({required this.updates, required this.prefs});

  final UpdateController updates;
  final AppPrefs prefs;

  @override
  State<_AboutCard> createState() => _AboutCardState();
}

class _AboutCardState extends State<_AboutCard> {
  String? _version;

  @override
  void initState() {
    super.initState();
    PackageInfo.fromPlatform()
        .then((info) {
          if (mounted) setState(() => _version = info.version);
        })
        .catchError((_) {});
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );
    final updates = widget.updates;
    final prefs = widget.prefs;
    final star = OutlinedButton.icon(
      onPressed: () => launchUrl(
        Uri.parse(kRepositoryUrl),
        mode: LaunchMode.externalApplication,
      ),
      icon: const Icon(AppIcons.star, size: 16),
      label: Text(l10n.starOnGitHub),
    );
    return GroupCard(
      child: LayoutBuilder(
        builder: (context, box) {
          // On a phone the Star button does not fit beside the name; it
          // joins the buttons below instead.
          final narrow = box.maxWidth < 440;
          return Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const BrandMark(size: 44),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(l10n.appTitle, style: theme.textTheme.titleMedium),
                        const SizedBox(height: 2),
                        Text(l10n.aboutBody, style: muted),
                        if (_version case final version?) ...[
                          const SizedBox(height: 2),
                          Text(
                            l10n.versionLabel(version),
                            key: const Key('settings-version'),
                            style: muted,
                          ),
                        ],
                      ],
                    ),
                  ),
                  if (!narrow) ...[const SizedBox(width: 10), star],
                ],
              ),
              if (updatesSupported) ...[
                const SizedBox(height: 14),
                ListenableBuilder(
                  listenable: Listenable.merge([updates, prefs]),
                  builder: (context, _) {
                    final release = updates.published;
                    final offer = updates.newer && release != null;
                    return Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        Wrap(
                          spacing: 8,
                          runSpacing: 8,
                          children: [
                            if (offer)
                              FilledButton.tonalIcon(
                                key: const Key('settings-update'),
                                onPressed: () =>
                                    showUpdateDialog(context, updates),
                                icon: const Icon(AppIcons.update, size: 16),
                                label: Text(
                                  l10n.updateTo('${release.version}'),
                                ),
                              )
                            else
                              OutlinedButton.icon(
                                key: const Key('settings-check-updates'),
                                onPressed: updates.busy
                                    ? null
                                    : () => showUpdateDialog(context, updates),
                                icon: const Icon(AppIcons.update, size: 16),
                                label: Text(l10n.checkForUpdates),
                              ),
                            if (narrow) star,
                          ],
                        ),
                        const SizedBox(height: 4),
                        SwitchListTile(
                          key: const Key('settings-auto-update'),
                          contentPadding: const EdgeInsets.symmetric(
                            horizontal: 4,
                          ),
                          title: Text(l10n.autoCheckUpdates),
                          subtitle: Text(l10n.autoCheckUpdatesHelp),
                          value: prefs.autoCheckUpdates,
                          onChanged: prefs.setAutoCheckUpdates,
                        ),
                      ],
                    );
                  },
                ),
              ] else if (narrow) ...[
                const SizedBox(height: 14),
                Align(alignment: AlignmentDirectional.centerStart, child: star),
              ],
            ],
          );
        },
      ),
    );
  }
}
