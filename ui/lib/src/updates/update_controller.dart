/// The one update flow: check, confirm, then fetch, verify and install.
///
/// A check runs when the user asks, or once a day in the background while
/// they let it (see `checkInBackground`); nothing is downloaded until they
/// press Update.
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:pb_mapper_ui/src/updates/update_installers.dart';
import 'package:pb_mapper_ui/src/updates/update_record.dart';
import 'package:pb_mapper_ui/src/updates/update_source.dart';

/// Whether this build can update itself. iOS builds are unsigned
/// development packages, installed by hand.
bool get updatesSupported =>
    !kIsWeb &&
    (Platform.isWindows ||
        Platform.isMacOS ||
        Platform.isLinux ||
        Platform.isAndroid);

enum UpdateStage {
  idle,
  checking,
  upToDate,
  available,

  /// A newer release exists, without a file for this device.
  noPackage,
  downloading,
  verifying,

  /// Handed over: the app is quitting so the update can replace it.
  installing,

  /// Saved to Downloads for the user to finish.
  saved,

  /// Opened in the browser (Android).
  opened,

  /// Debug builds verify everything and stop short of replacing themselves.
  verifiedOnly,
  failed,
}

/// What became of an update handed off before the app last quit.
class UpdateOutcome {
  const UpdateOutcome({required this.to, required this.installed});

  /// The version it was installing.
  final Version to;

  /// Whether this app is that version now.
  final bool installed;
}

class UpdateController extends ChangeNotifier {
  UpdateController({
    required this.quit,
    UpdateSource? source,
    this.installer = const PlatformUpdateInstaller(),
    this.record = const FileUpdateRecord(),
    this.loadInstalled = _loadInstalled,
    this.loadHost = _loadHost,
    this.workDir = _workDir,
    bool? replaceSelf,
  }) : source = source ?? UpdateSource(),
       replaceSelf = replaceSelf ?? !kDebugMode;

  /// Quit through the app's own path (tray removed, tunnels stopped).
  final Future<void> Function() quit;
  final UpdateSource source;
  final UpdateInstaller installer;
  final UpdateRecord record;
  final Future<Version> Function() loadInstalled;
  final Future<UpdateHost> Function() loadHost;

  /// A fresh folder for one download. A Windows installer stays in it after
  /// the app quits, until the hand-off has run it.
  final Future<Directory> Function() workDir;

  /// How long after a hand-off its outcome is still news. The installer
  /// reopens the app within a minute; a record older than this belongs to a
  /// launch nobody is waiting on.
  static const outcomeWindow = Duration(hours: 1);

  /// Off in debug builds: trying the flow from a development build verifies
  /// the download and stops, so it cannot clobber itself.
  final bool replaceSelf;

  UpdateStage _stage = UpdateStage.idle;
  Version? _installed;
  UpdateHost? _host;
  PublishedRelease? _published;
  UpdatePlan? _plan;
  UpdateException? _failure;
  String? _savedPath;
  int _received = 0;
  double _bytesPerSecond = 0;
  bool _disposed = false;

  UpdateStage get stage => _stage;
  Version? get installed => _installed;

  /// The newest release, once a check has found one.
  PublishedRelease? get published => _published;
  UpdatePlan? get plan => _plan;
  UpdateException? get failure => _failure;
  String? get savedPath => _savedPath;
  int get received => _received;
  double get bytesPerSecond => _bytesPerSecond;

  double get progress {
    final size = _plan?.asset.size ?? 0;
    return size == 0 ? 0 : _received / size;
  }

  /// Whether the last check found a release newer than this one, installable
  /// from here or not.
  bool get newer {
    final published = _published;
    final installed = _installed;
    return published != null &&
        installed != null &&
        published.version > installed;
  }

  bool get busy =>
      _stage == UpdateStage.checking ||
      _stage == UpdateStage.downloading ||
      _stage == UpdateStage.verifying ||
      _stage == UpdateStage.installing;

  void _set(UpdateStage stage) {
    _stage = stage;
    if (!_disposed) notifyListeners();
  }

  void _fail(UpdateException failure) {
    _failure = failure;
    debugPrint('Update failed: $failure');
    _set(UpdateStage.failed);
  }

  /// Ask GitHub for the newest release and, when it is newer than this app,
  /// read its signed manifest. A [quiet] check that fails leaves no trace
  /// beyond the log: nobody asked for it, so whatever the last check found
  /// stays on offer.
  Future<void> check({bool quiet = false}) async {
    if (busy) return;
    final stage = _stage;
    final published = _published;
    final plan = _plan;
    _failure = null;
    _plan = null;
    _set(UpdateStage.checking);
    void restore(Object failure) {
      debugPrint('Background update check failed: $failure');
      _published = published;
      _plan = plan;
      _set(stage);
    }

    try {
      final installed = _installed ??= await loadInstalled();
      final host = _host ??= await loadHost();
      final release = await source.newest();
      _published = release;
      if (release == null || !(release.version > installed)) {
        _set(UpdateStage.upToDate);
        return;
      }
      final manifest = await source.manifest(release);
      _plan = planUpdate(manifest, host);
      _set(_plan == null ? UpdateStage.noPackage : UpdateStage.available);
    } on UpdateException catch (failure) {
      if (!quiet) {
        _fail(failure);
      } else if (failure.kind == UpdateFailure.unsigned) {
        // A newer release that cannot be installed from here is still worth
        // pointing at.
        debugPrint('Background update check: $failure');
        _set(UpdateStage.noPackage);
      } else {
        restore(failure);
      }
    } catch (error) {
      if (quiet) {
        restore(error);
      } else {
        _fail(UpdateException(UpdateFailure.network, '$error'));
      }
    }
  }

