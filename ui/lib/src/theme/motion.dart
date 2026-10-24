import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoPageTransitionsBuilder;
import 'package:flutter/material.dart';

/// Shared motion tokens.
///
/// Three durations and two curves cover the app: a hover or press answers in
/// [Motion.fast], a panel opening takes [Motion.medium], a page or the theme
/// changing takes [Motion.slow]. Arrivals decelerate ([Motion.emphasized]);
/// released things spring ([Motion.spring]). One vocabulary keeps the app
/// feeling like a single object instead of a dozen widgets each timed by hand.
abstract final class Motion {
  /// Hover, press, small state swaps.
  static const fast = Duration(milliseconds: 120);

  /// Panels, expanding rows.
  static const medium = Duration(milliseconds: 220);

  /// Page transitions and the theme change.
  static const slow = Duration(milliseconds: 320);

  /// Things moving or resizing in place.
  static const Curve move = Curves.easeInOutCubic;

  /// Arrivals that should feel physical: a quick start that settles slowly.
  static const Curve emphasized = Cubic(0.05, 0.7, 0.1, 1);

  /// A damped spring with a whisper of overshoot, for things that are
  /// released — a selection gliding to a new row, a control springing back.
  static const Curve spring = _SpringCurve();

  /// [d], or zero when the platform asks for reduced motion — the state still
  /// changes, it just doesn't travel.
  static Duration of(BuildContext context, Duration d) =>
      reduced(context) ? Duration.zero : d;

  /// Whether the platform asks for reduced motion.
  static bool reduced(BuildContext context) =>
      MediaQuery.maybeDisableAnimationsOf(context) ?? false;
}

/// The step response of an under-damped spring (damping ratio 0.72), scaled
/// so it settles at 1 by the end: about 3 % of overshoot and back, which reads
/// as a physical release rather than a tween.
class _SpringCurve extends Curve {
  const _SpringCurve();

  static const double _zeta = 0.72;
  static const double _omega = 9.5;

  @override
  double transformInternal(double t) {
    final wd = _omega * math.sqrt(1 - _zeta * _zeta);
    final decay = math.exp(-_zeta * _omega * t);
    final x =
        1 -
        decay * (math.cos(wd * t) + (_zeta * _omega / wd) * math.sin(wd * t));
    return t >= 1 ? 1 : x;
  }
}

/// Desktop pages cross-fade instead of Material's zoom, which reads as a phone
/// animation inside a desktop window. Mobile keeps each platform's own.
const appPageTransitions = PageTransitionsTheme(
  builders: {
    TargetPlatform.android: FadeForwardsPageTransitionsBuilder(),
    TargetPlatform.iOS: CupertinoPageTransitionsBuilder(),
    TargetPlatform.macOS: _FadePageTransitionsBuilder(),
    TargetPlatform.windows: _FadePageTransitionsBuilder(),
    TargetPlatform.linux: _FadePageTransitionsBuilder(),
  },
);

class _FadePageTransitionsBuilder extends PageTransitionsBuilder {
  const _FadePageTransitionsBuilder();

  @override
  Duration get transitionDuration => Motion.medium;

  @override
  Widget buildTransitions<T>(
    PageRoute<T> route,
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
    Widget child,
  ) {
    if (Motion.reduced(context)) return child;
    return FadeTransition(
      opacity: CurvedAnimation(
        parent: animation,
        curve: Curves.easeOut,
        reverseCurve: Curves.easeIn,
      ),
      child: child,
    );
  }
}
