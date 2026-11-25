// The updater's real I/O: a socket, a disk, archives and swap helpers. Kept
// apart from update_test.dart, whose widget tests make the harness answer
// every HTTP request with a 400 for the whole file.
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:pb_mapper_ui/src/updates/update_installers.dart';
import 'package:pb_mapper_ui/src/updates/update_source.dart';

Future<void> _writeZip(Directory source, File archive) async {
  if (Platform.isWindows) {
    final proc = await Process.start(
      'powershell.exe',
      [
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        r"$ErrorActionPreference = 'Stop'; "
            r'Add-Type -AssemblyName System.IO.Compression.FileSystem; '
            r'[System.IO.Compression.ZipFile]::CreateFromDirectory($env:PB_SRC, $env:PB_ZIP)',
      ],
      environment: {'PB_SRC': source.path, 'PB_ZIP': archive.path},
      mode: ProcessStartMode.detachedWithStdio,
    );
    expect(await proc.exitCode, 0);
    return;
  }
  final result = await Process.run('python3', [
    '-c',
    '''
import pathlib, sys, zipfile
src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
with zipfile.ZipFile(out, 'w') as zf:
    for p in sorted(src.rglob('*')):
        zf.write(p, p.relative_to(src))
''',
    source.path,
    archive.path,
  ]);
  expect(result.exitCode, 0, reason: '${result.stderr}');
}

