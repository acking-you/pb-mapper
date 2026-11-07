import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/remote/invite_scanner.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_actions.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/app_shell.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';

/// How the add flow opens: the platform's way, the camera first, or the
/// clipboard at once.
enum AddMode { auto, scan, paste }

/// Asks for another computer's invitation, then connects to what it names
/// (§6.6). A dialog on a pointer, or on a touch screen wide enough for the
/// rail; otherwise the sheet, which honours [mode]: wherever the compact
/// layout offers Scan and Paste, they open it.
/// [replacing] is the remembered computer whose host identity changed: its
/// name is kept, and an invitation for its service replaces its own.
Future<void> showAddComputer(
  BuildContext context, {
  AddMode mode = AddMode.auto,
  RemoteComputer? replacing,
}) async {
  final store = AppScope.of(context).store;
  final touch =
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;
  AddComputerController controller() => AddComputerController(
    inspect: store.desktop.inspectInvitation,
    computers: store.computers,
    replacing: replacing,
  );
  final AddComputerResult? result;
  if (!touch || usesRail(context)) {
    result = await showDialog<AddComputerResult>(
      context: context,
      barrierColor: const Color(0x52141416),
      builder: (_) => AddComputerDialog(controller: controller),
    );
  } else {
    result = await showModalBottomSheet<AddComputerResult>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (_) => AddComputerSheet(controller: controller, mode: mode),
    );
  }
  if (result != null && context.mounted) await connectFromAdd(context, result);
}

enum AddState { empty, inspecting, recognised, invalid }

enum InviteRejection { invalid, setupString }

/// The add flow's state: the pasted or scanned invitation, what the FFI
/// recognised in it, and what to do with it. The invitation itself is
/// private: widgets see only [masked].
class AddComputerController extends ChangeNotifier {
  AddComputerController({
    required Future<DesktopInvitationInfo> Function(String) inspect,
    required RemoteComputerStore computers,
    this.replacing,
  }) : _inspect = inspect,
       _computers = computers,
       _remember = computers.support.available;

  final Future<DesktopInvitationInfo> Function(String) _inspect;
  final RemoteComputerStore _computers;

  /// The remembered computer this invitation is meant to replace.
  final RemoteComputer? replacing;

  /// The name the computer is saved and shown under.
  final nameField = TextEditingController();

  String _raw = '';
  AddState _state = AddState.empty;
  DesktopInvitationInfo? _info;
  InviteRejection? _rejection;
  int _serial = 0;
  Timer? _debounce;
  String? _defaultName;
  bool _replaceConfirmed = false, _remember, _relayOnly = false;
  bool _disposed = false;

  AddState get state => _state;
  DesktopInvitationInfo? get info => _info;
  InviteRejection? get rejection => _rejection;

  /// The invitation as it may be shown.
  String get masked => maskInvitation(_raw);

  /// Whether the vault can keep anything, and why not (D20).
  RememberSupport get support => _computers.support;

  /// The vault can keep it at all.
  bool get canRemember => support.available;

  /// Already remembered with this identity: Connect opens it.
  RemoteComputer? get duplicate =>
      _info == null ? null : _computers.sameIdentity(_info!);

  /// Remembered under this service with another identity: replaced only on
  /// purpose. The computer being replaced is not a conflict.
  RemoteComputer? get conflict {
    final info = _info;
    if (info == null) return null;
    final other = _computers.sameServiceOtherIdentity(info);
    return other != null && other.id == replacing?.id ? null : other;
  }

  /// The invitation is for another computer than [replacing].
  bool get otherComputer =>
      replacing != null &&
      _info != null &&
      _info!.service != replacing!.service;

  bool get replaceConfirmed => _replaceConfirmed;
  set replaceConfirmed(bool value) => _set(() => _replaceConfirmed = value);

  bool get remember => _remember && canRemember;
  set remember(bool value) => _set(() => _remember = value);

  bool get relayOnly => _relayOnly;
  set relayOnly(bool value) => _set(() => _relayOnly = value);

