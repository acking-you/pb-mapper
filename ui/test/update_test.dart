import 'dart:convert';
import 'dart:ffi' show DynamicLibrary;
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';
import 'package:pb_mapper_ui/src/updates/update_installers.dart';
import 'package:pb_mapper_ui/src/updates/update_record.dart';
import 'package:pb_mapper_ui/src/updates/update_schedule.dart';
import 'package:pb_mapper_ui/src/updates/update_source.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

const _site = 'https://github.com/acking-you/pb-mapper';
Uri _file(String tag, String name) =>
    Uri.parse('$_site/releases/download/$tag/$name');

Map<String, Object?> _release(
  String tag,
  List<String> names, {
  bool draft = false,
  bool prerelease = false,
}) => {
  'tag_name': tag,
  'draft': draft,
  'prerelease': prerelease,
  'html_url': '$_site/releases/tag/$tag',
  'assets': [
    for (final name in names)
      {'name': name, 'browser_download_url': '${_file(tag, name)}'},
  ],
};

/// GitHub, as far as the updater sees it.
class _FakeHttp implements UpdateHttp {
  final Map<Uri, List<int>> bodies = {};
  final Map<Uri, List<int>> files = {};
  final List<Uri> downloaded = [];
  bool cancelled = false;

  @override
  Future<List<int>> get(Uri url, {int limit = 1 << 20}) async {
    final body = bodies[url];
    if (body == null) {
      throw UpdateException(UpdateFailure.network, 'HTTP 404 from $url');
    }
    return body;
  }

  @override
  Future<String> download(
    Uri url,
    File target, {
    required int size,
    required void Function(int received) onProgress,
  }) async {
    downloaded.add(url);
    final body = files[url]!;
    await target.writeAsBytes(body);
    onProgress(body.length);
    return sha256.convert(body).toString();
  }

  @override
  void cancel() => cancelled = true;
}

class _FakeVerifier implements SignatureVerifier {
  bool valid = true;
  List<int>? signed;

  @override
  bool verify(List<int> message, List<int> signature, List<int> publicKey) {
    signed = message;
    return valid;
  }
}

class _FakeInstaller implements UpdateInstaller {
  final List<String> calls = [];
  Object? appImageError;
  Object? installerError;
  Object? androidError;

  @override
  Future<void> runInstallerAfterExit(File installer) async {
    if (installerError case final error?) throw error;
    calls.add('installer ${installer.uri.pathSegments.last}');
  }

  @override
  Future<void> replaceAppImage(File download, String appImage) async {
    if (appImageError case final error?) throw error;
    calls.add('appimage $appImage');
  }

  @override
  Future<String> saveToDownloads(File download, String name) async {
    calls.add('save $name');
    return '/downloads/$name';
  }

  @override
  Future<void> installAndroidPackage(File package) async {
    calls.add('android ${package.uri.pathSegments.last}');
    if (androidError case final error?) throw error;
  }
}

/// The note an update leaves for the next launch, kept in memory.
class _MemoryRecord implements UpdateRecord {
  PendingUpdate? pending;

  @override
  Future<void> write(PendingUpdate pending) async => this.pending = pending;

  @override
  Future<PendingUpdate?> take() async {
    final taken = pending;
    pending = null;
    return taken;
  }
}

