/// Putting a verified update in place, per platform.
///
/// By the time anything here runs, the file's size and SHA-256 have been
/// checked against a manifest signed by the release key. What is left is the
/// platform's own way of replacing a running app: Windows runs the installer
/// once the app has quit, Linux swaps the AppImage it runs from, and a disk
/// image or a portable archive is handed to the user, who knows where the
/// app lives.
library;

import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_ffi.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:url_launcher/url_launcher.dart';

/// The platform effects of an update, behind an interface so the flow can be
/// tested without replacing anything.
abstract interface class UpdateInstaller {
  /// Run [installer] quietly once this process has exited, then open the app
  /// again. The caller quits right after; this throws an [UpdateException]
  /// when nothing could be started to do it, so the caller stays.
  Future<void> runInstallerAfterExit(File installer);

  /// Put [download] in place of the AppImage at [appImage] and open it once
  /// this process has exited. The caller quits right after.
  Future<void> replaceAppImage(File download, String appImage);

  /// Move [download] into Downloads as [name] and show it; returns where it
  /// went.
  Future<String> saveToDownloads(File download, String name);

  /// Open [url] outside the app.
  Future<void> open(Uri url);
}

class PlatformUpdateInstaller implements UpdateInstaller {
  const PlatformUpdateInstaller();

  @override
  Future<void> runInstallerAfterExit(File installer) =>
      WindowsInstallerUpdate.handOff(
        installer,
        pid,
        Platform.resolvedExecutable,
      );

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
      await launchUrl(Uri.file(saved.path));
    } else {
      await launchUrl(Uri.directory(downloads.path));
    }
    return saved.path;
  }

  @override
  Future<void> open(Uri url) async {
    await launchUrl(url, mode: LaunchMode.externalApplication);
  }
}

Future<void> _delete(File file) async {
  try {
    if (await file.exists()) await file.delete();
  } catch (_) {
    // A leftover temporary file is not worth failing over.
  }
}

/// Windows, installed: the Inno Setup installer fastforge builds. It reuses
/// the previous install's folder (by its AppId), and asks for elevation
/// itself when the install is a machine-wide one.
abstract final class WindowsInstallerUpdate {
  /// Whether this copy was set up by the installer rather than unpacked from
  /// the portable archive: Inno Setup leaves its uninstaller beside the
  /// executable.
  static bool isInstalled([String? executable]) => File(
    '${File(executable ?? Platform.resolvedExecutable).parent.path}'
    '${Platform.pathSeparator}unins000.exe',
  ).existsSync();

  /// Start a hidden PowerShell that waits for [processId] to exit, runs
  /// [installer] quietly, and opens [app] again whether or not the install
  /// succeeded, so the user is never left without the app. It gives up,
  /// installing nothing, if the app has not exited within a minute.
  ///
  /// Started by `pb_mapper_spawn_hidden_powershell_v1` in the app's FFI
  /// library (or [library]), not by `Process.start`: a detached Dart child
  /// has no console, and PowerShell exits at once without running a line.
  /// Throws an [UpdateException] when it could not be started.
  static Future<void> handOff(
    File installer,
    int processId,
    String app, [
    DynamicLibrary? library,
  ]) async {
    final spawn = (library ?? PbMapperFFI().lib)
        .lookupFunction<_SpawnNative, _SpawnDart>(
          'pb_mapper_spawn_hidden_powershell_v1',
        );
    final command = script(
      installer.absolute.path,
      processId,
      app,
    ).toNativeUtf8();
    try {
      switch (spawn(command)) {
        case 1:
          return;
        case -1:
          throw const UpdateException(
            UpdateFailure.install,
            'The installer hand-off script was refused as malformed',
          );
        default:
          throw const UpdateException(
            UpdateFailure.install,
            'Windows would not start the installer hand-off (see the log)',
          );
      }
    } finally {
      malloc.free(command);
    }
  }

  /// The PowerShell [handOff] runs. Paths go in single quotes, where only a
  /// single quote is special. Setup writes its log beside the installer
  /// (`setup.log`). When setup succeeds the download's folder is removed;
  /// when it fails only the installer is, and the log stays for whoever
  /// asks why the update did not go in.
  static String script(String installer, int processId, String app) {
    String quote(String text) => "'${text.replaceAll("'", "''")}'";
    // A Windows path, read as Windows reads it whatever runs this, so the
    // script is the same when a test builds it elsewhere. The installer has
    // its download folder to itself.
    final cut = installer.lastIndexOf(RegExp(r'[\\/]'));
    final folder = cut < 0 ? null : installer.substring(0, cut);
    final log = '${folder ?? '.'}\\setup.log';
    final removeInstaller =
        'Remove-Item -LiteralPath ${quote(installer)} -ErrorAction SilentlyContinue';
    // Installed: nothing left to keep. Not installed: the log stays, for
    // whoever asks why.
    final cleanUp = folder == null
        ? removeInstaller
        : 'if (\$setup.ExitCode -eq 0) { Remove-Item -LiteralPath '
              '${quote(folder)} -Recurse -Force -ErrorAction SilentlyContinue } '
              'else { $removeInstaller }';
    return [
      'try { Wait-Process -Id $processId -Timeout 60 -ErrorAction Stop } catch {}',
      'if (Get-Process -Id $processId -ErrorAction SilentlyContinue) { exit 1 }',
      // Start-Process joins its arguments with spaces and quotes none of
      // them, so a log path with a space is quoted for setup here.
      '\$setup = Start-Process -FilePath ${quote(installer)} '
          "-ArgumentList '/SILENT','/SUPPRESSMSGBOXES','/NORESTART','/SP-',"
          '${quote('/LOG="$log"')} -Wait -PassThru',
      cleanUp,
      'Start-Process -FilePath ${quote(app)}',
    ].join('; ');
  }
}

typedef _SpawnNative = Int32 Function(Pointer<Utf8>);
typedef _SpawnDart = int Function(Pointer<Utf8>);

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