  void _set(VoidCallback change) {
    change();
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  /// Typed text: inspected once typing pauses.
  void edit(String text) {
    _debounce?.cancel();
    _raw = text;
    if (text.trim().isEmpty) {
      _reset();
      return;
    }
    _debounce = Timer(
      const Duration(milliseconds: 250),
      () => unawaited(_run(text)),
    );
  }

  /// Pasted or scanned text: inspected at once.
  void paste(String text) {
    _debounce?.cancel();
    _raw = text;
    if (text.trim().isEmpty) {
      _reset();
      return;
    }
    unawaited(_run(text));
  }

  void clear() {
    _debounce?.cancel();
    _raw = '';
    _reset();
  }

  void _reset() {
    _serial++;
    _state = AddState.empty;
    _info = null;
    _rejection = null;
    _notify();
  }

  Future<void> _run(String text) async {
    final serial = ++_serial;
    _state = AddState.inspecting;
    _info = null;
    _rejection = null;
    _notify();
    DesktopInvitationInfo? info;
    InviteRejection? rejection;
    try {
      info = await _inspect(text.trim());
    } on DesktopCallException catch (e) {
      rejection = e.code == 'SETUP_STRING'
          ? InviteRejection.setupString
          : InviteRejection.invalid;
    } catch (_) {
      rejection = InviteRejection.invalid;
    }
    // A later edit or paste supersedes this answer.
    if (_disposed || serial != _serial) return;
    _info = info;
    _rejection = rejection;
    _state = info == null ? AddState.invalid : AddState.recognised;
    _replaceConfirmed = false;
    if (info != null) _nameFor(info);
    _notify();
  }

  /// The saved name of a computer already known, else the service without
  /// its `desktop:` prefix. A name the user typed is kept.
  void _nameFor(DesktopInvitationInfo info) {
    final service = info.service;
    final replacing = this.replacing;
    final name =
        duplicate?.name ??
        (replacing != null && replacing.service == service
            ? replacing.name
            : null) ??
        (service.startsWith('desktop:')
            ? service.substring('desktop:'.length)
            : service);
    final typed = nameField.text.trim();
    if (typed.isEmpty || typed == _defaultName || duplicate != null) {
      nameField.text = name;
    }
    _defaultName = name;
  }

  /// Connect is offered: an invitation is recognised, and any conflicting
  /// saved identity is replaced on purpose.
  bool get ready =>
      _state == AddState.recognised &&
      _info != null &&
      (conflict == null || _replaceConfirmed);

  /// What Connect runs, built as it is pressed: typing a name does not
  /// notify, so a result built earlier would miss it. Null unless [ready].
  AddComputerResult? result() {
    final info = _info;
    if (!ready || info == null) return null;
    final conflict = this.conflict;
    final replacing = this.replacing;
    final typed = nameField.text.trim();
    return AddComputerResult(
      invitation: _raw.trim(),
      info: info,
      name: typed.isEmpty ? (_defaultName ?? info.name) : typed,
      remember: remember,
      relayOnly: _relayOnly,
      replaceId: replacing != null && replacing.service == info.service
          ? replacing.id
          : conflict?.id,
      existing: duplicate,
    );
  }

  @override
  void dispose() {
    _disposed = true;
    _debounce?.cancel();
    nameField.dispose();
    super.dispose();
  }
}

/// Connect's action: the flow's result as it is when pressed, so it carries
/// the name in the field then.
VoidCallback? _connect(BuildContext context, AddComputerController add) =>
    add.ready ? () => Navigator.of(context).pop(add.result()) : null;

Future<String?> _readClipboard() async {
  try {
    return (await Clipboard.getData(Clipboard.kTextPlain))?.text;
  } catch (_) {
    return null;
  }
}

/// The add flow as a dialog (AddComputer): 560 wide, the viewer opens as it
/// closes, so it never waits for the connection.
class AddComputerDialog extends StatefulWidget {
  const AddComputerDialog({super.key, required this.controller});

  /// Makes the flow's controller, which the dialog owns.
  final AddComputerController Function() controller;

  @override
  State<AddComputerDialog> createState() => _AddComputerDialogState();
}

class _AddComputerDialogState extends State<AddComputerDialog> {
  late final AddComputerController _add = widget.controller();