  /// The user confirmed: fetch the file, check it, and put it in place.
  Future<void> start() async {
    final published = _published;
    final plan = _plan;
    if (published == null || plan == null || busy || _disposed) return;
    final url = published.files[plan.asset.name];
    if (url == null) {
      _fail(
        UpdateException(
          UpdateFailure.network,
          '${plan.asset.name} is listed but not attached',
        ),
      );
      return;
    }
    _failure = null;
    if (plan.method == UpdateMethod.openInBrowser) {
      await installer.open(url);
      _set(UpdateStage.opened);
      return;
    }

    _received = 0;
    _bytesPerSecond = 0;
    _set(UpdateStage.downloading);
    final started = DateTime.now();
    File? file;
    try {
      final work = await workDir();
      file = File('${work.path}${Platform.pathSeparator}${plan.asset.name}');
      final digest = await source.http.download(
        url,
        file,
        size: plan.asset.size,
        onProgress: (received) {
          _received = received;
          final seconds =
              DateTime.now().difference(started).inMilliseconds / 1000;
          _bytesPerSecond = seconds <= 0 ? 0 : received / seconds;
          if (!_disposed) notifyListeners();
        },
      );
      _set(UpdateStage.verifying);
      if (digest != plan.asset.sha256) {
        throw UpdateException(
          UpdateFailure.checksum,
          '${plan.asset.name} hashes to $digest, the manifest lists '
          '${plan.asset.sha256}',
        );
      }
      await _apply(plan, file);
    } on UpdateCancelled {
      await _discard(file);
      _set(UpdateStage.available);
    } on UpdateException catch (failure) {
      await _discard(file);
      _fail(failure);
    } catch (error) {
      await _discard(file);
      _fail(UpdateException(UpdateFailure.install, '$error'));
    }
  }

  Future<void> _apply(UpdatePlan plan, File file) async {
    final replacesSelf =
        plan.method == UpdateMethod.installer ||
        plan.method == UpdateMethod.replaceAppImage;
    if (replacesSelf && !replaceSelf) {
      _set(UpdateStage.verifiedOnly);
      return;
    }
    switch (plan.method) {
      case UpdateMethod.installer:
        // Noted only once something is waiting to run it: a hand-off that
        // could not start fails here, with the app still open.
        await installer.runInstallerAfterExit(file);
        await _remember();
        _set(UpdateStage.installing);
        await quit();
      case UpdateMethod.replaceAppImage:
        final appImage = _host?.appImage;
        try {
          if (appImage == null) {
            throw const FileSystemException('Not running from an AppImage');
          }
          await installer.replaceAppImage(file, appImage);
        } on FileSystemException catch (error) {
          // Not ours to replace (a read-only or root-owned folder): hand
          // the new AppImage to the user instead.
          debugPrint('Could not replace the AppImage: $error');
          _savedPath = await installer.saveToDownloads(file, plan.asset.name);
          await _discard(file);
          _set(UpdateStage.saved);
          return;
        }
        await _remember();
        await _discard(file);
        _set(UpdateStage.installing);
        await quit();
      case UpdateMethod.saveDownload:
        _savedPath = await installer.saveToDownloads(file, plan.asset.name);
        await _discard(file);
        _set(UpdateStage.saved);
      case UpdateMethod.openInBrowser:
        // Handled before anything is downloaded.
        break;
    }
  }

  /// Write down where this update is going, for the launch after it to check.
  /// Best effort: an update is not refused for want of a note.
  Future<void> _remember() async {
    final from = _installed;
    final to = _published?.version;
    if (from == null || to == null) return;
    try {
      await record.write(PendingUpdate(from: from, to: to, at: DateTime.now()));
    } catch (error) {
      debugPrint('Could not record the update: $error');
    }
  }

  /// What became of the update handed off before this app last quit: null
  /// when there was none, or when it is too old to be the one just made.
  /// Read once; the next launch hears nothing.
  Future<UpdateOutcome?> takeOutcome() async {
    try {
      final pending = await record.take();
      if (pending == null ||
          DateTime.now().difference(pending.at) > outcomeWindow) {
        return null;
      }
      final installed = _installed ??= await loadInstalled();
      return UpdateOutcome(
        to: pending.to,
        installed: !(pending.to > installed),
      );
    } catch (error) {
      debugPrint('Could not read the last update: $error');
      return null;
    }
  }

  /// Remove the download's temporary folder. Not after handing it to the
  /// Windows installer, which runs it once the app has quit.
  static Future<void> _discard(File? file) async {
    if (file == null) return;
    try {
      await file.parent.delete(recursive: true);
    } catch (_) {}
  }

  void cancel() {
    if (_stage == UpdateStage.downloading) source.http.cancel();
  }

  @override
  void dispose() {
    _disposed = true;
    source.http.cancel();
    super.dispose();
  }

  static Future<Directory> _workDir() =>
      Directory.systemTemp.createTemp('pb-mapper-update-');

  static Future<Version> _loadInstalled() async {
    final info = await PackageInfo.fromPlatform();
    final version = Version.tryParse(info.version);
    if (version == null) {
      // Without its own version the app would take every release for an
      // update.
      throw UpdateException(
        UpdateFailure.install,
        'The app does not know its version (${info.version})',
      );
    }
    return version;
  }

  static Future<UpdateHost> _loadHost() async {
    // Dart names the processor it was compiled for at the end of its
    // version: `... on "windows_x64"`.
    final target = RegExp(r'on "\w+_(\w+)"').firstMatch(Platform.version);
    return UpdateHost(
      os: Platform.operatingSystem,
      arch: target?[1] ?? 'x64',
      windowsInstalled:
          Platform.isWindows && WindowsInstallerUpdate.isInstalled(),
      appImage: Platform.isLinux ? AppImageUpdate.current() : null,
    );
  }
}
