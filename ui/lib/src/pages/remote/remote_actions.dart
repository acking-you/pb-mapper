import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// The Remote page's actions that need a `BuildContext`: confirmations and
/// toasts. These are the only places that combine one with [RemoteDesktop]
/// (U26).

/// Opens a remembered computer, confirming a switch away from another open
/// session.
Future<void> openComputer(BuildContext context, RemoteComputer c) async {
  final remote = AppScope.of(context).remote;
  if (remote.switchesFrom(c) && !await _confirmSwitch(context, remote)) {
    return;
  }
  await remote.open(c);
}

/// Runs an [AddComputerResult], confirming a switch first when needed.
Future<void> connectFromAdd(BuildContext context, AddComputerResult r) async {
  final remote = AppScope.of(context).remote;
  final existing = r.existing;
  final switches = existing != null
      ? remote.switchesFrom(existing)
      : remote.hasOpenSession;
  if (switches && !await _confirmSwitch(context, remote)) return;
  if (!context.mounted) return;
  final failed = _rememberFailed(context);
  await remote.connectNew(r, onRememberFailed: failed);
}

/// Remembers the open session's computer after all: the transient row's
/// menu.
Future<void> rememberOpenComputer(BuildContext context) async {
  final failed = _rememberFailed(context);
  final failure = await AppScope.of(context).remote.rememberCurrent();
  if (failure != null) failed(failure);
}

/// Says why a computer was not remembered. The strings are read now: by the
/// time a session shows its first picture, the page that asked is gone.
ValueChanged<RememberFailure> _rememberFailed(BuildContext context) {
  final l10n = context.l10n;
  return (failure) => showToast(null, switch (failure) {
    RememberFailure.full => l10n.remoteRememberFull,
    RememberFailure.unavailable => l10n.addRememberUnavailable,
  }, kind: ToastKind.warning);
}

Future<bool> _confirmSwitch(BuildContext context, RemoteDesktop remote) {
  final l10n = context.l10n;
  return confirmDestructive(
    context,
    title: l10n.remoteSwitchTitle(remote.title),
    body: l10n.remoteSwitchBody,
    action: l10n.remoteSwitchAction,
  );
}

/// Asks for a new name for [c] and saves it.
Future<void> renameComputer(BuildContext context, RemoteComputer c) async {
  final computers = AppScope.of(context).store.computers;
  final name = await showDialog<String>(
    context: context,
    builder: (_) => _RenameDialog(initial: c.name),
  );
  if (name == null || name == c.name) return;
  await computers.rename(c.id, name);
}

/// Forgets [c], with its saved invitation and last picture, after asking.
Future<void> removeComputer(BuildContext context, RemoteComputer c) async {
  final computers = AppScope.of(context).store.computers;
  final l10n = context.l10n;
  final yes = await confirmDestructive(
    context,
    title: l10n.remoteRemoveConfirmTitle(c.name),
    body: l10n.remoteRemoveConfirmBody,
    action: l10n.remoteRemoveAction,
  );
  if (yes) await computers.forget(c.id);
}

/// The longest name the vault keeps.
const kComputerNameMax = 128;

/// The saved computers are there but cannot be opened: the list offers to
/// clear them.
bool vaultUnreadable(RememberSupport support) =>
    support is RememberUnavailable && support.reason == 'unreadable';

/// Why nothing can be remembered, in the page's words.
String cannotRememberText(BuildContext context, RememberSupport support) =>
    vaultUnreadable(support)
    ? context.l10n.remoteVaultUnreadable
    : context.l10n.addRememberUnavailable;

/// Clears saved computers that cannot be opened, with their last pictures,
/// after asking.
Future<void> resetComputers(BuildContext context) async {
  final computers = AppScope.of(context).store.computers;
  final l10n = context.l10n;
  final yes = await confirmDestructive(
    context,
    title: l10n.remoteVaultResetTitle,
    body: l10n.remoteVaultResetBody,
    action: l10n.remoteVaultResetAction,
  );
  if (yes) await computers.reset();
}

class _RenameDialog extends StatefulWidget {
  const _RenameDialog({required this.initial});
  final String initial;

  @override
  State<_RenameDialog> createState() => _RenameDialogState();
}

class _RenameDialogState extends State<_RenameDialog> {
  late final _name = TextEditingController(text: widget.initial)
    ..selection = TextSelection(
      baseOffset: 0,
      extentOffset: widget.initial.length,
    );

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  void _save() {
    final name = _name.text.trim();
    if (name.isNotEmpty) Navigator.of(context).pop(name);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    return AlertDialog(
      title: Text(l10n.remoteRename),
      content: TextField(
        key: const Key('computer-rename-field'),
        controller: _name,
        autofocus: true,
        inputFormatters: [LengthLimitingTextInputFormatter(kComputerNameMax)],
        decoration: InputDecoration(labelText: l10n.addName),
        onSubmitted: (_) => _save(),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
        ListenableBuilder(
          listenable: _name,
          builder: (context, _) => FilledButton(
            key: const Key('computer-rename-save'),
            onPressed: _name.text.trim().isEmpty ? null : _save,
            child: Text(l10n.save),
          ),
        ),
      ],
    );
  }
}
