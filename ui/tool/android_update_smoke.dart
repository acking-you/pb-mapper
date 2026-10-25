// Device acceptance for the native handoff, without publishing a release.
// Serve a verified, newer, same-certificate APK as /update.apk on localhost
// port 8765, then: adb reverse tcp:8765 tcp:8765
// Run this debug target with a lower --build-number than that APK. Never ship it.
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/updates/update_installers.dart';

void main() {
  if (!kDebugMode || !Platform.isAndroid) {
    throw StateError('This acceptance target is only for Android debug builds');
  }
  runApp(const MaterialApp(home: _Smoke()));
}

class _Smoke extends StatefulWidget {
  const _Smoke();

  @override
  State<_Smoke> createState() => _SmokeState();
}

class _SmokeState extends State<_Smoke> {
  String status = 'Ready';
  bool busy = false;
  File? package;

  Future<void> install() async {
    setState(() => busy = true);
    try {
      if (package == null) {
        final cache = await getTemporaryDirectory();
        final work = await Directory(
          '${cache.path}/pb-mapper-updates',
        ).create(recursive: true);
        final directory = await work.createTemp('download-');
        final file = File('${directory.path}/update.apk');
        final client = HttpClient();
        try {
          final request = await client.getUrl(
            Uri.http('127.0.0.1:8765', '/update.apk'),
          );
          final response = await request.close();
          if (response.statusCode != HttpStatus.ok) {
            throw HttpException('HTTP ${response.statusCode}');
          }
          await response.pipe(file.openWrite());
          package = file;
        } finally {
          client.close(force: true);
        }
      }
      await AndroidPackageUpdate.install(package!);
      status = 'System installer opened';
    } catch (error) {
      status = '$error';
    } finally {
      debugPrint('ANDROID_UPDATE_SMOKE: $status');
      if (mounted) setState(() => busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Android update acceptance')),
    body: Column(
      children: [
        SelectableText(status),
        FilledButton(
          onPressed: busy ? null : install,
          child: const Text('Install fixture'),
        ),
      ],
    ),
  );
}
