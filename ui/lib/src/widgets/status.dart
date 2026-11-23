import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';

/// A small solid dot with a soft glow of its own colour.
class StatusDot extends StatelessWidget {
  const StatusDot({super.key, required this.color, this.size = 8});

  final Color color;
  final double size;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    decoration: BoxDecoration(
      color: color,
      shape: BoxShape.circle,
      boxShadow: [
        BoxShadow(
          color: color.withValues(alpha: 0.45),
          blurRadius: 4,
          spreadRadius: 0.3,
        ),
      ],
    ),
  );
}

/// How many times a dot for a state that lasts — a viewer of this computer, a
/// relay running here — brightens before it rests lit. A breath that never
/// ends asks for a frame at the display's rate for as long as it shows.
const kSettlingBreaths = 3;

/// A dot that breathes. Without [breaths] it breathes for as long as it
/// shows, for something in flight that ends on its own: a tunnel retrying,
/// the relay being asked. With [breaths] it marks a state that can last: it
/// brightens that many times as it appears, the last time to stay lit,
/// brighter than a [StatusDot], and then draws nothing more. Rests lit under
/// reduced motion.
class PulsingDot extends StatefulWidget {
  const PulsingDot({
    super.key,
    required this.color,
    this.size = 8,
    this.breaths,
  }) : assert(breaths == null || breaths > 0);

  final Color color;
  final double size;

  /// How many times it brightens when it appears, the last time to stay;
  /// null breathes until it is gone.
  final int? breaths;

  @override
  State<PulsingDot> createState() => _PulsingDotState();
}

class _PulsingDotState extends State<PulsingDot>
    with SingleTickerProviderStateMixin {
  /// Half a breath: dim to lit, or back.
  static const _half = Duration(milliseconds: 900);

  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: _half,
  );
  bool _begun = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final first = !_begun;
    _begun = true;
    if (Motion.reduced(context)) {
      _c
        ..stop()
        ..value = 1;
      return;
    }
    final breaths = widget.breaths;
    if (breaths == null) {
      if (!_c.isAnimating) _c.repeat(reverse: true);
    } else if (first) {
      // One run through every half breath: it ends lit, and the controller
      // stops by itself, so no frame is asked for after it.
      _c
        ..duration = _half * (2 * breaths - 1)
        ..forward(from: 0);
    }
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  /// How lit the dot is, 0 to 1, at the controller's [value].
  double _lift(double value) {
    final breaths = widget.breaths;
    if (breaths == null) return Curves.easeInOut.transform(value);
    // Up on even halves and down on odd ones; the last half, up, ends at 1.
    final at = value * (2 * breaths - 1);
    final half = at.floor();
    final into = at - half;
    return Curves.easeInOut.transform(half.isEven ? into : 1 - into);
  }

  @override
  Widget build(BuildContext context) => RepaintBoundary(
    // While it breathes, only the dot repaints, not the bar around it.
    child: AnimatedBuilder(
      animation: _c,
      builder: (context, _) {
        final t = _lift(_c.value);
        return Container(
          width: widget.size,
          height: widget.size,
          decoration: BoxDecoration(
            color: Color.lerp(
              widget.color.withValues(alpha: 0.4),
              widget.color,
              t,
            ),
            shape: BoxShape.circle,
            boxShadow: [
              BoxShadow(
                color: widget.color.withValues(alpha: 0.2 + 0.5 * t),
                blurRadius: 2 + 5 * t,
                spreadRadius: 0.4 + t,
              ),
            ],
          ),
        );
      },
    ),
  );
}

/// A dot and a word. [filled] sets it in a faint pill of its own colour, for
/// a card where a bare dot and text would float without weight; [inkLabel]
/// writes the word in that colour too, for a pill that is the card's status
/// rather than a note beside it.
class StatusChip extends StatelessWidget {
  const StatusChip({
    super.key,
    required this.color,
    required this.label,
    this.pulsing = false,
    this.breaths,
    this.filled = true,
    this.shrinks = false,
    this.inkLabel = false,
  });

  final Color color;
  final String label;
  final bool pulsing;

  /// With [pulsing], how many times the dot breathes before it rests; see
  /// [PulsingDot.breaths].
  final int? breaths;
  final bool filled;
  final bool inkLabel;

  /// Ellipsize the label when room runs out. Only where the chip's width is
  /// bounded: as a fixed child of a Row it meets unbounded width, where a
  /// shrinking label would have nothing to shrink to.
  final bool shrinks;

  @override
  Widget build(BuildContext context) {
    final ink = inkLabel
        ? color
        : Theme.of(context).colorScheme.onSurfaceVariant;
    final text = Text(
      label,
      maxLines: 1,
      overflow: shrinks ? TextOverflow.ellipsis : null,
      style: TextStyle(fontSize: 12, fontWeight: FontWeight.w500, color: ink),
    );
    final row = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        pulsing
            ? PulsingDot(color: color, breaths: breaths)
            : StatusDot(color: color),
        const SizedBox(width: 6),
        shrinks ? Flexible(child: text) : text,
      ],
    );
    if (!filled) return row;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 4),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.13),
        borderRadius: BorderRadius.circular(20),
      ),
      child: row,
    );
  }
}

/// A tunnel's state as a chip.
class HealthChip extends StatelessWidget {
  const HealthChip({super.key, required this.health, this.filled = true});

  final TunnelHealth health;
  final bool filled;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return StatusChip(
      color: healthColor(scheme, health),
      label: healthLabel(context, health),
      pulsing: health == TunnelHealth.retrying,
      filled: filled,
    );
  }
}

/// The relay's state as a chip.
class RelayChip extends StatelessWidget {
  const RelayChip({
    super.key,
    required this.state,
    this.filled = true,
    this.shrinks = false,
  });

  final RelayState state;
  final bool filled;

  /// See [StatusChip.shrinks].
  final bool shrinks;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final l10n = context.l10n;
    final (color, label) = switch (state) {
      RelayState.online => (successColor(scheme), l10n.relayOnline),
      RelayState.offline => (scheme.error, l10n.relayOffline),
      RelayState.checking => (cautionColor(scheme), l10n.relayChecking),
      RelayState.unset => (scheme.outline, l10n.relayUnset),
    };
    return StatusChip(
      color: color,
      label: label,
      pulsing: state == RelayState.checking,
      filled: filled,
      shrinks: shrinks,
    );
  }
}

/// What [health] is called.
String healthLabel(BuildContext context, TunnelHealth health) {
  final l10n = context.l10n;
  return switch (health) {
    TunnelHealth.running => l10n.healthRunning,
    TunnelHealth.retrying => l10n.healthRetrying,
    TunnelHealth.failed => l10n.healthFailed,
    TunnelHealth.stopped => l10n.healthStopped,
  };
}
