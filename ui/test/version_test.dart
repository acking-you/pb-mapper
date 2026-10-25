import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

void main() {
  // The updater compares the running app's version against the newest
  // release. An app built with a stale version would find every release
  // newer than itself and offer the same update forever.
  test('the app is versioned with the workspace', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final cargo = File('../Cargo.toml').readAsStringSync();
    final app = RegExp(
      r'^version:\s*(\d+)\.(\d+)\.(\d+)\+(\d+)\s*$',
      multiLine: true,
    ).firstMatch(pubspec);
    // `version` is the first key of [workspace.package], and the first
    // `version = ` line in the file.
    final workspace = RegExp(
      r'^version = "([^"]+)"',
      multiLine: true,
    ).firstMatch(cargo);
    expect(app, isNotNull, reason: 'pubspec.yaml has no x.y.z+n version');
    expect(workspace, isNotNull, reason: 'Cargo.toml has no version');

    final [major, minor, patch] = [
      for (final i in [1, 2, 3]) int.parse(app![i]!),
    ];
    expect('$major.$minor.$patch', workspace![1]);
    // Package builds advance independently of the public version, including
    // locally distributed rebuilds. APK continuity is checked against the
    // preceding signed package by scripts/release/android_package.py.
    expect(int.parse(app![4]!), greaterThan(0));
  });
}
