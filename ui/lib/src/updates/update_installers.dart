/// Putting a verified update in place, per platform.
///
/// By the time anything here runs, the file's size and SHA-256 have been
/// checked against a manifest signed by the release key. What is left is the
/// platform's own way of replacing a running app: Windows starts the
/// installer, which waits for the app to quit, Linux swaps the AppImage it
/// runs from, and a disk image or a portable archive is handed to the user,
/// who knows where the app lives.
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:url_launcher/url_launcher.dart';

/// The platform effects of an update, behind an interface so the flow can be
/// tested without replacing anything.
abstract interface class UpdateInstaller {
  /// Start [installer], which waits for this process to quit before it
  /// replaces anything and opens the app again when it ends. Completes once
  /// the installer runs with the rights it installs with, which Windows may
  /// first ask the user for, in front of this app: the caller then quits.
  /// Throws an [UpdateException] when the installer could not start or ended
  /// before that (permission refused, a failure), and the caller stays.
  Future<void> startInstaller(File installer);

  /// Put [download] in place of the AppImage at [appImage] and open it once
  /// this process has exited. The caller quits right after.
  Future<void> replaceAppImage(File download, String appImage);

  /// Move [download] into Downloads as [name] and show it; returns where it
  /// went.
  Future<String> saveToDownloads(File download, String name);

  /// Verify package identity, request install permission if needed, and open
  /// Android's confirmation. Permission denial keeps the download retryable.
  Future<void> installAndroidPackage(File package);
}

class PlatformUpdateInstaller implements UpdateInstaller {
  const PlatformUpdateInstaller();

  @override
  Future<void> startInstaller(File installer) =>
      WindowsInstallerUpdate.start(installer, pid);

  @override
  Future<void> replaceAppImage(File download, String appImage) =>
      AppImageUpdate.replace(download, appImage, pid);

  @override
  Future<String> saveToDownloads(File download, String name) async {
    final downloads =
        await getDownloadsDirectory() ??
        Directory(
          '${Platform.environment['HOME'] ?? Platform.environment['USERPROFILE'] ?? download.parent.path}'
          '${Platform.pathSeparator}Downloads',
        );
    await downloads.create(recursive: true);
    final target = File('${downloads.path}${Platform.pathSeparator}$name');
    if (await target.exists()) await target.delete();
    final saved = await download.copy(target.path);
    await _delete(download);
    if (Platform.isWindows) {
      // Explorer, with the file selected.
      await Process.start('explorer.exe', ['/select,', saved.path]);
    } else if (Platform.isMacOS) {
      // Opening the disk image mounts it, with the app ready to drag into
      // Applications. Through NSWorkspace rather than a child process, which
      // the app sandbox would confine.
      await _open(Uri.file(saved.path));
    } else {
      await _open(Uri.directory(downloads.path));
    }
    return saved.path;
  }

  @override
  Future<void> installAndroidPackage(File package) =>
      AndroidPackageUpdate.install(package);

  static Future<void> _open(Uri url) async {
    if (!await launchUrl(url, mode: LaunchMode.externalApplication)) {
      throw UpdateException(UpdateFailure.install, 'Could not open $url');
    }
  }
}

abstract final class AndroidPackageUpdate {
  static const channel = MethodChannel('pb_mapper/updates');

  static Future<List<String>> abis() async =>
      await channel.invokeListMethod<String>('abis') ?? const [];

  static Future<void> install(File package) async {
    try {
      await channel.invokeMethod<void>('install', {'path': package.path});
    } on PlatformException catch (error) {
      throw UpdateException(switch (error.code) {
        'permission_denied' => UpdateFailure.permission,
        'incompatible_package' => UpdateFailure.incompatiblePackage,
        _ => UpdateFailure.install,
      }, error.message);
    }
  }
}

Future<void> _delete(File file) async {
  try {
    if (await file.exists()) await file.delete();
  } catch (_) {
    // A leftover temporary file is not worth failing over.
  }
}

/// Windows, installed: the Inno Setup installer that
/// `ui/windows/packaging/exe/inno_setup.iss` defines. It updates the
/// installation it finds by its AppId, a machine-wide one after Windows asks
/// for permission, waits for the app named by `/UPDATEPID` to quit before it
/// replaces a file, and when it ends opens the installed app again as the
/// user who started it.
abstract final class WindowsInstallerUpdate {
  /// Whether this copy was set up by the installer rather than unpacked from
  /// the portable archive: Inno Setup leaves its uninstaller beside the
  /// executable. A copy of an installation, such as the desktop helper's
  /// protected one, counts too: the installer updates the installation and
  /// opens that.
  static bool isInstalled([String? executable]) => File(
    '${File(executable ?? Platform.resolvedExecutable).parent.path}'
    '${Platform.pathSeparator}unins000.exe',
  ).existsSync();

