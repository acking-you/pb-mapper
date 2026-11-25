/// Putting a verified update in place, per platform.
///
/// By the time anything here runs, the file's size and SHA-256 have been
/// checked against a manifest signed by the release key. What is left is the
/// platform's own way of replacing a running app: macOS verifies and swaps its
/// `.app` bundle, Windows starts the installer (or unpacks a portable archive
/// over its folder), Linux swaps its AppImage or portable folder, and only an
/// installation in a folder this user cannot write to is saved to Downloads.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:url_launcher/url_launcher.dart';

/// The platform effects of an update, behind an interface so the flow can be
/// tested without replacing anything.
abstract interface class UpdateInstaller {
  /// Extract the app bundle from [diskImage], verify its Developer ID
  /// signature, notarization, bundle identifier and [version], and arrange for
  /// it to replace the running `.app` and reopen once this process exits.
  /// Throws a [FileSystemException] when the running bundle's location cannot
  /// be replaced by this user, leaving [diskImage] in place so the caller can
  /// save and open it instead.
  Future<void> replaceMacBundle(
    File diskImage, {
    required Version version,
    String? bundleId,
  });

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

  /// Unpack [archive] into a staging folder, check that it holds a complete
  /// portable bundle, and arrange for it to replace the running portable
  /// folder and reopen once this process exits. Throws a [FileSystemException]
  /// when the running folder cannot be replaced by this user, leaving
  /// [archive] in place so the caller can save it to Downloads instead.
  Future<void> replacePortable(File archive);

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
  Future<void> replaceMacBundle(
    File diskImage, {
    required Version version,
    String? bundleId,
  }) =>
      MacBundleUpdate.replace(diskImage, version: version, bundleId: bundleId);

  @override
  Future<void> startInstaller(File installer) =>
      WindowsInstallerUpdate.start(installer, pid);

  @override
  Future<void> replaceAppImage(File download, String appImage) =>
      AppImageUpdate.replace(download, appImage, pid);

  @override
  Future<void> replacePortable(File archive) =>
      PortableArchiveUpdate.replace(archive);

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
      // Applications when `/Applications` is not writable by this user.
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

Future<void> _delete(FileSystemEntity entity) async {
  try {
    if (await entity.exists()) await entity.delete(recursive: true);
  } catch (_) {
    // A leftover temporary file is not worth failing over.
  }
}

Future<ProcessResult> _runChecked(
  String executable,
  List<String> arguments, {
  UpdateFailure failure = UpdateFailure.verification,
}) async {
  final ProcessResult result;
  try {
    result = await Process.run(executable, arguments);
  } on ProcessException catch (error) {
    throw UpdateException(failure, '$executable: ${error.message}');
  }
  if (result.exitCode != 0) {
    final detail = '${result.stderr}'.trim();
    throw UpdateException(
      failure,
      detail.isEmpty
          ? '$executable ${arguments.join(' ')} exited with ${result.exitCode}'
          : '$executable ${arguments.join(' ')}: $detail',
    );
  }
  return result;
}

bool _probeDirectoryWritable(Directory directory) {
  final probe = File(
    '${directory.path}${Platform.pathSeparator}'
    '.pb-mapper-update-${DateTime.now().microsecondsSinceEpoch}-$pid',
  );
  try {
    probe.writeAsStringSync('');
    probe.deleteSync();
    return true;
  } catch (_) {
    return false;
  }
}

Future<void> _copyDirectory(Directory source, Directory destination) async {
  await destination.create(recursive: true);
  await for (final entry in source.list(followLinks: false)) {
    final name = entry.uri.pathSegments.where((s) => s.isNotEmpty).last;
    final targetPath = '${destination.path}${Platform.pathSeparator}$name';
    if (entry is Directory) {
      await _copyDirectory(entry, Directory(targetPath));
    } else if (entry is File) {
      await entry.copy(targetPath);
    }
  }
}

/// macOS: the new bundle comes out of the signed, notarized disk image, is
/// checked against the Developer ID team and Gatekeeper, and replaces the
/// running `.app` once the app has quit (after proxy-everything's
/// `MacBundleUpdate`).
abstract final class MacBundleUpdate {
  /// The Developer ID team every macOS release is signed by.
  static const String teamId = '5T7QB722FM';
  static const String defaultBundleId = 'com.pb-mapper.ui';

