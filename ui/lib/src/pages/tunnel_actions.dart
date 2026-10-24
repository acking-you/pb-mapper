import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_editor.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// The things a tunnel row and a planet's panel can do, written once so the
/// two never drift: a list row whose Stop restarted the tunnel is what came
/// of keeping the rule in three places.

/// Stop a live tunnel, or start a stopped one. A failing tunnel is still live
/// — its retry loop runs — so it stops too.
Future<void> toggleTunnel(
  BuildContext context,
  AppStore store,
  Tunnel tunnel,
) async {
  final l10n = context.l10n;
  if (tunnel.isLive) {
    final result = await store.stop(tunnel);
    if (!context.mounted) return;
    showToast(
      context,
      result.success ? l10n.toastStopped(tunnel.key) : result.message,
      kind: result.success ? ToastKind.info : ToastKind.error,
    );
    return;
  }
  final result = await store.resume(tunnel);
  if (!context.mounted) return;
  if (!result.success) {
    showToast(context, result.message, kind: ToastKind.error);
    return;
  }
  await followTunnel(context, store, tunnel.kind, tunnel.key);
}

/// Ask, then stop [tunnel] and forget it.
Future<void> deleteTunnel(
  BuildContext context,
  AppStore store,
  Tunnel tunnel,
) async {
  final l10n = context.l10n;
  final yes = await confirmDestructive(
    context,
    title: l10n.deleteTitle,
    body: l10n.deleteBody(tunnel.key),
    action: l10n.delete,
  );
  if (!yes || !context.mounted) return;
  final result = await store.remove(tunnel);
  if (!context.mounted) return;
  showToast(
    context,
    result.success ? l10n.toastDeleted(tunnel.key) : result.message,
    kind: result.success ? ToastKind.info : ToastKind.error,
  );
}

/// Put [value] on the clipboard and say so.
Future<void> copyText(BuildContext context, String value) async {
  await Clipboard.setData(ClipboardData(text: value));
  if (!context.mounted) return;
  showToast(context, context.l10n.copied(value), kind: ToastKind.success);
}