/// A newest release `v[version]-ui` carrying [packages], signed or not, over
/// older releases and a CLI release that must be ignored.
class _World {
  _World({
    String version = '0.9.1',
    Map<String, List<int>>? packages,
    bool signed = true,
    String? manifestVersion,
    List<int>? corrupt,
  }) {
    final tag = 'v$version-ui';
    this.packages =
        packages ??
        {
          'pb-mapper-windows-x86_64-setup.exe': utf8.encode('setup'),
          'pb-mapper-windows-x86_64.zip': utf8.encode('portable'),
          'pb-mapper-linux.AppImage': utf8.encode('appimage'),
          'pb-mapper-macos-arm64.dmg': utf8.encode('dmg'),
          'pb-mapper-arm64-v8a.apk': utf8.encode('apk'),
        };
    manifest = utf8.encode(
      jsonEncode({
        'version': manifestVersion ?? version,
        'assets': [
          for (final MapEntry(key: name, value: body) in this.packages.entries)
            {
              'name': name,
              'size': body.length,
              'sha256': sha256.convert(body).toString(),
            },
        ],
      }),
    );
    final names = [
      ...this.packages.keys,
      if (signed) ...[manifestName, signatureName],
    ];
    http.bodies[UpdateSource.releasesUrl] = utf8.encode(
      jsonEncode([
        _release('v0.10.0-ui', names, draft: true),
        _release('v$version', ['pb-mapper-x86_64-pc-windows-msvc.zip']),
        _release(tag, names),
        _release('v0.9.0-ui', ['pb-mapper-windows-x86_64.zip']),
      ]),
    );
    http.bodies[_file(tag, manifestName)] = manifest;
    http.bodies[_file(tag, signatureName)] = utf8.encode('c2lnbmVk\n');
    for (final MapEntry(key: name, value: body) in this.packages.entries) {
      http.files[_file(tag, name)] = corrupt ?? body;
    }
    addTearDown(() => downloads.deleteSync(recursive: true));
  }

  /// Where this world's downloads land, rather than the real temp folder:
  /// a handed-off installer's folder is never removed by the app.
  final downloads = Directory.systemTemp.createTempSync('pb-mapper-test-');
  final http = _FakeHttp();
  final verifier = _FakeVerifier();
  final installer = _FakeInstaller();
  final record = _MemoryRecord();
  late final Map<String, List<int>> packages;
  late final List<int> manifest;
  int quits = 0;

  UpdateController controller({
    UpdateHost host = const UpdateHost(
      os: 'windows',
      arch: 'x64',
      windowsInstalled: true,
    ),
    String installed = '0.9.0',
    bool replaceSelf = true,
  }) => UpdateController(
    quit: () async => quits++,
    source: UpdateSource(http: http, verifier: verifier),
    installer: installer,
    record: record,
    loadInstalled: () async => Version.tryParse(installed)!,
    loadHost: () async => host,
    workDir: () => downloads.createTemp('update-'),
    replaceSelf: replaceSelf,
  );
}