  /// The `.app` bundle this process runs from.
  static Directory currentBundle([String? executable]) =>
      File(executable ?? Platform.resolvedExecutable).parent.parent.parent;

  /// Whether this user may replace [bundle] in place without elevation.
  /// Never writes inside [bundle], which would disturb its sealed signature.
  static Future<bool> canReplace(Directory bundle) async {
    if (!bundle.path.endsWith('.app') ||
        bundle.path.contains('/AppTranslocation/') ||
        !Directory('${bundle.path}/Contents/MacOS').existsSync()) {
      return false;
    }
    if (!_probeDirectoryWritable(bundle.parent)) return false;
    try {
      final writable = await Process.run('/bin/test', ['-w', bundle.path]);
      return writable.exitCode == 0;
    } on ProcessException {
      return false;
    }
  }

  /// Mount [image] quietly, copy its `.app` into [work], and return it only if
  /// [verifyBundle] accepts its signature, notarization, [bundleId] and
  /// [version].
  static Future<Directory> stage(
    File image,
    Directory work, {
    required Version version,
    String? bundleId,
  }) async {
    final mount = Directory('${work.path}/mount');
    await mount.create(recursive: true);
    await _runChecked('/usr/bin/hdiutil', [
      'attach',
      '-nobrowse',
      '-readonly',
      '-noautoopen',
      '-mountpoint',
      mount.path,
      image.path,
    ]);
    try {
      final apps = mount
          .listSync(followLinks: false)
          .whereType<Directory>()
          .where((entry) => entry.path.endsWith('.app'))
          .toList();
      if (apps.length != 1) {
        throw const UpdateException(
          UpdateFailure.verification,
          'The disk image does not hold exactly one application',
        );
      }
      final name = apps.single.uri.pathSegments
          .where((segment) => segment.isNotEmpty)
          .last;
      final staged = Directory('${work.path}/$name');
      await _delete(staged);
      await _runChecked('/usr/bin/ditto', [
        apps.single.path,
        staged.path,
      ], failure: UpdateFailure.install);
      await verifyBundle(
        staged,
        version: version,
        bundleId: (bundleId == null || bundleId.isEmpty)
            ? defaultBundleId
            : bundleId,
      );
      return staged;
    } finally {
      await Process.run('/usr/bin/hdiutil', ['detach', mount.path, '-force']);
    }
  }

  /// Fail unless [app] is intact, signed by [teamId] with Developer ID,
  /// accepted by Gatekeeper (notarized), and matches [bundleId] and [version].
  static Future<void> verifyBundle(
    Directory app, {
    required Version version,
    String bundleId = defaultBundleId,
  }) async {
    await _runChecked('/usr/bin/codesign', [
      '--verify',
      '--deep',
      '--strict',
      app.path,
    ]);
    final details = await _runChecked('/usr/bin/codesign', [
      '-dv',
      '--verbose=4',
      app.path,
    ]);
    final signature = '${details.stdout}\n${details.stderr}';
    if (!signature.contains('TeamIdentifier=$teamId') ||
        !signature.contains('Authority=Developer ID Application:')) {
      throw const UpdateException(
        UpdateFailure.verification,
        'The application is not signed by the pb-mapper Developer ID team',
      );
    }
    await _runChecked('/usr/sbin/spctl', [
      '--assess',
      '--type',
      'execute',
      app.path,
    ]);
    final plist = '${app.path}/Contents/Info.plist';
    final id = await _runChecked('/usr/bin/plutil', [
      '-extract',
      'CFBundleIdentifier',
      'raw',
      plist,
    ]);
    final shortVersion = await _runChecked('/usr/bin/plutil', [
      '-extract',
      'CFBundleShortVersionString',
      'raw',
      plist,
    ]);
    final executable = await _runChecked('/usr/bin/plutil', [
      '-extract',
      'CFBundleExecutable',
      'raw',
      plist,
    ]);
    final actualId = '${id.stdout}'.trim();
    final actualVersion = Version.tryParse('${shortVersion.stdout}'.trim());
    final exeName = '${executable.stdout}'.trim();
    if (actualId != bundleId || actualVersion != version) {
      throw UpdateException(
        UpdateFailure.verification,
        'Expected $bundleId $version, found $actualId '
        '${'${shortVersion.stdout}'.trim()}',
      );
    }
    if (exeName.isEmpty ||
        exeName.contains('/') ||
        exeName.contains('..') ||
        !File('${app.path}/Contents/MacOS/$exeName').existsSync()) {
      throw UpdateException(
        UpdateFailure.verification,
        'Bundle executable $exeName is missing',
      );
    }
  }

