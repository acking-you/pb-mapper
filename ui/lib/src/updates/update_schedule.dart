import 'dart:async';

import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';

/// The background check: at launch and then about twice a day while the app
/// keeps running in the tray, unless the user turned it off. A newer release
/// is announced once; downloading it waits for the user.
class UpdateSchedule {
  UpdateSchedule({
    required this.updates,
    required this.prefs,
    required this.announce,
    this.every = const Duration(hours: 12),
    DateTime Function()? now,
  }) : _now = now ?? DateTime.now;

  final UpdateController updates;
  final AppPrefs prefs;

  /// Tell the user about [version], which they have not heard of yet.
  final void Function(Version version) announce;
  final Duration every;
  final DateTime Function() _now;
  Timer? _timer;

  void start() {
    unawaited(tick());
    // Hourly, so a check is at most an hour late after a sleep.
    _timer = Timer.periodic(const Duration(hours: 1), (_) => tick());
  }

  /// Check if one is due.
  Future<void> tick() async {
    if (!prefs.autoCheckUpdates || updates.busy) return;
    final last = prefs.lastUpdateCheck;
    final now = _now();
    if (last != null && now.difference(last) < every) return;
    await prefs.markUpdateCheck(now);
    await updates.check(quiet: true);
    final release = updates.published;
    if (!updates.newer || release == null) return;
    final version = '${release.version}';
    if (prefs.announcedUpdate == version) return;
    await prefs.markUpdateAnnounced(version);
    announce(release.version);
  }

  void dispose() => _timer?.cancel();
}
