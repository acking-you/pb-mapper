// The updater's real I/O: a socket and a disk. Kept apart from
// update_test.dart, whose widget tests make the harness answer every HTTP
// request with a 400 for the whole file.
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:pb_mapper_ui/src/updates/update_installers.dart';
import 'package:pb_mapper_ui/src/updates/update_source.dart';

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
    await work.delete(recursive: true);
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
}