  /// Verify and stage the bundle from [diskImage] beside [bundle] (the running
  /// `.app`), then start the swap helper. Throws [FileSystemException] when
  /// [bundle] cannot be replaced in place, leaving [diskImage] untouched for
  /// the caller to save and open.
  static Future<void> replace(
    File diskImage, {
    required Version version,
    String? bundleId,
    Directory? bundle,
    int? processId,
    bool reopen = true,
  }) async {
    final target = bundle ?? currentBundle();
    final ownerPid = processId ?? pid;
    final staged = await stage(
      diskImage,
      diskImage.parent,
      version: version,
      bundleId: bundleId,
    );
    if (!await canReplace(target)) {
      await _delete(staged);
      throw FileSystemException(
        'Application bundle is not writable',
        target.path,
      );
    }
    final targetName = target.uri.pathSegments
        .where((segment) => segment.isNotEmpty)
        .last;
    final beside = Directory(
      '${target.parent.path}/.$targetName.update-$ownerPid',
    );
    await _delete(beside);
    try {
      try {
        await staged.rename(beside.path);
      } on FileSystemException {
        // Cross-volume staging (e.g. an app kept on an external drive): copy
        // with ditto so permissions, symlinks and extended attributes survive.
        final copy = await Process.run('/usr/bin/ditto', [
          staged.path,
          beside.path,
        ]);
        if (copy.exitCode != 0) {
          throw FileSystemException(
            'ditto failed: ${copy.stderr}'.trim(),
            beside.path,
          );
        }
        await _delete(staged);
      }
    } catch (_) {
      await _delete(staged);
      await _delete(beside);
      rethrow;
    }
    await handOff(beside, target, ownerPid, reopen: reopen);
  }

  /// Start a detached helper that waits for [processId] to exit, swaps
  /// [staged] in for [target] (putting the previous bundle back if the second
  /// move fails), and opens [target] again when [reopen] is true.
  static Future<void> handOff(
    Directory staged,
    Directory target,
    int processId, {
    bool reopen = true,
  }) async {
    await Process.start('/bin/sh', [
      '-c',
      _macScript,
      'pb-mapper-update',
      '$processId',
      target.path,
      staged.path,
      reopen ? '1' : '0',
    ], mode: ProcessStartMode.detached);
  }

  static const String _macScript = r'''
pid="$1"
target="$2"
staged="$3"
reopen="$4"
i=0
while kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -ge 600 ]; then
    rm -rf "$staged"
    exit 1
  fi
  sleep 0.2
done
previous="$target.previous-$$"
if mv "$target" "$previous"; then
  if mv "$staged" "$target"; then
    rm -rf "$previous"
    if [ "$reopen" = "1" ]; then
      /usr/bin/open "$target"
    fi
    exit 0
  fi
  mv "$previous" "$target"
fi
rm -rf "$staged"
exit 1
''';
}

/// Which portable bundle layout an archive carries.
enum PortableLayout { windows, linux }

/// Portable Windows and Linux archives (`.zip`): unpacked into a staging
/// folder, checked for the bundle's required files, and copied over the
/// running folder once this process exits.
abstract final class PortableArchiveUpdate {
  static PortableLayout _defaultLayout() =>
      Platform.isWindows ? PortableLayout.windows : PortableLayout.linux;

