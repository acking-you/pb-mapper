import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/physics.dart';
import 'package:flutter/widgets.dart';

import '../../../ffi/desktop_pointer.dart';
import '../../../state/desktop_input.dart';

/// Units of high-resolution scrolling (`wheel_hires`) per logical pixel of
/// a trackpad's travel or a fling: a notch, 120 units, is 100 px of it.
const kScrollUnitsPerPixel = DesktopScroll.unitsPerNotch / 100;

/// A trackpad's travel per notch toward a host that scrolls in notches
/// alone, and a mouse wheel's notch where the embedder's is not known.
const kTrackpadPxPerNotch = 30.0;

/// Windows' lines per notch until the runner says otherwise: its default.
const kWindowsWheelLines = 3;

/// The logical pixels Flutter reports for one notch of a mouse wheel on
/// [platform], so that a notch here is a notch on the host:
///
/// - Windows: the embedder makes a notch `lines × 100 / 3` physical
///   pixels, cut to a whole number (`flutter_window.cc`), with [windowsLines]
///   the lines a notch scrolls there, and the framework divides it by the
///   view's [devicePixelRatio];
/// - macOS: 40 per line (`FlutterViewController.mm`);
/// - Linux: 53 (`fl_scrolling_manager.cc`);
/// - elsewhere, not measured: [kTrackpadPxPerNotch].
double wheelPxPerNotch(
  TargetPlatform platform, {
  required double devicePixelRatio,
  int windowsLines = kWindowsWheelLines,
}) {
  switch (platform) {
    case TargetPlatform.windows:
      final lines = windowsLines >= 1 && windowsLines <= 100
          ? windowsLines
          : kWindowsWheelLines;
      final ratio = devicePixelRatio > 0 ? devicePixelRatio : 1.0;
      return (lines * 100 / 3).truncateToDouble() / ratio;
    case TargetPlatform.macOS:
      return 40;
    case TargetPlatform.linux:
      return 53;
    case TargetPlatform.android || TargetPlatform.iOS || TargetPlatform.fuchsia:
      return kTrackpadPxPerNotch;
  }
}

/// The lines a notch of this computer's wheel scrolls on Windows, as the
/// runner read them when it started, which is when Flutter's embedder read
/// them for the distance it reports: [kWindowsWheelLines] until the runner
/// answers, and if it cannot.
abstract final class WheelLines {
  static int value = kWindowsWheelLines;
  static bool _asked = false;

  /// Asks the runner once a process; nothing off Windows.
  static void load() {
    if (_asked || kIsWeb || defaultTargetPlatform != TargetPlatform.windows) {
      return;
    }
    _asked = true;
    unawaited(
      desktopWheelLines().then((lines) {
        if (lines != null) value = lines;
      }),
    );
  }
}

/// Whole steps out of fractional amounts, per axis, what falls short of one
/// kept for the next. An amount within a millionth of a whole step counts
/// as whole, so steps that add up exactly, as a Windows notch at 150 % in
/// quarters does, are never one short for rounding.
final class ScrollCarry {
  double _x = 0, _y = 0;

  /// The whole steps [dx] and [dy] complete with what was kept.
  (int, int) take(double dx, double dy) {
    _x += dx;
    _y += dy;
    final x = _whole(_x), y = _whole(_y);
    _x -= x;
    _y -= y;
    return (x, y);
  }

  void clear() => _x = _y = 0;

  static int _whole(double amount) {
    final nearest = amount.roundToDouble();
    return ((amount - nearest).abs() < 1e-6 ? nearest : amount).truncate();
  }
}

/// A trackpad's fling, carried on after the fingers lift as this
/// computer's own scroll views carry one, since Flutter passes on none of
/// the system's: on a Mac or an iPad UIKit's normal deceleration (0.998 a
/// millisecond, `BouncingScrollSimulation`'s friction), elsewhere Android's
/// (`ClampingScrollSimulation`). It ends when it slows below what a
/// scroll view still shows, 20 device pixels a second.
final class ScrollFling {
  ScrollFling._(this._direction, this._simulation);

  /// A fling at [velocity] in logical pixels a second, at most
  /// [kMaxFlingVelocity]; null below [kMinFlingVelocity].
  static ScrollFling? start(
    Velocity velocity, {
    required TargetPlatform platform,
    double devicePixelRatio = 1,
  }) {
    final pps = velocity.pixelsPerSecond;
    final speed = pps.distance;
    if (!(speed >= kMinFlingVelocity)) return null;
    final ratio = devicePixelRatio > 0 ? devicePixelRatio : 1.0;
    final tolerance = Tolerance(
      velocity: 1 / (0.050 * ratio),
      distance: 1 / ratio,
    );
    final initial = math.min(speed, kMaxFlingVelocity);
    final Simulation simulation = switch (platform) {
      TargetPlatform.macOS || TargetPlatform.iOS => FrictionSimulation(
        0.135,
        0,
        initial,
        tolerance: tolerance,
      ),
      TargetPlatform.windows ||
      TargetPlatform.linux ||
      TargetPlatform.android ||
      TargetPlatform.fuchsia => ClampingScrollSimulation(
        position: 0,
        velocity: initial,
        tolerance: tolerance,
      ),
    };
    return ScrollFling._(pps / speed, simulation);
  }

  final Offset _direction;
  final Simulation _simulation;

  /// How far the fling has travelled [elapsed] after it began.
  Offset offsetAt(Duration elapsed) =>
      _direction * _simulation.x(_seconds(elapsed));

  bool isDoneAt(Duration elapsed) => _simulation.isDone(_seconds(elapsed));

  static double _seconds(Duration elapsed) =>
      elapsed.inMicroseconds / Duration.microsecondsPerSecond;

  /// What estimates a trackpad's velocity on [platform] as its own scroll
  /// views do.
  static VelocityTracker tracker(TargetPlatform platform) => switch (platform) {
    TargetPlatform.macOS => MacOSScrollViewFlingVelocityTracker(
      PointerDeviceKind.trackpad,
    ),
    TargetPlatform.iOS => IOSScrollViewFlingVelocityTracker(
      PointerDeviceKind.trackpad,
    ),
    TargetPlatform.windows ||
    TargetPlatform.linux ||
    TargetPlatform.android ||
    TargetPlatform.fuchsia => VelocityTracker.withKind(
      PointerDeviceKind.trackpad,
    ),
  };
}