  @override
  void dispose() {
    _add.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return Dialog(
      backgroundColor: raisedCardColor(scheme),
      insetPadding: const EdgeInsets.all(24),
      clipBehavior: Clip.antiAlias,
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxWidth: 560,
          maxHeight: MediaQuery.sizeOf(context).height * 0.9,
        ),
        child: ListenableBuilder(
          listenable: _add,
          builder: (context, _) => SingleChildScrollView(
            padding: const EdgeInsets.fromLTRB(28, 24, 28, 28),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _Header(title: l10n.addTitle, size: 18),
                const SizedBox(height: 20),
                InviteField(controller: _add),
                if (_add.state == AddState.recognised) ...[
                  const SizedBox(height: 20),
                  RecognisedHostCard(controller: _add),
                  const SizedBox(height: 20),
                  _NameField(controller: _add),
                ],
                const SizedBox(height: 20),
                _Options(controller: _add),
                const SizedBox(height: 20),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    TextButton(
                      style: TextButton.styleFrom(
                        minimumSize: const Size(0, 40),
                      ),
                      onPressed: () => Navigator.of(context).pop(),
                      child: Text(l10n.cancel),
                    ),
                    const SizedBox(width: 10),
                    FilledButton(
                      key: const Key('add-connect'),
                      style: FilledButton.styleFrom(
                        minimumSize: const Size(0, 40),
                        textStyle: buttonLabel(
                          context,
                          14,
                          weight: FontWeight.w600,
                        ),
                      ),
                      onPressed: _connect(context, _add),
                      child: Text(l10n.connect),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The add flow as a phone sheet (MobileAdd): the camera first where there
/// is one, Paste beside it, and once an invitation is recognised, what it
/// names and Connect.
class AddComputerSheet extends StatefulWidget {
  const AddComputerSheet({
    super.key,
    required this.controller,
    this.mode = AddMode.auto,
  });

  final AddComputerController Function() controller;
  final AddMode mode;

  @override
  State<AddComputerSheet> createState() => _AddComputerSheetState();
}

class _AddComputerSheetState extends State<AddComputerSheet> {
  late final AddComputerController _add = widget.controller();
  late final bool _scan =
      InviteScanner.supported && widget.mode != AddMode.paste;

  @override
  void initState() {
    super.initState();
    if (widget.mode == AddMode.paste) unawaited(_paste());
  }

  @override
  void dispose() {
    _add.dispose();
    super.dispose();
  }

  Future<void> _paste() async {
    final text = await _readClipboard();
    if (text != null && mounted) _add.paste(text);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return ListenableBuilder(
      listenable: _add,
      builder: (context, _) {
        final recognised = _add.state == AddState.recognised;
        return SingleChildScrollView(
          padding: EdgeInsets.fromLTRB(
            20,
            0,
            20,
            24 + MediaQuery.viewInsetsOf(context).bottom,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _Header(title: l10n.addTitle, size: 20, filledClose: true),
              const SizedBox(height: 16),
              if (recognised) ...[
                RecognisedHostCard(controller: _add),
                const SizedBox(height: 16),
                _NameField(controller: _add),
                const SizedBox(height: 16),
                FilledButton(
                  key: const Key('add-connect'),
                  style: FilledButton.styleFrom(
                    minimumSize: const Size.fromHeight(48),
                  ),
                  onPressed: _connect(context, _add),
                  child: Text(l10n.connect),
                ),
              ] else if (_scan) ...[
                InviteScanner(onDetect: _add.paste),
                const SizedBox(height: 16),
                Row(
                  children: [
                    const Expanded(child: Divider()),
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 12),
                      child: Text(
                        l10n.scanOr,
                        style: TextStyle(
                          fontSize: 12.5,
                          color: onSurfaceMuted(scheme),
                        ),
                      ),
                    ),
                    const Expanded(child: Divider()),
                  ],
                ),
                const SizedBox(height: 16),
                OutlinedButton.icon(
                  key: const Key('add-paste'),
                  style: OutlinedButton.styleFrom(
                    minimumSize: const Size.fromHeight(48),
                    side: BorderSide(color: scheme.onSurface),
                  ),
                  onPressed: () => unawaited(_paste()),
                  icon: const Icon(AppIcons.paste, size: AppIconSize.button),
                  label: Text(l10n.remotePasteInvite),
                ),
                if (_add.state == AddState.inspecting) ...[
                  const SizedBox(height: 12),
                  const Center(
                    child: SizedBox.square(
                      dimension: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    ),
                  ),
                ],
                if (_add.rejection case final rejection?) ...[
                  const SizedBox(height: 8),
                  _RejectionText(rejection),
                ],
              ] else
                InviteField(controller: _add),
              const SizedBox(height: 16),
              _Options(controller: _add),
              if (_scan) ...[
                const SizedBox(height: 12),
                Text(
                  l10n.cameraNote,
                  style: TextStyle(
                    fontSize: 12,
                    height: 1.5,
                    color: quietText(scheme),
                  ),
                ),
              ],
            ],
          ),
        );
      },
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({
    required this.title,
    required this.size,
    this.filledClose = false,
  });

  final String title;
  final double size;

  /// The sheet's close sits in a grey disc.
  final bool filledClose;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Row(
      children: [
        Expanded(
          child: Text(
            title,
            style: TextStyle(fontSize: size, fontWeight: FontWeight.w600),
          ),
        ),
        IconButton(
          key: const Key('add-close'),
          tooltip: context.l10n.close,
          style: IconButton.styleFrom(
            minimumSize: const Size(36, 36),
            backgroundColor: filledClose ? cardFieldFill(scheme) : null,
          ),
          onPressed: () => Navigator.of(context).pop(),
          icon: const Icon(AppIcons.close, size: AppIconSize.button),
        ),
      ],
    );
  }
}

/// The invitation: a field to paste or type it into, then its masked form
/// once recognised.
class InviteField extends StatefulWidget {
  const InviteField({super.key, required this.controller});
  final AddComputerController controller;

  @override
  State<InviteField> createState() => _InviteFieldState();
}

class _InviteFieldState extends State<InviteField> {
  final _text = TextEditingController();

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _paste() async {
    final text = await _readClipboard();
    if (text == null || !mounted) return;
    // A pasted secret is inspected, never written into the field.
    _text.clear();
    widget.controller.paste(text);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final add = widget.controller;
    final recognised = add.state == AddState.recognised;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          l10n.inviteLabel,
          style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
        ),
        const SizedBox(height: 8),
        if (recognised)
          Container(
            padding: const EdgeInsetsDirectional.fromSTEB(14, 6, 6, 6),
            decoration: BoxDecoration(
              color: cardFieldFill(scheme),
              borderRadius: BorderRadius.circular(kFieldRadius),
            ),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    add.masked,
                    key: const Key('add-invite-masked'),
                    maxLines: 1,
                    softWrap: false,
                    overflow: TextOverflow.fade,
                    style: mono(fontSize: 13),
                  ),
                ),
                IconButton(
                  key: const Key('add-invite-clear'),
                  tooltip: MaterialLocalizations.of(context).clearButtonTooltip,
                  onPressed: () {
                    _text.clear();
                    add.clear();
                  },
                  icon: const Icon(AppIcons.close, size: AppIconSize.button),
                ),
              ],
            ),
          )
        else
          Stack(
            children: [
              TextField(
                key: const Key('add-invite'),
                controller: _text,
                minLines: 3,
                maxLines: 3,
                autocorrect: false,
                enableSuggestions: false,
                enableIMEPersonalizedLearning: false,
                smartDashesType: SmartDashesType.disabled,
                smartQuotesType: SmartQuotesType.disabled,
                keyboardType: TextInputType.visiblePassword,
                style: mono(fontSize: 13, height: 1.6),
                decoration: InputDecoration(
                  fillColor: cardFieldFill(scheme),
                  contentPadding: const EdgeInsetsDirectional.fromSTEB(
                    14,
                    12,
                    96,
                    12,
                  ),
                ),
                onChanged: add.edit,
              ),
              PositionedDirectional(
                top: 10,
                end: 10,
                child: OutlinedButton.icon(
                  key: const Key('add-paste'),
                  style: OutlinedButton.styleFrom(
                    minimumSize: const Size(0, 30),
                    padding: const EdgeInsets.symmetric(horizontal: 10),
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(9),
                    ),
                    textStyle: buttonLabel(
                      context,
                      12.5,
                      weight: FontWeight.w400,
                    ),
                    tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                  ),
                  onPressed: () => unawaited(_paste()),
                  icon: const Icon(AppIcons.paste, size: AppIconSize.inline),
                  label: Text(l10n.paste),
                ),
              ),
            ],
          ),
        if (add.state == AddState.inspecting) ...[
          const SizedBox(height: 8),
          const LinearProgressIndicator(minHeight: 2),
        ],
        if (add.rejection case final rejection?) ...[
          const SizedBox(height: 8),
          _RejectionText(rejection),
        ],
      ],
    );
  }
}

