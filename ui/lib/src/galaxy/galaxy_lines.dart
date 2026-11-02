/// A reusable line batch, clipped before submitting coordinates to the GPU.
library;

import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';

class GalaxyLines {
  Float32List _points = Float32List(256);
  int _length = 0;

  void clear() => _length = 0;

  /// Add each segment of a projected run, clipping it to [viewport].
  void addRun(Float32List run, Rect viewport) {
    for (var i = 2; i + 1 < run.length; i += 2) {
      add(run[i - 2], run[i - 1], run[i], run[i + 1], viewport);
    }
  }

  /// Liang–Barsky clipping also rejects non-finite projected coordinates.
  void add(double x0, double y0, double x1, double y1, Rect viewport) {
    if (!x0.isFinite || !y0.isFinite || !x1.isFinite || !y1.isFinite) return;
    final dx = x1 - x0;
    final dy = y1 - y0;
    var enter = 0.0;
    var leave = 1.0;

    if (dx == 0) {
      if (x0 < viewport.left || x0 > viewport.right) return;
    } else {
      final a = (viewport.left - x0) / dx;
      final b = (viewport.right - x0) / dx;
      enter = math.max(enter, math.min(a, b));
      leave = math.min(leave, math.max(a, b));
    }
    if (dy == 0) {
      if (y0 < viewport.top || y0 > viewport.bottom) return;
    } else {
      final a = (viewport.top - y0) / dy;
      final b = (viewport.bottom - y0) / dy;
      enter = math.max(enter, math.min(a, b));
      leave = math.min(leave, math.max(a, b));
    }
    if (enter > leave) return;
    if (_length + 4 > _points.length) {
      _points = Float32List(_points.length * 2)..setRange(0, _length, _points);
    }
    _points
      ..[_length] = (x0 + enter * dx).clamp(viewport.left, viewport.right)
      ..[_length + 1] = (y0 + enter * dy).clamp(viewport.top, viewport.bottom)
      ..[_length + 2] = (x0 + leave * dx).clamp(viewport.left, viewport.right)
      ..[_length + 3] = (y0 + leave * dy).clamp(viewport.top, viewport.bottom);
    _length += 4;
  }

  /// The current batch; valid until it is cleared or extended.
  Float32List get points => Float32List.sublistView(_points, 0, _length);

  void draw(Canvas canvas, Paint paint) {
    if (_length != 0) canvas.drawRawPoints(ui.PointMode.lines, points, paint);
  }
}
