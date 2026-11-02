import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_lines.dart';

void main() {
  test('segments are clipped, rejected or retained at viewport edges', () {
    final lines = GalaxyLines();
    const viewport = Rect.fromLTWH(0, 0, 100, 100);
    lines.add(-50, 50, 150, 50, viewport);
    lines.add(50, -50, 50, 150, viewport);
    lines.add(-30, -20, -10, -20, viewport);
    lines.add(0, 0, 100, 100, viewport);
    lines.add(double.nan, 0, 10, 10, viewport);
    lines.add(0, 0, double.infinity, 1, viewport);
    expect(lines.points, [0, 50, 100, 50, 50, 0, 50, 100, 0, 0, 100, 100]);
    lines.clear();
    lines.addRun(Float32List.fromList([-50, -50, 50, 50, 150, 150]), viewport);
    expect(lines.points, [0, 0, 50, 50, 50, 50, 100, 100]);
  });

  test('line batches grow without dropping segments', () {
    final lines = GalaxyLines();
    const viewport = Rect.fromLTWH(0, 0, 100, 100);
    for (var i = 0; i < 300; i++) {
      lines.add(0, i % 100.0, 100, i % 100.0, viewport);
    }
    expect(lines.points.length, 1200);
    expect(lines.points.sublist(1196), [0, 99, 100, 99]);
  });
}