class _RejectionText extends StatelessWidget {
  const _RejectionText(this.rejection);
  final InviteRejection rejection;

  @override
  Widget build(BuildContext context) => Text(
    switch (rejection) {
      InviteRejection.setupString => context.l10n.addSetupString,
      InviteRejection.invalid => context.l10n.addInvalid,
    },
    key: const Key('add-error'),
    style: TextStyle(
      fontSize: 12.5,
      color: Theme.of(context).colorScheme.error,
    ),
  );
}

/// The host the invitation names, before anything connects (D3): its
/// service and the short fingerprint the viewer will pin. And what this
/// device already knows of it.
class RecognisedHostCard extends StatelessWidget {
  const RecognisedHostCard({super.key, required this.controller});
  final AddComputerController controller;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final info = controller.info;
    if (info == null) return const SizedBox.shrink();
    final duplicate = controller.duplicate;
    final conflict = controller.conflict;
    final note = TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Container(
          key: const Key('add-recognised'),
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
          decoration: BoxDecoration(
            color: scheme.tertiaryContainer.withValues(alpha: 0.45),
            borderRadius: BorderRadius.circular(kRowRadius),
          ),
          child: Row(
            children: [
              Icon(
                AppIcons.helper,
                size: AppIconSize.nav,
                color: signalColor(scheme),
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(info.service, style: mono(fontSize: 13)),
                    const SizedBox(height: 2),
                    Text(l10n.addRecognized(info.fingerprint), style: note),
                  ],
                ),
              ),
            ],
          ),
        ),
        if (duplicate != null) ...[
          const SizedBox(height: 8),
          Text(l10n.addDuplicate(duplicate.name), style: note),
        ] else if (conflict != null) ...[
          const SizedBox(height: 8),
          Text(
            l10n.addIdentityConflict(conflict.name),
            style: note.copyWith(color: scheme.error),
          ),
          SwitchListTile(
            key: const Key('add-replace'),
            contentPadding: EdgeInsets.zero,
            title: Text(
              l10n.addReplaceConfirm,
              style: const TextStyle(fontSize: 14),
            ),
            value: controller.replaceConfirmed,
            onChanged: (v) => controller.replaceConfirmed = v,
          ),
        ] else if (controller.otherComputer) ...[
          const SizedBox(height: 8),
          Text(l10n.addOtherComputer, style: note),
        ],
      ],
    );
  }
}

