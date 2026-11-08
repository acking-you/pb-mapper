import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
import '../remote_format.dart';
import 'viewer_overlays.dart';

/// Where a phone's controls go around the picture (ui §9.2).
enum GutterKind {
  /// In the black left and right of the picture.
  side,

  /// Above and below it: a tablet in portrait, or a 4:3 remote.
  bars,

  /// Too little room either way: over the picture's edges, on plates.
  overlay,
}

/// The narrowest gutter the controls fit in.
const kGutterMin = 64.0;

/// The two gutters of the touch viewer.
@immutable
class GutterLayout {
  const GutterLayout({
    required this.kind,
    required this.lead,
    required this.trail,
  });

  final GutterKind kind;

  /// Back, quality and mode: the left gutter, or the top bar.
  final Rect lead;

  /// Keyboard, shortcuts, right click and more: the right gutter, or the
  /// bottom bar.
  final Rect trail;

  /// What lies between the gutters, where hints, the minimap and panels go.
  Rect content(Size screen) => kind == GutterKind.bars
      ? Rect.fromLTRB(0, lead.bottom, screen.width, trail.top)
      : Rect.fromLTRB(lead.right, 0, trail.left, screen.height);

  /// The picture's frame: it fits and pans between side gutters or bars,
  /// so no control ever stands over it. Plates stand over the picture,
  /// which then fills the screen.
  EdgeInsets frame(Size screen) => switch (kind) {
    GutterKind.side => EdgeInsets.fromLTRB(
      lead.right,
      0,
      screen.width - trail.left,
      0,
    ),
    GutterKind.bars => EdgeInsets.fromLTRB(
      0,
      lead.bottom,
      0,
      screen.height - trail.top,
    ),
    GutterKind.overlay => EdgeInsets.zero,
  };

  /// The dark strips under side gutters or bars, from the screen's edge
  /// (a cutout's too) to the frame; none for plates.
  ({Rect lead, Rect trail})? strips(Size screen) {
    final f = frame(screen);
    final w = screen.width, h = screen.height;
    return switch (kind) {
      GutterKind.side => (
        lead: Rect.fromLTRB(0, 0, f.left, h),
        trail: Rect.fromLTRB(w - f.right, 0, w, h),
      ),
      GutterKind.bars => (
        lead: Rect.fromLTRB(0, 0, w, f.top),
        trail: Rect.fromLTRB(0, h - f.bottom, w, h),
      ),
      GutterKind.overlay => null,
    };
  }

  @override
  bool operator ==(Object other) =>
      other is GutterLayout &&
      other.kind == kind &&
      other.lead == lead &&
      other.trail == trail;

  @override
  int get hashCode => Object.hash(kind, lead, trail);
}

/// The gutters for a picture of [aspect] fitted into [screen]: beside it
/// when it leaves 64 px on each side, above and below it when it leaves
/// that there, else over its edges. Each keeps out of [padding] (cutouts).
GutterLayout gutterLayoutFor(Size screen, double aspect, EdgeInsets padding) {
  final w = screen.width, h = screen.height;
  final gx = (w - h * aspect) / 2;
  final gy = (h - w / aspect) / 2;
  if (gx >= kGutterMin) {
    return GutterLayout(
      kind: GutterKind.side,
      lead: Rect.fromLTRB(
        padding.left,
        padding.top,
        math.max(gx, padding.left + kGutterMin),
        h - padding.bottom,
      ),
      trail: Rect.fromLTRB(
        math.min(w - gx, w - padding.right - kGutterMin),
        padding.top,
        w - padding.right,
        h - padding.bottom,
      ),
    );
  }
  if (gy >= kGutterMin) {
    return GutterLayout(
      kind: GutterKind.bars,
      lead: Rect.fromLTRB(
        padding.left,
        padding.top,
        w - padding.right,
        math.max(gy, padding.top + kGutterMin),
      ),
      trail: Rect.fromLTRB(
        padding.left,
        math.min(h - gy, h - padding.bottom - kGutterMin),
        w - padding.right,
        h - padding.bottom,
      ),
    );
  }
  // The plates stand a little in from the screen's edges.
  const inset = 8.0;
  return GutterLayout(
    kind: GutterKind.overlay,
    lead: Rect.fromLTRB(
      padding.left + inset,
      padding.top,
      padding.left + inset + kGutterMin,
      h - padding.bottom,
    ),
    trail: Rect.fromLTRB(
      w - padding.right - inset - kGutterMin,
      padding.top,
      w - padding.right - inset,
      h - padding.bottom,
    ),
  );
}

/// Over the picture, a control stands on a dark plate.
const _plate = BoxDecoration(
  color: ViewerPalette.hint,
  borderRadius: BorderRadius.all(Radius.circular(14)),
);

