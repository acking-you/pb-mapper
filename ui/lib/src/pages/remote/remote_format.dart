import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';

/// The words and masks the Remote page shows: never a whole invitation,
/// never an OS display name, dates in the reader's own calendar.

/// A button label at [size] in the theme's label style. A button style's
/// own `textStyle` replaces the theme's rather than merging with it, so a
/// bare `TextStyle` there would lose the UI typeface.
TextStyle? buttonLabel(
  BuildContext context,
  double size, {
  FontWeight weight = FontWeight.w500,
}) => Theme.of(
  context,
).textTheme.labelLarge?.copyWith(fontSize: size, fontWeight: weight);

/// An invitation as the page may show it: its format, the first and last
/// four characters of its payload, and bullets for the secret between. A
/// v1 JSON invitation shows only its format.
String maskInvitation(String invitation) {
  final text = invitation.trim();
  const bullets = '••••••••••••••••••••';
  if (text.startsWith('pbd1.')) {
    final payload = text.substring(5);
    if (payload.length < 12) return 'pbd1·$bullets';
    return 'pbd1·${payload.substring(0, 4)}$bullets'
        '${payload.substring(payload.length - 4)}';
  }
  return 'json·$bullets••••';
}

/// Whether [text] has an invitation in it, token and all: a `pbd1` one
/// anywhere, or a v1 JSON one.
bool carriesInvitation(String text) =>
    text.contains('pbd1.') || text.contains('"token"');

/// `direct` and `relay` in words, a direct path on one LAN ([lan]) as
/// such, and one over TCP ([tcp]) with its carrier, since QUIC is the
/// usual one; anything else (not yet known) as a dash.
String pathLabel(
  BuildContext context,
  String? path, {
  bool lan = false,
  bool tcp = false,
}) => switch (path) {
  'direct' => [
    lan ? context.l10n.pathLan : context.l10n.pathDirect,
    if (tcp) 'TCP',
  ].join(' · '),
  'relay' => context.l10n.pathRelay,
  _ => '—',
};

/// A transfer rate in the largest binary unit it reaches, to three figures
/// at most, so it never runs past nine characters and a fixed cell holds
/// it: "512 B/s", "9.5 KiB/s", "286 KiB/s", "2.4 MiB/s".
String formatSpeed(int bytesPerSecond) {
  const units = ['B/s', 'KiB/s', 'MiB/s', 'GiB/s'];
  var value = bytesPerSecond.toDouble();
  var unit = 0;
  while (value >= 999.5 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  final digits = unit == 0 || value >= 9.95 ? 0 : 1;
  return '${value.toStringAsFixed(digits)} ${units[unit]}';
}

/// How long a viewer has been connected: just now, minutes, then hours.
String formatSessionDuration(BuildContext context, Duration elapsed) {
  final l10n = context.l10n;
  if (elapsed.inMinutes < 1) return l10n.hostDurationJustNow;
  if (elapsed.inMinutes < 60) {
    return l10n.hostDurationMinutes(elapsed.inMinutes);
  }
  return l10n.hostDurationHours(elapsed.inHours, elapsed.inMinutes % 60);
}

/// When a computer was last opened: today or yesterday with the time, a
/// day this year, or a full date before that.
String formatLastSeen(BuildContext context, DateTime when, DateTime now) {
  final l10n = context.l10n;
  final material = MaterialLocalizations.of(context);
  final local = when.toLocal();
  final today = DateUtils.dateOnly(now.toLocal());
  final day = DateUtils.dateOnly(local);
  final time =
      '${local.hour.toString().padLeft(2, '0')}:'
      '${local.minute.toString().padLeft(2, '0')}';
  if (day == today) return l10n.remoteToday(time);
  if (day == DateTime(today.year, today.month, today.day - 1)) {
    return l10n.remoteYesterday(time);
  }
  return local.year == today.year
      ? material.formatShortMonthDay(local)
      : material.formatCompactDate(local);
}