  /// Whether [target] looks like a portable bundle of [layout] that this user
  /// may overwrite in place.
  static Future<bool> canReplace(
    Directory target,
    File executable, {
    PortableLayout? layout,
  }) async {
    final kind = layout ?? _defaultLayout();
    final sep = Platform.pathSeparator;
    final looksLikeBundle = switch (kind) {
      PortableLayout.windows =>
        File('${target.path}${sep}pb_mapper_ffi.dll').existsSync() &&
            File('${target.path}${sep}flutter_windows.dll').existsSync(),
      PortableLayout.linux => File(
        '${target.path}${sep}lib${sep}libpb_mapper_ffi.so',
      ).existsSync(),
    };
    if (!looksLikeBundle || !_probeDirectoryWritable(target)) return false;
    if (!Platform.isWindows) {
      try {
        final writable = await Process.run('/bin/test', [
          '-w',
          executable.path,
        ]);
        if (writable.exitCode != 0) return false;
      } on ProcessException {
        return false;
      }
    }
    return true;
  }

  /// Extract [archive] inside [work] and check that it holds a complete
  /// portable bundle of [layout] with no symbolic links.
  static Future<Directory> stage(
    File archive,
    Directory work, {
    PortableLayout? layout,
  }) async {
    final kind = layout ?? _defaultLayout();
    final sep = Platform.pathSeparator;
    final extracted = Directory('${work.path}${sep}extracted');
    await _delete(extracted);
    await extracted.create(recursive: true);
    await _extractZip(archive, extracted);

    for (final entry in extracted.listSync(
      recursive: true,
      followLinks: false,
    )) {
      if (entry is Link) {
        throw const UpdateException(
          UpdateFailure.verification,
          'Portable archive contains an unexpected symbolic link',
        );
      }
    }

    var root = extracted;
    final topEntries = extracted.listSync(followLinks: false);
    if (topEntries.length == 1 && topEntries.single is Directory) {
      root = topEntries.single as Directory;
    }

    switch (kind) {
      case PortableLayout.windows:
        for (final required in const [
          'pb_mapper_ui.exe',
          'pb_mapper_ffi.dll',
          'flutter_windows.dll',
          'data/app.so',
        ]) {
          if (!File(
            '${root.path}$sep${required.replaceAll('/', sep)}',
          ).existsSync()) {
            throw UpdateException(
              UpdateFailure.verification,
              'Portable archive is missing $required',
            );
          }
        }
      case PortableLayout.linux:
        final uiBinary = File('${root.path}${sep}ui');
        final ffiLib = File('${root.path}${sep}lib${sep}libpb_mapper_ffi.so');
        final dataDir = Directory('${root.path}${sep}data');
        if (!uiBinary.existsSync() ||
            !ffiLib.existsSync() ||
            !dataDir.existsSync()) {
          throw const UpdateException(
            UpdateFailure.verification,
            'Portable archive is missing required Linux bundle files',
          );
        }
        if (!Platform.isWindows) {
          await _runChecked('chmod', [
            '755',
            uiBinary.path,
          ], failure: UpdateFailure.install);
        }
    }
    return root;
  }

