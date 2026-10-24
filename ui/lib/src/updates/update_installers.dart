/// Putting a verified update in place, per platform.
///
/// By the time anything here runs, the file's size and SHA-256 have been
/// checked against a manifest signed by the release key. What is left is the
/// platform's own way of replacing a running app: Windows runs the installer
/// once the app has quit, Linux swaps the AppImage it runs from, and a disk
/// image or a portable archive is handed to the user, who knows where the
/// app lives.
library;

import 'dart:io';

import 'package:path_provider/path_provider.dart';
import 'package:url_launcher/url_launcher.dart';

/// The platform effects of an update, behind an interface so the flow can be
/// tested without replacing anything.
abstract interface class UpdateInstaller {
  /// Run [installer] quietly once this process has exited, then open the app
  /// again. The caller quits right after.
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
  static Future<void> handOff(File installer, int processId, String app) async {
    await Process.start('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-WindowStyle',
      'Hidden',
      '-Command',
      script(installer.absolute.path, processId, app),
    ], mode: ProcessStartMode.detached);
  }

  /// The PowerShell [handOff] runs. Paths go in single quotes, where only a
  /// single quote is special.
  static String script(String installer, int processId, String app) {
    String quote(String text) => "'${text.replaceAll("'", "''")}'";
    return [
      'try { Wait-Process -Id $processId -Timeout 60 -ErrorAction Stop } catch {}',
      'if (Get-Process -Id $processId -ErrorAction SilentlyContinue) { exit 1 }',
      'Start-Process -FilePath ${quote(installer)} '
          "-ArgumentList '/SILENT','/SUPPRESSMSGBOXES','/NORESTART','/SP-' -Wait",
      'Start-Process -FilePath ${quote(app)}',
    ].join('; ');
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

  static Future<void> replace(File download, String appImage, int pid) async {
    final target = File(appImage);
    // Staged beside the target, so the rename that replaces it is atomic.
    final staged = File(
      '${target.parent.path}/.${target.uri.pathSegments.last}.update',
    );
    try {
      await download.copy(staged.path);
      final chmod = await Process.run('chmod', ['755', staged.path]);
      if (chmod.exitCode != 0) {
        throw FileSystemException('chmod failed: ${chmod.stderr}', staged.path);
      }
      await staged.rename(target.path);
    } catch (_) {
      await _delete(staged);
      rethrow;
    } finally {
      await _delete(download);
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