  /// Setup's log, beside the installer: the download's folder is its own.
  static File log(File installer) =>
      File('${installer.parent.path}${Platform.pathSeparator}setup.log');

  /// Start [installer] to update the app running as [processId]; see
  /// [UpdateInstaller.startInstaller].
  static Future<void> start(File installer, int processId) =>
      run(installer.path, [
        '/SILENT',
        '/SUPPRESSMSGBOXES',
        '/NORESTART',
        '/SP-',
        '/LOG=${log(installer).path}',
        '/UPDATEPID=$processId',
      ], log(installer));

  /// Start [executable] with [arguments], and complete once [ready] exists
  /// while it still runs. Setup starts its log only in the process that
  /// installs: for an installation for all users, the one Windows starts
  /// elevated once the user allowed it. A [ready] left by an earlier attempt
  /// is removed first, so that it cannot count.
  ///
  /// Started from here, while this app is open and in front, so that Windows
  /// asks for that permission in front of it. Started after the app had quit,
  /// by a process with no window, Setup asked from behind, where Windows may
  /// leave the prompt as a flashing taskbar button: unseen, it left nothing
  /// installed and no app open.
  ///
  /// Throws an [UpdateException] when [executable] cannot start, or ends
  /// first.
  @visibleForTesting
  static Future<void> run(
    String executable,
    List<String> arguments,
    File ready, {
    Duration poll = const Duration(milliseconds: 100),
  }) async {
    if (await ready.exists()) await ready.delete();
    final Process setup;
    try {
      setup = await Process.start(executable, arguments);
    } on ProcessException catch (error) {
      throw UpdateException(
        UpdateFailure.install,
        'Could not start the installer: ${error.message}',
      );
    }
    // Setup writes nothing there; drained, the pipes can never fill.
    unawaited(setup.stdout.drain<void>());
    unawaited(setup.stderr.drain<void>());
    int? ended;
    unawaited(setup.exitCode.then((code) => ended = code));
    while (ended == null && !await ready.exists()) {
      await Future<void>.delayed(poll);
    }
    if (ended case final code?) {
      throw UpdateException(
        UpdateFailure.install,
        'The installer ended with exit code $code before installing anything'
        '${code == 2 ? ': Windows was not given permission, or it was cancelled' : ''}.',
      );
    }
  }

  /// What is left of an update's download once the app it installed has
  /// opened: the installer goes, and Setup's log too when the update went in,
  /// then the folder if that left it empty. The log of an update that did not
  /// go in stays, for whoever asks why.
  static Future<void> removeDownload(
    String installer, {
    required bool installed,
  }) async {
    final file = File(installer);
    await _delete(file);
    if (installed) await _delete(log(file));
    try {
      await file.parent.delete();
    } on FileSystemException {
      // Not empty, or already gone.
    }
  }
}

/// Linux, as an AppImage: the file the app runs from is replaced, which a
/// running AppImage tolerates, since it keeps its own mount of the old file.
abstract final class AppImageUpdate {
  /// The AppImage this app runs from, if it runs from one.
  static String? current() {
    final path = Platform.environment['APPIMAGE'];
    if (path == null || path.isEmpty || !File(path).existsSync()) return null;
    return path;
  }

  /// Put [download] in place of [appImage]. [download] stays where it is,
  /// for the caller to save elsewhere when this fails with a
  /// [FileSystemException] (a folder the user cannot write to) or to clean
  /// up when it succeeds.
  static Future<void> replace(File download, String appImage, int pid) async {
    final target = File(appImage);
    // Staged beside the target, so the rename that replaces it is atomic.
    final staged = File(
      '${target.parent.path}/.${target.uri.pathSegments.last}.update',
    );
    try {
      await download.copy(staged.path);
      // On disk before it takes the AppImage's name: a power loss after the
      // rename must not leave a name pointing at data that never landed.
      final written = await staged.open(mode: FileMode.append);
      try {
        await written.flush();
      } finally {
        await written.close();
      }
      final ProcessResult chmod;
      try {
        chmod = await Process.run('chmod', ['755', staged.path]);
      } on ProcessException catch (error) {
        throw FileSystemException('chmod failed: $error', staged.path);
      }
      if (chmod.exitCode != 0) {
        throw FileSystemException('chmod failed: ${chmod.stderr}', staged.path);
      }
      await staged.rename(target.path);
    } catch (_) {
      await _delete(staged);
      rethrow;
    }
    await Process.start('/bin/sh', [
      '-c',
      r'while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$2"',
      'pb-mapper-update',
      '$pid',
      appImage,
    ], mode: ProcessStartMode.detached);
  }
}