  static Future<void> _extractZip(File archive, Directory destination) async {
    if (Platform.isWindows) {
      // `detachedWithStdio` uses `DETACHED_PROCESS` so a GUI parent never
      // flashes a console window while extracting.
      final Process proc;
      try {
        proc = await Process.start(
          'powershell.exe',
          [
            '-NoProfile',
            '-NonInteractive',
            '-Command',
            r"$ErrorActionPreference = 'Stop'; "
                r'Add-Type -AssemblyName System.IO.Compression.FileSystem; '
                r'[System.IO.Compression.ZipFile]::ExtractToDirectory($env:PB_ZIP, $env:PB_DST)',
          ],
          environment: {'PB_ZIP': archive.path, 'PB_DST': destination.path},
          mode: ProcessStartMode.detachedWithStdio,
        );
      } on ProcessException catch (error) {
        throw UpdateException(
          UpdateFailure.install,
          'Could not unpack archive: ${error.message}',
        );
      }
      final stdoutFuture = utf8.decodeStream(proc.stdout);
      final stderrFuture = utf8.decodeStream(proc.stderr);
      final code = await proc.exitCode;
      await stdoutFuture;
      final stderr = (await stderrFuture).trim();
      if (code != 0) {
        throw UpdateException(
          UpdateFailure.verification,
          stderr.isEmpty
              ? 'Could not unpack archive (exit code $code)'
              : stderr,
        );
      }
      return;
    }

    try {
      final unzip = await Process.run('unzip', [
        '-q',
        archive.path,
        '-d',
        destination.path,
      ]);
      if (unzip.exitCode == 0) return;
    } on ProcessException {
      // Fall through to Python's standard-library zip extractor.
    }
    await _runChecked('python3', [
      '-c',
      _pythonUnzip,
      archive.path,
      destination.path,
    ]);
  }

  static const String _pythonUnzip = '''
import os, pathlib, sys, zipfile
src, dst = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]).resolve()
with zipfile.ZipFile(src) as zf:
    for info in zf.infolist():
        target = (dst / info.filename).resolve()
        if dst not in target.parents and target != dst:
            raise SystemExit(f"Unsafe archive entry: {info.filename}")
        if info.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with zf.open(info) as r, target.open("wb") as w:
                while chunk := r.read(65536):
                    w.write(chunk)
            mode = (info.external_attr >> 16) & 0o777
            if mode:
                os.chmod(target, mode)
''';

  /// Unpack and verify [archive], stage it inside the running portable folder,
  /// and start the replacement helper. Throws [FileSystemException] when the
  /// running folder is not writable, leaving [archive] untouched to save.
  static Future<void> replace(
    File archive, {
    File? executable,
    int? processId,
    PortableLayout? layout,
    bool reopen = true,
  }) async {
    final kind = layout ?? _defaultLayout();
    final exe = executable ?? File(Platform.resolvedExecutable);
    final target = exe.parent;
    final ownerPid = processId ?? pid;
    final sep = Platform.pathSeparator;
    final staged = await stage(archive, archive.parent, layout: kind);
    if (!await canReplace(target, exe, layout: kind)) {
      await _delete(staged.parent);
      throw FileSystemException(
        'Portable directory is not writable',
        target.path,
      );
    }

    final defaultExeName = switch (kind) {
      PortableLayout.windows => 'pb_mapper_ui.exe',
      PortableLayout.linux => 'ui',
    };
    final runningExeName = exe.uri.pathSegments
        .where((segment) => segment.isNotEmpty)
        .last;
    if (runningExeName != defaultExeName) {
      final bundledExe = File('${staged.path}$sep$defaultExeName');
      if (bundledExe.existsSync()) {
        await bundledExe.rename('${staged.path}$sep$runningExeName');
      }
    }

    final inPlace = Directory('${target.path}$sep.pb-mapper-update-$ownerPid');
    await _delete(inPlace);
    try {
      try {
        await staged.rename(inPlace.path);
      } on FileSystemException {
        await _copyDirectory(staged, inPlace);
        await _delete(staged);
      }
    } catch (_) {
      await _delete(staged);
      await _delete(inPlace);
      rethrow;
    }
    await handOff(inPlace, target, exe, ownerPid, reopen: reopen);
  }