class _NameField extends StatelessWidget {
  const _NameField({required this.controller});
  final AddComputerController controller;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          context.l10n.addName,
          style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
        ),
        const SizedBox(height: 8),
        TextField(
          key: const Key('add-name'),
          controller: controller.nameField,
          enabled: controller.duplicate == null,
          inputFormatters: [computerNameLimit],
          decoration: InputDecoration(fillColor: cardFieldFill(scheme)),
        ),
      ],
    );
  }
}

/// Remember this computer, and relay only behind More options.
class _Options extends StatefulWidget {
  const _Options({required this.controller});
  final AddComputerController controller;

  @override
  State<_Options> createState() => _OptionsState();
}

class _OptionsState extends State<_Options> {
  late bool _more = widget.controller.relayOnly;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final add = widget.controller;
    final mobile =
        defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    final hairline = BorderSide(color: scheme.outlineVariant);
    return DecoratedBox(
      decoration: BoxDecoration(border: Border(top: hairline)),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          DecoratedBox(
            decoration: BoxDecoration(border: Border(bottom: hairline)),
            child: SwitchListTile(
              key: const Key('add-remember'),
              contentPadding: EdgeInsets.zero,
              title: Text(l10n.addRemember),
              subtitle: Text(
                !add.canRemember
                    ? cannotRememberText(context, add.support)
                    : mobile
                    ? l10n.addRememberHelpMobile
                    : l10n.addRememberHelp,
                style: TextStyle(fontSize: 12.5, color: quietText(scheme)),
              ),
              value: add.remember,
              onChanged: add.canRemember && add.duplicate == null
                  ? (v) => add.remember = v
                  : null,
            ),
          ),
          InkWell(
            key: const Key('add-more'),
            borderRadius: BorderRadius.circular(kControlRadius),
            onTap: () => setState(() => _more = !_more),
            child: SizedBox(
              height: 44,
              child: Row(
                children: [
                  Icon(
                    _more ? AppIcons.chevronDown : AppIcons.chevronRight,
                    size: AppIconSize.inline,
                    color: scheme.onSurfaceVariant,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      l10n.addMoreOptions,
                      style: TextStyle(
                        fontSize: 13.5,
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
          if (_more)
            SwitchListTile(
              key: const Key('add-relay-only'),
              contentPadding: EdgeInsets.zero,
              title: Text(l10n.remoteRelayOnly),
              value: add.relayOnly,
              onChanged: (v) => add.relayOnly = v,
            ),
        ],
      ),
    );
  }
}
