/// What an update that replaces the app expects to find on the other side.
///
/// The app quits for the installer, and whatever opens next is a new process:
/// left to itself it cannot tell an update that worked from one that never
/// ran (a declined permission prompt, a failed install), and neither can the
/// person, who saw the app vanish. So the update writes down where it was
/// going before it hands off, and the next launch reads it once.
library;

import 'dart:convert';
import 'dart:io';

import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';

/// An update handed off to an installer, on its way from [from] to [to].
class PendingUpdate {
  const PendingUpdate({required this.from, required this.to, required this.at});

  final Version from;
  final Version to;
  final DateTime at;

  Map<String, Object> toJson() => {
    'from': '$from',
    'to': '$to',
    'at': at.toUtc().toIso8601String(),
  };

  static PendingUpdate? fromJson(Object? json) {
    if (json is! Map) return null;
    final from = Version.tryParse('${json['from']}');
    final to = Version.tryParse('${json['to']}');
    final at = DateTime.tryParse('${json['at']}');
    if (from == null || to == null || at == null) return null;
    return PendingUpdate(from: from, to: to, at: at);
  }
}

/// Where a [PendingUpdate] waits for the next launch.
abstract interface class UpdateRecord {
  Future<void> write(PendingUpdate pending);

  /// The pending update, if there is one, which is forgotten as it is read.
  Future<PendingUpdate?> take();
}

/// A file in the app's support folder, which an update leaves alone.
class FileUpdateRecord implements UpdateRecord {
  const FileUpdateRecord();

  Future<File> _file() async {
    final dir = await getApplicationSupportDirectory();
    return File('${dir.path}${Platform.pathSeparator}pending-update.json');
  }

  @override
  Future<void> write(PendingUpdate pending) async {
    final file = await _file();
    await file.parent.create(recursive: true);
    await file.writeAsString(jsonEncode(pending.toJson()), flush: true);
  }

  @override
  Future<PendingUpdate?> take() async {
    final file = await _file();
    if (!await file.exists()) return null;
    try {
      return PendingUpdate.fromJson(jsonDecode(await file.readAsString()));
    } on FormatException {
      return null;
    } finally {
      try {
        await file.delete();
      } on FileSystemException {
        // Read is what matters; a record left behind ages out.
      }
    }
  }
}