/// A gutter control: a 44 circle with its icon, and a small label below.
/// It never takes the keyboard from the picture.
class GutterButton extends StatelessWidget {
  const GutterButton({
    super.key,
    required this.icon,
    required this.semanticLabel,
    this.label,
    this.onTap,
    this.onLongPress,
    this.fill = ViewerPalette.fill,
    this.iconColor = ViewerPalette.text,
    this.enabled = true,
    this.plate = false,
  });

  final IconData icon;
  final String semanticLabel;

  /// Null: the circle alone.
  final String? label;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;
  final Color fill;
  final Color iconColor;

  /// Off: faded and inert.
  final bool enabled;

  /// Over the picture rather than beside it.
  final bool plate;

  @override
  Widget build(BuildContext context) {
    final label = this.label;
    Widget body = Container(
      width: 44,
      height: 44,
      decoration: BoxDecoration(color: fill, shape: BoxShape.circle),
      child: Icon(icon, size: AppIconSize.nav, color: iconColor),
    );
    if (label != null) {
      body = Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          body,
          const SizedBox(height: 4),
          Text(
            label,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            textAlign: TextAlign.center,
            style: const TextStyle(fontSize: 10.5, color: ViewerPalette.text),
          ),
        ],
      );
    }
    final button = MergeSemantics(
      child: Semantics(
        button: true,
        enabled: enabled,
        label: semanticLabel,
        child: Material(
          type: MaterialType.transparency,
          child: InkWell(
            onTap: enabled ? onTap : null,
            onLongPress: enabled ? onLongPress : null,
            borderRadius: const BorderRadius.all(Radius.circular(14)),
            child: Container(
              width: kGutterMin,
              padding: plate ? const EdgeInsets.symmetric(vertical: 6) : null,
              decoration: plate ? _plate : null,
              child: ExcludeSemantics(child: Center(child: body)),
            ),
          ),
        ),
      ),
    );
    return enabled ? button : Opacity(opacity: 0.4, child: button);
  }
}

/// The path and frame rate at a glance; a tap opens the quality panel.
class QualityGutterButton extends StatelessWidget {
  const QualityGutterButton({
    super.key,
    required this.session,
    required this.stats,
    required this.onTap,
    this.selected = false,
    this.plate = false,
  });

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopStats> stats;
  final VoidCallback onTap;

  /// The panel is open.
  final bool selected;
  final bool plate;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([session, stats]),
    builder: (context, _) {
      final l10n = context.l10n;
      final s = session.value;
      final st = stats.value;
      final (:degraded, :dot) = viewerLink(s, st);
      final path = pathLabel(context, s.path);
      final fps = s.connected ? st.presentedFps.round() : null;
      return MergeSemantics(
        child: Semantics(
          button: true,
          selected: selected,
          label: fps == null
              ? l10n.viewerQuality
              : l10n.touchQualityA11y(path, fps),
          child: Material(
            type: MaterialType.transparency,
            child: InkWell(
              onTap: onTap,
              borderRadius: const BorderRadius.all(Radius.circular(14)),
              child: Container(
                width: plate ? kGutterMin : 56,
                padding: const EdgeInsets.symmetric(vertical: 8),
                // Open, a plate lightens as a bare chip fills.
                decoration: switch ((plate, selected)) {
                  (true, false) => _plate,
                  (true, true) => _plate.copyWith(
                    color: Color.alphaBlend(
                      ViewerPalette.fillStrong,
                      ViewerPalette.hint,
                    ),
                  ),
                  (false, true) => const BoxDecoration(
                    color: ViewerPalette.fill,
                    borderRadius: BorderRadius.all(Radius.circular(14)),
                  ),
                  (false, false) => null,
                },
                child: ExcludeSemantics(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Container(
                        width: 8,
                        height: 8,
                        decoration: BoxDecoration(
                          color: dot,
                          shape: BoxShape.circle,
                        ),
                      ),
                      const SizedBox(height: 3),
                      Text(
                        path,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          fontSize: 11,
                          color: ViewerPalette.text,
                        ),
                      ),
                      const SizedBox(height: 3),
                      Text(
                        fps == null ? '—' : '$fps',
                        style: mono(
                          fontSize: 14,
                          fontWeight: FontWeight.w500,
                          color: degraded
                              ? ViewerPalette.caution
                              : ViewerPalette.text,
                        ),
                      ),
                      const SizedBox(height: 3),
                      const Text(
                        'fps',
                        style: TextStyle(
                          fontSize: 10.5,
                          color: ViewerPalette.textSecondary,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      );
    },
  );
}
