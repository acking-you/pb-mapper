import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';

/// Builds a tab of the Relay page again whenever the app or the relay
/// changes. The page builds its tabs as constants, so its own rebuilding
/// stops short of them; each tab listens for itself.
class RelayListener extends StatelessWidget {
  const RelayListener({super.key, required this.builder});

  final WidgetBuilder builder;

  @override
  Widget build(BuildContext context) {
    final scope = AppScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([scope.store, scope.relay]),
      builder: (context, _) => builder(context),
    );
  }
}

/// [span] in the largest whole unit: "45 s", "12 min", "3 h", "6 d".
String formatSpan(BuildContext context, Duration span) {
  final l10n = context.l10n;
  final s = span.isNegative ? -span : span;
  if (s.inMinutes < 1) return l10n.spanSeconds(s.inSeconds);
  if (s.inHours < 1) return l10n.spanMinutes(s.inMinutes);
  if (s.inDays < 1) return l10n.spanHours(s.inHours);
  return l10n.spanDays(s.inDays);
}

/// "just now", or "3 min ago".
String formatAgo(BuildContext context, Duration span) => span.inSeconds < 5
    ? context.l10n.justNow
    : context.l10n.timeAgo(formatSpan(context, span));

/// "in 6 d".
String formatIn(BuildContext context, Duration span) =>
    context.l10n.timeIn(formatSpan(context, span));

DateTime fromUnix(int seconds) =>
    DateTime.fromMillisecondsSinceEpoch(seconds * 1000);

String formatDate(int unixSeconds) {
  final d = fromUnix(unixSeconds);
  String two(int n) => n.toString().padLeft(2, '0');
  return '${d.year}-${two(d.month)}-${two(d.day)} ${two(d.hour)}:${two(d.minute)}';
}

/// Who registered something on the relay, from its credential id.
String ownerName(BuildContext context, RelayStore relay, int keyId) {
  final l10n = context.l10n;
  if (keyId == 0) return l10n.relayOwnerAdmin;
  final label = relay.labelOf(keyId);
  return label == null
      ? l10n.relayOwnerKey(keyId)
      : '${l10n.relayOwnerKey(keyId)} · $label';
}

/// A credential's name: its label, or its number.
String credentialName(BuildContext context, RelayKey key) =>
    key.label ?? context.l10n.credentialUnnamed(key.keyId);

/// Where a credential stands, in words: expires in 6 d, expired 2 h ago,
/// revoked.
String credentialStanding(BuildContext context, RelayKey key) {
  final l10n = context.l10n;
  final now = DateTime.now();
  final expires = fromUnix(key.expiresAt);
  return switch (key.state) {
    RelayKeyState.revoked => l10n.credentialStateRevoked,
    RelayKeyState.active when expires.isAfter(now) => l10n.credentialExpires(
      formatIn(context, expires.difference(now)),
    ),
    _ => l10n.credentialExpired(formatAgo(context, now.difference(expires))),
  };
}

/// [key]'s state as of now: one the relay still lists as active after its
/// time ran out is expired, before the relay's next sweep says so.
RelayKeyState standingOf(RelayKey key) =>
    key.state == RelayKeyState.active &&
        !fromUnix(key.expiresAt).isAfter(DateTime.now())
    ? RelayKeyState.expired
    : key.state;

Color credentialColor(ColorScheme scheme, RelayKeyState state) =>
    switch (state) {
      RelayKeyState.active => successColor(scheme),
      RelayKeyState.expired => cautionColor(scheme),
      RelayKeyState.revoked => scheme.error,
      RelayKeyState.unknown => availableColor(scheme),
    };

String credentialStateLabel(BuildContext context, RelayKeyState state) {
  final l10n = context.l10n;
  return switch (state) {
    RelayKeyState.active => l10n.credentialStateActive,
    RelayKeyState.expired => l10n.credentialStateExpired,
    RelayKeyState.revoked => l10n.credentialStateRevoked,
    RelayKeyState.unknown => '—',
  };
}

/// Copy a secret without showing it again in the toast, as [copyText] would.
Future<void> copySecret(
  BuildContext context,
  String value,
  String message,
) async {
  await Clipboard.setData(ClipboardData(text: value));
  if (!context.mounted) return;
  showToast(context, message, kind: ToastKind.success);
}

/// Run [change], and show its failure as a toast rather than throw it.
Future<T?> attempt<T>(BuildContext context, Future<T> Function() change) async {
  try {
    return await change();
  } on RelayCallException catch (error) {
    if (context.mounted) {
      showToast(context, error.message, kind: ToastKind.error);
    }
    return null;
  }
}

/// The lifetimes a credential can be given or renewed for.
const List<Duration> credentialLifetimes = [
  Duration(hours: 1),
  Duration(days: 1),
  Duration(days: 7),
  Duration(days: 30),
  Duration(days: 90),
];

String lifetimeLabel(BuildContext context, Duration ttl) {
  final l10n = context.l10n;
  if (ttl.inDays >= 1) return l10n.lifetimeDays(ttl.inDays);
  return l10n.lifetimeHours(ttl.inHours);
}
