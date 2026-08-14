import 'package:flutter/material.dart';

/// Moving the navigation between the rail and the bottom bar.
///
/// Crossing the breakpoint used to swap one widget tree for another, so the
/// rail vanished and the bar appeared in a single frame — the layout jumped
/// rather than changed. These slide and shrink each one instead, on the two
/// halves of a single controller so the outgoing bar is gone before the
/// incoming rail starts to widen.
///
/// The curves and the split-interval structure follow Material's own adaptive
/// scaffold pattern, which proxy-everything uses for the same transition.

/// Width and height, held back until the offset has had a moment to move.
class NavSizeAnimation extends CurvedAnimation {
  NavSizeAnimation(Animation<double> parent)
    : super(
        parent: parent,
        curve: const Interval(0.2, 0.8, curve: Curves.easeInOutCubicEmphasized),
        reverseCurve: Interval(
          0,
          0.2,
          curve: Curves.easeInOutCubicEmphasized.flipped,
        ),
      );
}

/// The slide itself, which finishes last so the panel settles into place.
class NavOffsetAnimation extends CurvedAnimation {
  NavOffsetAnimation(Animation<double> parent)
    : super(
        parent: parent,
        curve: const Interval(0.4, 1.0, curve: Curves.easeInOutCubicEmphasized),
        reverseCurve: Interval(
          0,
          0.2,
          curve: Curves.easeInOutCubicEmphasized.flipped,
        ),
      );
}

/// The rail, growing out of and collapsing into the left edge.
class RailTransition extends StatefulWidget {
  const RailTransition({
    super.key,
    required this.animation,
    required this.backgroundColor,
    required this.child,
  });

  final Animation<double> animation;
  final Color backgroundColor;
  final Widget child;

  @override
  State<RailTransition> createState() => _RailTransitionState();
}

class _RailTransitionState extends State<RailTransition> {
  late Animation<Offset> offsetAnimation;
  late Animation<double> widthAnimation;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();

    final ltr = Directionality.of(context) == TextDirection.ltr;

    widthAnimation = Tween<double>(
      begin: 0,
      end: 1,
    ).animate(NavSizeAnimation(widget.animation));

    offsetAnimation = Tween<Offset>(
      begin: ltr ? const Offset(-1, 0) : const Offset(1, 0),
      end: Offset.zero,
    ).animate(NavOffsetAnimation(widget.animation));
  }

  @override
  Widget build(BuildContext context) {
    return ClipRect(
      child: DecoratedBox(
        decoration: BoxDecoration(color: widget.backgroundColor),
        child: Align(
          alignment: Alignment.topLeft,
          widthFactor: widthAnimation.value,
          child: FractionalTranslation(
            translation: offsetAnimation.value,
            child: widget.child,
          ),
        ),
      ),
    );
  }
}

/// The bottom bar, sliding down past the edge of the window as it goes.
class BarTransition extends StatefulWidget {
  const BarTransition({
    super.key,
    required this.animation,
    required this.backgroundColor,
    required this.child,
  });

  final Animation<double> animation;
  final Color backgroundColor;
  final Widget child;

  @override
  State<BarTransition> createState() => _BarTransitionState();
}

class _BarTransitionState extends State<BarTransition> {
  late final Animation<Offset> offsetAnimation = Tween<Offset>(
    begin: const Offset(0, 1),
    end: Offset.zero,
  ).animate(NavOffsetAnimation(widget.animation));

  late final Animation<double> heightAnimation = Tween<double>(
    begin: 0,
    end: 1,
  ).animate(NavSizeAnimation(widget.animation));

  @override
  Widget build(BuildContext context) {
    return ClipRect(
      child: DecoratedBox(
        decoration: BoxDecoration(color: widget.backgroundColor),
        child: Align(
          alignment: Alignment.topLeft,
          heightFactor: heightAnimation.value,
          child: FractionalTranslation(
            translation: offsetAnimation.value,
            child: widget.child,
          ),
        ),
      ),
    );
  }
}