void main() {
  group('versions and releases', () {
    test('a version reads x.y.z and ignores the build', () {
      expect(Version.tryParse('0.9.0+900'), const Version(0, 9, 0));
      expect(Version.tryParse('v0.9.0'), isNull);
      expect(Version.tryParse('0.9'), isNull);
      expect(const Version(0, 10, 0) > const Version(0, 9, 9), isTrue);
      expect(const Version(1, 0, 0) > const Version(1, 0, 0), isFalse);
    });

    test('the newest UI release skips drafts, prereleases and the CLI', () {
      final newest = PublishedRelease.newest([
        _release('v0.12.0', []),
        _release('v0.11.0-ui', [], draft: true),
        _release('v0.10.5-ui', [], prerelease: true),
        _release('v0.9.1-ui', ['a']),
        _release('v0.10.0-ui', ['b']),
        _release('v0.9.9-ui', ['c']),
      ]);
      expect(newest?.version, const Version(0, 10, 0));
      expect(newest?.files.keys, ['b']);
    });

    test('each platform takes its own package', () {
      final manifest = ReleaseManifest(
        version: const Version(0, 9, 1),
        assets: [
          for (final name in [
            'pb-mapper-windows-x86_64-setup.exe',
            'pb-mapper-windows-x86_64.zip',
            'pb-mapper-linux.AppImage',
            'pb-mapper-linux.zip',
            'pb-mapper-macos-arm64.dmg',
            'pb-mapper-macos-x86_64.dmg',
            'pb-mapper-arm64-v8a.apk',
            'pb-mapper-armeabi-v7a.apk',
          ])
            ReleaseAsset(name: name, size: 1, sha256: '0' * 64),
        ],
      );
      String? plan(UpdateHost host) {
        final plan = planUpdate(manifest, host);
        return plan == null ? null : '${plan.asset.name} ${plan.method.name}';
      }

      expect(
        plan(
          const UpdateHost(os: 'windows', arch: 'x64', windowsInstalled: true),
        ),
        'pb-mapper-windows-x86_64-setup.exe installer',
      );
      expect(
        plan(const UpdateHost(os: 'windows', arch: 'x64')),
        'pb-mapper-windows-x86_64.zip saveDownload',
      );
      expect(
        plan(const UpdateHost(os: 'linux', arch: 'x64', appImage: '/a')),
        'pb-mapper-linux.AppImage replaceAppImage',
      );
      expect(
        plan(const UpdateHost(os: 'linux', arch: 'x64')),
        'pb-mapper-linux.zip saveDownload',
      );
      expect(
        plan(const UpdateHost(os: 'macos', arch: 'arm64')),
        'pb-mapper-macos-arm64.dmg saveDownload',
      );
      expect(
        plan(const UpdateHost(os: 'macos', arch: 'x64')),
        'pb-mapper-macos-x86_64.dmg saveDownload',
      );
      expect(
        plan(const UpdateHost(os: 'android', arch: 'arm')),
        'pb-mapper-armeabi-v7a.apk systemInstaller',
      );
      // Not in this release, and not a platform that updates.
      expect(plan(const UpdateHost(os: 'android', arch: 'x64')), isNull);
      expect(plan(const UpdateHost(os: 'ios', arch: 'arm64')), isNull);
      expect(plan(const UpdateHost(os: 'linux', arch: 'arm64')), isNull);
      expect(plan(const UpdateHost(os: 'macos', arch: 'unknown')), isNull);
      expect(
        plan(
          const UpdateHost(
            os: 'android',
            arch: 'arm',
            androidAbis: ['arm64-v8a', 'armeabi-v7a'],
          ),
        ),
        'pb-mapper-arm64-v8a.apk systemInstaller',
      );
    });

    test('a manifest names only plain files with sound hashes', () {
      Map<String, Object?> asset(String name, String hash) => {
        'version': '0.9.1',
        'assets': [
          {'name': name, 'size': 1, 'sha256': hash},
        ],
      };
      expect(
        () => ReleaseManifest.parse(jsonEncode(asset('../evil', '0' * 64))),
        throwsFormatException,
      );
      expect(
        () => ReleaseManifest.parse(jsonEncode(asset('a.zip', 'XYZ'))),
        throwsFormatException,
      );
      expect(
        ReleaseManifest.parse(
          jsonEncode(asset('a.zip', '0' * 64)),
        ).assets.single.name,
        'a.zip',
      );
    });
  });

  group('the trust check', () {
    test('the signature covers the manifest exactly as fetched', () async {
      final world = _World();
      final updates = world.controller();
      await updates.check();
      expect(updates.stage, UpdateStage.available);
      expect(world.verifier.signed, world.manifest);
    });

    test('a release without a signed manifest is not installed', () async {
      final world = _World(signed: false);
      final updates = world.controller();
      await updates.check();
      expect(updates.stage, UpdateStage.failed);
      expect(updates.failure?.kind, UpdateFailure.unsigned);
      expect(updates.newer, isTrue);
    });

    test('a bad signature stops everything', () async {
      final world = _World()..verifier.valid = false;
      final updates = world.controller();
      await updates.check();
      expect(updates.failure?.kind, UpdateFailure.signature);
      expect(updates.plan, isNull);
    });

    test(
      'an older signed manifest cannot stand in for a newer release',
      () async {
        final world = _World(manifestVersion: '0.9.0');
        final updates = world.controller();
        await updates.check();
        expect(updates.failure?.kind, UpdateFailure.signature);
      },
    );

    test('a download that does not match is thrown away', () async {
      final world = _World(corrupt: utf8.encode('tampered'));
      final updates = world.controller();
      await updates.check();
      await updates.start();
      expect(updates.failure?.kind, UpdateFailure.checksum);
      expect(world.installer.calls, isEmpty);
      expect(world.quits, 0);
    });
  });

  group('the flow', () {
    test('up to date needs no manifest at all', () async {
      final world = _World(version: '0.9.0', signed: false);
      final updates = world.controller();
      await updates.check();
      expect(updates.stage, UpdateStage.upToDate);
      expect(updates.newer, isFalse);
    });

    test('Windows installs after quitting', () async {
      final world = _World();
      final updates = world.controller();
      await updates.check();
      expect(updates.plan?.asset.name, 'pb-mapper-windows-x86_64-setup.exe');
      await updates.start();
      expect(world.installer.calls, [
        'installer pb-mapper-windows-x86_64-setup.exe',
      ]);
      expect(world.quits, 1);
      expect(updates.stage, UpdateStage.installing);

      // The app that opens next says what happened, once: the installer
      // reopens it out of sight, and a failed install looks the same.
      final updated = world.controller(installed: '0.9.1');
      final outcome = await updated.takeOutcome();
      expect('${outcome?.to}', '0.9.1');
      expect(outcome?.installed, isTrue);
      expect(await updated.takeOutcome(), isNull);
    });

    test('an install that did not happen is said so', () async {
      final world = _World();
      final updates = world.controller();
      await updates.check();
      await updates.start();
      final unchanged = world.controller(installed: '0.9.0');
      final outcome = await unchanged.takeOutcome();
      expect(outcome?.installed, isFalse);

      // A note from long ago is not news.
      world.record.pending = PendingUpdate(
        from: const Version(0, 9, 0),
        to: const Version(0, 9, 1),
        at: DateTime.now().subtract(const Duration(days: 1)),
      );
      expect(await unchanged.takeOutcome(), isNull);
    });

    test(
      'a hand-off that cannot start fails with the app still open',
      () async {
        final world = _World()
          ..installer.installerError = const UpdateException(
            UpdateFailure.install,
            'Could not start the installer',
          );
        final updates = world.controller();
        await updates.check();
        await updates.start();
        expect(updates.stage, UpdateStage.failed);
        expect(updates.failure?.kind, UpdateFailure.install);
        expect(world.quits, 0);
        // Nothing is waiting to install it, so the next launch hears nothing.
        expect(world.record.pending, isNull);
      },
    );

    test(
      'a debug build verifies and stops short of replacing itself',
      () async {
        final world = _World();
        final updates = world.controller(replaceSelf: false);
        await updates.check();
        await updates.start();
        expect(updates.stage, UpdateStage.verifiedOnly);
        expect(world.installer.calls, isEmpty);
        expect(world.quits, 0);
      },
    );

    test('an AppImage it cannot replace is saved instead', () async {
      final world = _World()
        ..installer.appImageError = const FileSystemException('read-only');
      final updates = world.controller(
        host: const UpdateHost(
          os: 'linux',
          arch: 'x64',
          appImage: '/opt/pb-mapper.AppImage',
        ),
      );
      await updates.check();
      await updates.start();
      expect(world.installer.calls, ['save pb-mapper-linux.AppImage']);
      expect(updates.stage, UpdateStage.saved);
      expect(world.quits, 0);
    });

    test(
      'Android downloads and verifies before opening the system installer',
      () async {
        final world = _World();
        final updates = world.controller(
          host: const UpdateHost(os: 'android', arch: 'arm64'),
        );
        await updates.check();
        await updates.start();
        expect(world.http.downloaded, [
          _file('v0.9.1-ui', 'pb-mapper-arm64-v8a.apk'),
        ]);
        expect(world.installer.calls, ['android pb-mapper-arm64-v8a.apk']);
        expect(updates.stage, UpdateStage.installerOpened);
        expect(updates.busy, isFalse);
        expect(world.quits, 0);
        // Returning from a cancelled installer can reopen the same download.
        await updates.start();
        expect(world.http.downloaded, hasLength(1));
      },
    );

    test(
      'permission denial keeps the APK, but a modified cache cannot install',
      () async {
        final world = _World()
          ..installer.androidError = const UpdateException(
            UpdateFailure.permission,
          );
        final updates = world.controller(
          host: const UpdateHost(os: 'android', arch: 'arm64'),
        );
        await updates.check();
        await updates.start();
        expect(updates.failure?.kind, UpdateFailure.permission);
        world.installer.androidError = null;
        await updates.start();
        expect(updates.stage, UpdateStage.installerOpened);
        expect(world.http.downloaded, hasLength(1));
        final cached = world.downloads
            .listSync(recursive: true)
            .whereType<File>()
            .single;
        cached.writeAsStringSync('tampered');
        await updates.start();
        expect(updates.failure?.kind, UpdateFailure.checksum);
        expect(world.installer.calls, hasLength(2));
        expect(cached.existsSync(), isFalse);
      },
    );

    test('cancellation on the final download chunk never hands off', () async {
      final world = _World();
      final updates = world.controller(
        host: const UpdateHost(os: 'android', arch: 'arm64'),
      );
      await updates.check();
      updates.addListener(() {
        if (updates.stage == UpdateStage.downloading && updates.received > 0) {
          updates.cancel();
        }
      });
      await updates.start();
      expect(updates.stage, UpdateStage.available);
      expect(world.installer.calls, isEmpty);
      expect(world.downloads.listSync(), isEmpty);
    });

    test('a quiet check that fails keeps what the last one found', () async {
      final world = _World();
      final updates = world.controller();
      await updates.check(quiet: true);
      expect(updates.stage, UpdateStage.available);

      world.http.bodies.clear();
      await updates.check(quiet: true);
      expect(updates.stage, UpdateStage.available);
      expect(updates.plan?.asset.name, 'pb-mapper-windows-x86_64-setup.exe');
      expect(updates.published?.version, const Version(0, 9, 1));
      expect(updates.failure, isNull);
    });

    test('a quiet check still points at a newer unsigned release', () async {
      final world = _World(signed: false);
      final updates = world.controller();
      await updates.check(quiet: true);
      expect(updates.stage, UpdateStage.noPackage);
      expect(updates.newer, isTrue);
    });

    test('a quiet check that fails leaves no trace', () async {
      final world = _World()..http.bodies.clear();
      final updates = world.controller();
      await updates.check(quiet: true);
      expect(updates.stage, UpdateStage.idle);
      expect(updates.failure, isNull);
    });
  });

  group('the schedule', () {
    test('announces a release once, and checks at most twice a day', () async {
      final world = _World();
      final prefs = AppPrefs(persist: false);
      final announced = <String>[];
      var now = DateTime(2026, 10, 25, 9);
      final schedule = UpdateSchedule(
        updates: world.controller(),
        prefs: prefs,
        announce: (version) => announced.add('$version'),
        now: () => now,
      );

      await schedule.tick();
      expect(announced, ['0.9.1']);

      // An hour later: not due, nothing fetched.
      world.http.bodies.clear();
      now = now.add(const Duration(hours: 1));
      await schedule.tick();
      expect(prefs.lastUpdateCheck, DateTime(2026, 10, 25, 9));

      // Due again, the same release: not announced twice.
      final again = _World();
      world.http.bodies.addAll(again.http.bodies);
      now = now.add(const Duration(hours: 12));
      await schedule.tick();
      expect(announced, ['0.9.1']);
      expect(prefs.lastUpdateCheck, now);
    });

    test('turned off, it never checks', () async {
      final world = _World();
      final prefs = AppPrefs(persist: false)..autoCheckUpdates = false;
      final updates = world.controller();
      await UpdateSchedule(
        updates: updates,
        prefs: prefs,
        announce: (_) => fail('announced while off'),
      ).tick();
      expect(updates.stage, UpdateStage.idle);
      expect(prefs.lastUpdateCheck, isNull);
    });
  });

  test("the Windows hand-off quotes paths PowerShell's way", () {
    final script = WindowsInstallerUpdate.script(
      r"C:\Users\o'neil smith\Temp\pb-mapper-windows-x86_64-setup.exe",
      42,
      r'C:\Program Files\pb-mapper\pb_mapper_ui.exe',
    );
    expect(script, contains(r"'C:\Users\o''neil smith\Temp\"));
    expect(script, contains('Wait-Process -Id 42'));
    expect(script, contains('/SILENT'));
    // Setup's log path, with its space, quoted for setup itself.
    expect(
      script,
      contains(r"""'/LOG="C:\Users\o''neil smith\Temp\setup.log"'"""),
    );
    expect(script, contains(r"Start-Process -FilePath 'C:\Program Files\"));
  });

  final library = () {
    final name = Platform.isWindows
        ? 'pb_mapper_ffi.dll'
        : Platform.isMacOS
        ? 'libpb_mapper_ffi.dylib'
        : 'libpb_mapper_ffi.so';
    for (final profile in ['release', 'debug']) {
      final file = File('../target/$profile/$name');
      if (file.existsSync()) return file.absolute.path;
    }
    return null;
  }();

  test(
    'the native check accepts a real Ed25519 signature and nothing else',
    () {
      // A throwaway key, generated and signed with OpenSSL; not the release
      // key.
      final key = base64Decode('NRMpmBFknjIWE2u07GKRJJJ2nWkZUfknRG0i6jtLGYY=');
      final signature = base64Decode(
        'p+RF3TPJxMSi0EG1QWDB7KNJI0HNilMSyXuUAtu7anLvLtGrGWfBDb+oKxh1FZXuC++EK71nCuBbVenPeCPHCw==',
      );
      final message = utf8.encode('{"version":"0.9.1","assets":[]}');
      final verifier = NativeSignatureVerifier(DynamicLibrary.open(library!));
      expect(verifier.verify(message, signature, key), isTrue);
      expect(
        verifier.verify(
          utf8.encode('{"version":"0.9.2","assets":[]}'),
          signature,
          key,
        ),
        isFalse,
      );
      expect(
        verifier.verify(message, signature, base64Decode(updatePublicKey)),
        isFalse,
      );
      expect(verifier.verify(message, signature.sublist(1), key), isFalse);
    },
    skip: library == null ? 'no built pb_mapper_ffi library' : false,
  );

  testWidgets('the About card fits a small phone in Chinese', (tester) async {
    final world = _World();
    final updates = world.controller();
    await updates.check(quiet: true);
    await pumpApp(
      tester,
      FakePbMapperApi(),
      page: AppPage.settings,
      size: const Size(320, 640),
      locale: const Locale('zh'),
      updates: updates,
    );
    // Below the list's cache extent at this size: scroll until it is built.
    await tester.scrollUntilVisible(
      find.byKey(const Key('settings-auto-update')),
      200,
      // The list's own, not a narrow row's sideways one.
      scrollable: find
          .descendant(
            of: find.byKey(const Key('settings-list')),
            matching: find.byType(Scrollable),
          )
          .first,
    );
    await settle(tester);
    expect(find.text('更新到 0.9.1'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await finish(tester);
  });

  testWidgets('Settings offers a release a check found', (tester) async {
    final world = _World();
    final updates = world.controller();
    await pumpApp(
      tester,
      FakePbMapperApi(),
      page: AppPage.settings,
      updates: updates,
    );
    final check = find.byKey(const Key('settings-check-updates'));
    // Below the relay card, past what the list builds before it scrolls.
    await tester.scrollUntilVisible(
      check,
      200,
      scrollable: find
          .descendant(
            of: find.byKey(const Key('settings-list')),
            matching: find.byType(Scrollable),
          )
          .first,
    );
    await settle(tester);
    await tester.tap(check);
    await settle(tester);
    expect(find.byKey(const Key('update-available')), findsOneWidget);
    expect(find.text('pb-mapper 0.9.1 is available'), findsOneWidget);
    expect(find.byKey(const Key('update-start')), findsOneWidget);

    await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
    await settle(tester);
    // Closed, the card now names the version on offer.
    expect(find.text('Update to 0.9.1'), findsOneWidget);
    await finish(tester);
  });

  test(
    'the Windows hand-off waits for the app, installs, and reopens it',
    () async {
      final dir = Directory.systemTemp.createTempSync('pb-mapper-test-');
      addTearDown(() async {
        // The reopened stand-in may still be closing.
        await Future<void>.delayed(const Duration(milliseconds: 500));
        try {
          dir.deleteSync(recursive: true);
        } on FileSystemException catch (_) {}
      });
      final native = DynamicLibrary.open(library!);
      final installed = File('${dir.path}\\installed.txt');

      /// One update whose setup exits with [exitCode]: its download folder,
      /// once the app has been reopened.
      Future<Directory> update(int exitCode, {bool shared = false}) async {
        final name = '$exitCode${shared ? '-shared' : ''}';
        final download = Directory('${dir.path}\\download-$name')..createSync();
        if (shared) File('${download.path}\\other.txt').writeAsStringSync('');
        final reopened = File('${dir.path}\\reopened-$name.txt');
        final installer = File('${download.path}\\setup.cmd')
          ..writeAsStringSync(
            '@echo %*> "${installed.path}"\r\n@exit /b $exitCode\r\n',
          );
        final app = File('${dir.path}\\app-$name.cmd')
          ..writeAsStringSync('@echo reopened> "${reopened.path}"\r\n');
        // The app: a process that ends a moment after the hand-off begins.
        final running = await Process.start('powershell.exe', [
          '-NoProfile',
          '-Command',
          'Start-Sleep -Seconds 2',
        ]);
        await WindowsInstallerUpdate.handOff(
          installer,
          running.pid,
          app.path,
          native,
        );
        expect(installed.existsSync(), isFalse, reason: 'the app still runs');
        final deadline = DateTime.now().add(const Duration(seconds: 30));
        while (!reopened.existsSync()) {
          if (DateTime.now().isAfter(deadline)) {
            fail('the app was not reopened');
          }
          await Future<void>.delayed(const Duration(milliseconds: 100));
        }
        expect(installed.readAsStringSync(), contains('/SILENT'));
        expect(installed.readAsStringSync(), contains('/LOG='));
        installed.deleteSync();
        return download;
      }

      // Installed: nothing of the download is left.
      expect((await update(0)).existsSync(), isFalse);
      // Only what the update made is removed: anything else in its folder,
      // and so the folder, stays.
      final shared = await update(0, shared: true);
      expect(File('${shared.path}\\setup.cmd').existsSync(), isFalse);
      expect(File('${shared.path}\\other.txt').existsSync(), isTrue);
      // Not installed: the installer goes, its folder (and setup's log) stay.
      final failed = await update(1);
      expect(failed.existsSync(), isTrue);
      expect(File('${failed.path}\\setup.cmd').existsSync(), isFalse);
    },
    skip: !Platform.isWindows
        ? 'the hand-off is Windows-only'
        : library == null
        ? 'no built pb_mapper_ffi library'
        : false,
  );
}