void main() {
  late HttpServer server;
  late Directory work;
  late List<int> body;

  setUp(() async {
    body = List<int>.generate(300000, (i) => i * 7 % 251);
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    server.listen((request) async {
      final extra = request.uri.path == '/longer' ? [1, 2, 3] : <int>[];
      request.response.add([...body, ...extra]);
      await request.response.close();
    });
    work = await Directory.systemTemp.createTemp('pb-mapper-update-test-');
  });

  tearDown(() async {
    await server.close(force: true);
    for (var attempt = 0; attempt < 20; attempt++) {
      try {
        await work.delete(recursive: true);
        return;
      } on FileSystemException {
        await Future<void>.delayed(const Duration(milliseconds: 200));
      }
    }
  });

  Uri at(String path) => Uri.http('127.0.0.1:${server.port}', path);

  Matcher failsWith(UpdateFailure kind) =>
      throwsA(isA<UpdateException>().having((e) => e.kind, 'kind', kind));

  test('a download is hashed as written', () async {
    final target = File('${work.path}/package');
    final digest = await IoUpdateHttp().download(
      at('/package'),
      target,
      size: body.length,
      onProgress: (_) {},
    );
    expect(digest, sha256.convert(body).toString());
    expect(await target.readAsBytes(), body);
  });

  test('a file that cannot be written is not vouched for', () async {
    await expectLater(
      IoUpdateHttp().download(
        at('/package'),
        File('${work.path}/missing/package'),
        size: body.length,
        onProgress: (_) {},
      ),
      failsWith(UpdateFailure.install),
    );
  });

  test('more bytes than the manifest lists are refused', () async {
    await expectLater(
      IoUpdateHttp().download(
        at('/longer'),
        File('${work.path}/package'),
        size: body.length,
        onProgress: (_) {},
      ),
      failsWith(UpdateFailure.checksum),
    );
  });

  test('an AppImage it cannot replace leaves the download to save', () async {
    final download = File('${work.path}/pb-mapper-linux.AppImage')
      ..writeAsBytesSync(body);
    await expectLater(
      AppImageUpdate.replace(
        download,
        '${work.path}/not-a-folder/pb-mapper.AppImage',
        pid,
      ),
      throwsA(isA<FileSystemException>()),
    );
    expect(download.existsSync(), isTrue);
  });

  test(
    'MacBundleUpdate swaps the app bundle only after the running process exits',
    () async {
      final root = Directory('${work.path}/Applications')..createSync();
      final target = Directory('${root.path}/pb-mapper.app')..createSync();
      File('${target.path}/marker').writeAsStringSync('old');
      final staged = Directory('${root.path}/.pb-mapper.app.update-test')
        ..createSync();
      File('${staged.path}/marker').writeAsStringSync('new');

      final running = await Process.start('/bin/sleep', ['1']);
      await MacBundleUpdate.handOff(staged, target, running.pid, reopen: false);
      await Future<void>.delayed(const Duration(milliseconds: 350));
      expect(File('${target.path}/marker').readAsStringSync(), 'old');

      await running.exitCode;
      for (var i = 0; i < 40 && staged.existsSync(); i++) {
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
      expect(File('${target.path}/marker').readAsStringSync(), 'new');
      expect(staged.existsSync(), isFalse);
      expect(
        root
            .listSync()
            .map((e) => e.path)
            .where((p) => p.contains('.previous')),
        isEmpty,
      );
    },
    skip: !Platform.isMacOS ? 'macOS bundle hand-off test' : false,
  );

  test(
    'MacBundleUpdate rejects an unsigned bundle before replacing anything',
    () async {
      final fake = Directory('${work.path}/pb-mapper.app');
      Directory('${fake.path}/Contents/MacOS').createSync(recursive: true);
      File(
        '${fake.path}/Contents/MacOS/pb-mapper',
      ).writeAsStringSync('#!/bin/sh\n');
      File('${fake.path}/Contents/Info.plist').writeAsStringSync('''
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>com.pb-mapper.ui</string>
  <key>CFBundleShortVersionString</key>
  <string>0.14.0</string>
  <key>CFBundleExecutable</key>
  <string>pb-mapper</string>
</dict>
</plist>
''');
      await expectLater(
        MacBundleUpdate.verifyBundle(fake, version: const Version(0, 14, 0)),
        failsWith(UpdateFailure.verification),
      );
    },
    skip: !Platform.isMacOS ? 'macOS codesign/Gatekeeper test' : false,
  );

  test(
    'MacBundleUpdate accepts a real Developer ID release and rejects wrong metadata',
    () async {
      final installed = Directory('/Applications/pb-mapper.app');
      final plist = await Process.run('/usr/bin/plutil', [
        '-extract',
        'CFBundleShortVersionString',
        'raw',
        '${installed.path}/Contents/Info.plist',
      ]);
      final version = Version.tryParse('${plist.stdout}'.trim())!;
      await MacBundleUpdate.verifyBundle(installed, version: version);
      await expectLater(
        MacBundleUpdate.verifyBundle(
          installed,
          version: Version(version.major, version.minor, version.patch + 1),
        ),
        failsWith(UpdateFailure.verification),
      );
      await expectLater(
        MacBundleUpdate.verifyBundle(
          installed,
          version: version,
          bundleId: 'com.other.app',
        ),
        failsWith(UpdateFailure.verification),
      );
    },
    skip:
        !Platform.isMacOS ||
            !Directory('/Applications/pb-mapper.app').existsSync()
        ? 'no Developer ID signed /Applications/pb-mapper.app on this host'
        : false,
  );

  test(
    'PortableArchiveUpdate unpacks, verifies and replaces files after process exit',
    () async {
      final sep = Platform.pathSeparator;
      final layout = Platform.isWindows
          ? PortableLayout.windows
          : PortableLayout.linux;
      final bundleSrc = Directory('${work.path}${sep}bundle-src')..createSync();
      final installedDir = Directory('${work.path}${sep}installed')
        ..createSync();

      late final File runningExe;
      if (layout == PortableLayout.windows) {
        Directory('${bundleSrc.path}${sep}data').createSync();
        File(
          '${bundleSrc.path}${sep}pb_mapper_ui.exe',
        ).writeAsStringSync('new-exe');
        File(
          '${bundleSrc.path}${sep}pb_mapper_ffi.dll',
        ).writeAsStringSync('new-ffi');
        File(
          '${bundleSrc.path}${sep}flutter_windows.dll',
        ).writeAsStringSync('new-flutter');
        File(
          '${bundleSrc.path}${sep}data${sep}app.so',
        ).writeAsStringSync('new-app');

        Directory('${installedDir.path}${sep}data').createSync();
        runningExe = File('${installedDir.path}${sep}pb_mapper_ui.exe')
          ..writeAsStringSync('old-exe');
        File(
          '${installedDir.path}${sep}pb_mapper_ffi.dll',
        ).writeAsStringSync('old-ffi');
        File(
          '${installedDir.path}${sep}flutter_windows.dll',
        ).writeAsStringSync('old-flutter');
        File(
          '${installedDir.path}${sep}data${sep}app.so',
        ).writeAsStringSync('old-app');
      } else {
        Directory('${bundleSrc.path}${sep}lib').createSync();
        Directory('${bundleSrc.path}${sep}data').createSync();
        File('${bundleSrc.path}${sep}ui').writeAsStringSync('new-exe');
        File(
          '${bundleSrc.path}${sep}lib${sep}libpb_mapper_ffi.so',
        ).writeAsStringSync('new-ffi');
        File(
          '${bundleSrc.path}${sep}data${sep}icudtl.dat',
        ).writeAsStringSync('new-icu');

        Directory('${installedDir.path}${sep}lib').createSync();
        Directory('${installedDir.path}${sep}data').createSync();
        runningExe = File('${installedDir.path}${sep}ui')
          ..writeAsStringSync('old-exe');
        File(
          '${installedDir.path}${sep}lib${sep}libpb_mapper_ffi.so',
        ).writeAsStringSync('old-ffi');
      }
      File('${installedDir.path}${sep}user-note.txt').writeAsStringSync('keep');

      final archiveDir = Directory('${work.path}${sep}download')..createSync();
      final archive = File('${archiveDir.path}${sep}pb-mapper.zip');
      await _writeZip(bundleSrc, archive);

      final child = Platform.isWindows
          ? await Process.start('powershell.exe', [
              '-NoProfile',
              '-NonInteractive',
              '-Command',
              'Start-Sleep -Seconds 1',
            ], mode: ProcessStartMode.detachedWithStdio)
          : await Process.start('/bin/sleep', ['1']);

      await PortableArchiveUpdate.replace(
        archive,
        executable: runningExe,
        processId: child.pid,
        layout: layout,
        reopen: false,
      );
      await Future<void>.delayed(const Duration(milliseconds: 300));
      expect(runningExe.readAsStringSync(), 'old-exe');

      await child.exitCode;
      for (
        var i = 0;
        i < 40 &&
            (runningExe.readAsStringSync() != 'new-exe' ||
                installedDir.listSync().any(
                  (e) => e.path.contains('.pb-mapper-update-'),
                ));
        i++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
      expect(runningExe.readAsStringSync(), 'new-exe');
      expect(
        File('${installedDir.path}${sep}user-note.txt').readAsStringSync(),
        'keep',
      );
      expect(
        installedDir.listSync().where(
          (e) => e.path.contains('.pb-mapper-update-'),
        ),
        isEmpty,
      );
    },
  );

  test('PortableArchiveUpdate rejects an incomplete archive', () async {
    final sep = Platform.pathSeparator;
    final incomplete = Directory('${work.path}${sep}incomplete')..createSync();
    File(
      '${incomplete.path}${sep}README.txt',
    ).writeAsStringSync('not a bundle');
    final archiveDir = Directory('${work.path}${sep}bad-download')
      ..createSync();
    final archive = File('${archiveDir.path}${sep}bad.zip');
    await _writeZip(incomplete, archive);

    await expectLater(
      PortableArchiveUpdate.stage(archive, archiveDir),
      failsWith(UpdateFailure.verification),
    );
  });
}