  /// Start a detached helper that waits for [processId] to exit, copies
  /// [staged]'s files over [target], removes [staged], and opens [executable]
  /// when [reopen] is true.
  static Future<void> handOff(
    Directory staged,
    Directory target,
    File executable,
    int processId, {
    bool reopen = true,
  }) async {
    if (Platform.isWindows) {
      await Process.start(
        'powershell.exe',
        ['-NoProfile', '-NonInteractive', '-Command', _windowsPortableScript],
        environment: {
          'PB_PID': '$processId',
          'PB_TARGET': target.path,
          'PB_STAGED': staged.path,
          'PB_EXE': executable.path,
          'PB_REOPEN': reopen ? '1' : '0',
        },
        mode: ProcessStartMode.detached,
      );
      return;
    }
    await Process.start('/bin/sh', [
      '-c',
      _unixPortableScript,
      'pb-mapper-update',
      '$processId',
      target.path,
      staged.path,
      executable.path,
      reopen ? '1' : '0',
    ], mode: ProcessStartMode.detached);
  }

  static const String _windowsPortableScript = r'''
$ErrorActionPreference = 'Stop'
$targetPid = [int]$env:PB_PID
$target = $env:PB_TARGET
$staged = $env:PB_STAGED
$exe = $env:PB_EXE
$reopen = $env:PB_REOPEN -eq '1'
try {
  $proc = Get-Process -Id $targetPid -ErrorAction SilentlyContinue
  if ($null -ne $proc) {
    if (-not $proc.WaitForExit(120000)) {
      Remove-Item -LiteralPath $staged -Recurse -Force -ErrorAction SilentlyContinue
      exit 1
    }
  }
  $copied = $false
  for ($attempt = 0; $attempt -lt 25; $attempt++) {
    try {
      Get-ChildItem -LiteralPath $staged -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $target -Recurse -Force
      }
      $copied = $true
      break
    } catch {
      Start-Sleep -Milliseconds 200
    }
  }
  Remove-Item -LiteralPath $staged -Recurse -Force -ErrorAction SilentlyContinue
  if ($copied -and $reopen) {
    Start-Process -FilePath $exe -WorkingDirectory $target
  }
  if (-not $copied) { exit 1 }
} catch {
  Remove-Item -LiteralPath $staged -Recurse -Force -ErrorAction SilentlyContinue
  exit 1
}
''';

  static const String _unixPortableScript = r'''
pid="$1"
target="$2"
staged="$3"
exe="$4"
reopen="$5"
i=0
while kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -ge 600 ]; then
    rm -rf "$staged"
    exit 1
  fi
  sleep 0.2
done
if cp -Rf "$staged"/. "$target"/; then
  rm -rf "$staged"
  if [ "$reopen" = "1" ]; then
    exec "$exe"
  fi
  exit 0
fi
rm -rf "$staged"
exit 1
''';
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
  /// executable. A protected copy under `pb-mapper-desktop\release-*` (with
  /// `installation.json` beside it) counts too, even if originally set up from
  /// a portable folder: that folder is read-only to the user and updates
  /// through Setup.
  static bool isInstalled([String? executable]) {
    final dir = File(executable ?? Platform.resolvedExecutable).parent;
    final sep = Platform.pathSeparator;
    if (File('${dir.path}${sep}unins000.exe').existsSync()) return true;
    final dirSegments = dir.uri.pathSegments
        .where((segment) => segment.isNotEmpty)
        .toList();
    final parent = dir.parent;
    final parentSegments = parent.uri.pathSegments
        .where((segment) => segment.isNotEmpty)
        .toList();
    final dirName = dirSegments.isEmpty ? null : dirSegments.last;
    final parentName = parentSegments.isEmpty ? null : parentSegments.last;
    return dirName != null &&
        dirName.startsWith('release-') &&
        parentName?.toLowerCase() == 'pb-mapper-desktop' &&
        File('${parent.path}${sep}installation.json').existsSync();
  }

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
      r'i=0; while kill -0 "$1" 2>/dev/null; do '
          r'i=$((i + 1)); [ "$i" -ge 600 ] && exit 1; sleep 0.2; '
          r'done; exec "$2"',
      'pb-mapper-update',
      '$pid',
      appImage,
    ], mode: ProcessStartMode.detached);
  }
}
