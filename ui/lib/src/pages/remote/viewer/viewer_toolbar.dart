import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/glass.dart';
import '../../../theme/metrics.dart';
import '../../../theme/motion.dart';
import '../../../theme/viewer_palette.dart';
import '../remote_format.dart';
import 'viewer_menus.dart';
import 'viewer_overlays.dart';

/// An unpinned toolbar folds away this long after the pointer leaves it.
const kToolbarCollapseDelay = Duration(seconds: 2);

/// Hovering the folded handle this long brings the toolbar back.
const kHandleHoverExpand = Duration(milliseconds: 300);

/// Where the pills at the top of the picture go: below the toolbar (12 +
/// 48 + 8), or below its handle (20 + 8).
const kToolbarInsetExpanded = 68.0;
const kToolbarInsetCollapsed = 28.0;

/// Below this picture width the mode, Display and Shortcuts buttons show
/// their icons only.
const _labelsFrom = 760.0;

/// When the desktop viewer's toolbar is out (ui §10.7). Folding it away by
/// itself never touches the pinned preference: manual and automatic
/// collapse stay apart.
class ToolbarController extends ChangeNotifier {
  ToolbarController({required bool pinned}) : _pinned = pinned;

  bool _pinned, _expanded = true, _inside = false;
  int _holds = 0;
  Timer? _collapse, _hover;
  final _inset = ValueNotifier(kToolbarInsetExpanded);

  bool get pinned => _pinned;
  bool get expanded => _expanded;

  /// The top of the pills under the toolbar.
  ValueListenable<double> get inset => _inset;

  /// The viewer is shown.
  void start() {
    _setExpanded(true);
    _scheduleCollapse();
  }

  /// The caller also remembers it ([ViewerPrefs.setToolbarPinned]).
  void setPinned(bool value) {
    if (value == _pinned) return;
    _pinned = value;
    if (value) {
      _cancelHover();
      _cancelCollapse();
      _setExpanded(true);
    } else {
      _scheduleCollapse();
    }
    notifyListeners();
  }

  /// The region is the toolbar and its 12 px top margin, so the handle's
  /// place counts as inside once the toolbar is out: no hover loop.
  void pointerEnter() {
    _inside = true;
    _cancelCollapse();
  }

  void pointerExit() {
    _inside = false;
    _scheduleCollapse();
  }

  /// A touch or a stylus lifted on the toolbar: there is no hover to leave.
  void touchEnded() => _scheduleCollapse();

  void handleEnter() {
    _cancelHover();
    _hover = Timer(kHandleHoverExpand, () {
      _hover = null;
      _expandNow();
    });
  }

  void handleExit() => _cancelHover();

  void handleTap() {
    _cancelHover();
    _expandNow();
  }

  /// A menu, the quality panel or a dialog is open.
  void hold() {
    _holds++;
    _cancelCollapse();
    _setExpanded(true);
  }

  void unhold() {
    if (_holds == 0) return;
    _holds--;
    _scheduleCollapse();
  }

  bool get _idle => !_pinned && _holds == 0 && !_inside;

  void _expandNow() {
    _setExpanded(true);
    // The pointer coming in cancels it; a tap from a touch needs it.
    _scheduleCollapse();
  }

  void _scheduleCollapse() {
    _cancelCollapse();
    if (!_idle) return;
    _collapse = Timer(kToolbarCollapseDelay, () {
      _collapse = null;
      if (_idle) _setExpanded(false);
    });
  }

  void _cancelCollapse() {
    _collapse?.cancel();
    _collapse = null;
  }

  void _cancelHover() {
    _hover?.cancel();
    _hover = null;
  }

  void _setExpanded(bool value) {
    if (value == _expanded) return;
    _expanded = value;
    _inset.value = value ? kToolbarInsetExpanded : kToolbarInsetCollapsed;
    notifyListeners();
  }

  @override
  void dispose() {
    _cancelCollapse();
    _cancelHover();
    _inset.dispose();
    super.dispose();
  }
}

/// The desktop viewer's floating toolbar (Viewer:33-76), or its handle once
/// folded. Translucent graphite, never a blur over the live picture (U19).
class ViewerToolbar extends StatelessWidget {
  const ViewerToolbar({
    super.key,
    required this.controller,
    required this.actions,
    required this.pictureFocus,
    this.fullscreen = false,
    this.onFullscreen,
  });

  final ToolbarController controller;
  final ViewerActions actions;
  final FocusNode pictureFocus;
  final bool fullscreen;

  /// Null where the window cannot go full screen.
  final VoidCallback? onFullscreen;

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, box) => ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        final expanded = controller.expanded;
        return Stack(
          alignment: Alignment.topCenter,
          children: [
            _Shown(
              visible: expanded,
              hidden: const Offset(0, -0.4),
              child: _region(context, box.maxWidth),
            ),
            _Shown(
              visible: !expanded,
              hidden: const Offset(0, -1),
              child: ToolbarHandle(controller: controller),
            ),
          ],
        );
      },
    ),
  );

  Widget _region(BuildContext context, double width) => MouseRegion(
    // The top margin counts as inside for hover, but a click there still
    // reaches the picture.
    hitTestBehavior: HitTestBehavior.translucent,
    onEnter: (_) => controller.pointerEnter(),
    onExit: (_) => controller.pointerExit(),
    child: Listener(
      onPointerUp: (e) {
        if (e.kind != ui.PointerDeviceKind.mouse) controller.touchEnded();
      },
      child: Padding(
        padding: const EdgeInsets.only(top: 12),
        child: ConstrainedBox(
          constraints: BoxConstraints(maxWidth: (width - 24).clamp(0, 2000)),
          child: FittedBox(
            fit: BoxFit.scaleDown,
            child: _bar(context, labels: width >= _labelsFrom),
          ),
        ),
      ),
    ),
  );

  Widget _bar(BuildContext context, {required bool labels}) {
    final l10n = context.l10n;
    final a = actions;
    final desktop = a.desktop;
    const separator = SizedBox(
      width: 9,
      height: 22,
      child: Center(
        child: SizedBox(
          width: 1,
          height: 22,
          child: ColoredBox(color: ViewerPalette.separator),
        ),
      ),
    );
    return Semantics(
      container: true,
      label: l10n.viewerToolbar,
      child: Container(
        height: 48,
        padding: const EdgeInsets.symmetric(horizontal: 6),
        decoration: BoxDecoration(
          color: ViewerPalette.chrome,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(color: ViewerPalette.edge),
          boxShadow: const [
            BoxShadow(
              color: Color.fromRGBO(0, 0, 0, 0.4),
              blurRadius: 30,
              offset: Offset(0, 8),
            ),
          ],
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 2,
          children: [
            ListenableBuilder(
              listenable: controller,
              builder: (context, _) {
                final pinned = controller.pinned;
                return ToolbarButton(
                  key: const Key('viewer-pin'),
                  icon: pinned ? AppIcons.pin : AppIcons.unpin,
                  tooltip: pinned ? l10n.viewerUnpin : l10n.viewerPin,
                  selected: pinned,
                  onPressed: () {
                    controller.setPinned(!pinned);
                    unawaited(a.prefs.setToolbarPinned(!pinned));
                  },
                );
              },
            ),
            separator,
            ToolbarStatusChip(
              key: const Key('viewer-quality-chip'),
              session: desktop.session,
              stats: desktop.stats,
              onTap: () => a.prefs.setQualityPanel(!a.prefs.qualityPanel),
            ),
            separator,
            _ModeControl(desktop: desktop, labels: labels),
            separator,
            ListenableBuilder(
              listenable: Listenable.merge([a.prefs, desktop.quality]),
              builder: (context, _) => ViewerMenuButton(
                buttonKey: const Key('viewer-display'),
                icon: AppIcons.remote,
                label: labels ? l10n.viewerDisplay : null,
                tooltip: labels ? null : l10n.viewerDisplay,
                entries: displayMenu(l10n, a),
                toolbar: controller,
                pictureFocus: pictureFocus,
                width: 260,
              ),
            ),
            ListenableBuilder(
              listenable: Listenable.merge([a.input.ready, desktop.session]),
              builder: (context, _) => ViewerMenuButton(
                buttonKey: const Key('viewer-shortcuts'),
                icon: AppIcons.shortcuts,
                label: labels ? l10n.viewerShortcuts : null,
                tooltip: labels ? null : l10n.viewerShortcuts,
                entries: shortcutsMenu(l10n, a),
                toolbar: controller,
                pictureFocus: pictureFocus,
              ),
            ),
            ListenableBuilder(
              listenable: Listenable.merge([a.input.ready, a.prefs]),
              builder: (context, _) => ViewerMenuButton(
                buttonKey: const Key('viewer-more'),
                icon: AppIcons.more,
                tooltip: l10n.more,
                entries: moreMenu(l10n, a, touch: false),
                toolbar: controller,
                pictureFocus: pictureFocus,
                width: 260,
              ),
            ),
            separator,
            if (onFullscreen case final toggle?)
              ToolbarButton(
                key: const Key('viewer-fullscreen'),
                icon: fullscreen
                    ? AppIcons.exitFullscreen
                    : AppIcons.fullscreen,
                tooltip: fullscreen
                    ? l10n.viewerExitFullscreen
                    : l10n.viewerFullscreen,
                onPressed: toggle,
              ),
            ToolbarButton(
              key: const Key('viewer-disconnect'),
              icon: AppIcons.disconnect,
              label: l10n.viewerDisconnect,
              color: ViewerPalette.danger,
              onPressed: a.onDisconnect,
            ),
          ],
        ),
      ),
    );
  }
}

/// Shown or folded away: a slide and a fade, instant under reduced motion.
/// Folded away, it takes no pointer, focus or semantics.
class _Shown extends StatelessWidget {
  const _Shown({
    required this.visible,
    required this.hidden,
    required this.child,
  });

  final bool visible;
  final Offset hidden;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final duration = Motion.of(context, Motion.medium);
    return IgnorePointer(
      ignoring: !visible,
      child: ExcludeFocus(
        excluding: !visible,
        child: ExcludeSemantics(
          excluding: !visible,
          child: AnimatedSlide(
            offset: visible ? Offset.zero : hidden,
            duration: duration,
            curve: Motion.emphasized,
            child: AnimatedOpacity(
              opacity: visible ? 1 : 0,
              duration: duration,
              child: child,
            ),
          ),
        ),
      ),
    );
  }
}

/// View or Control. Control is disabled while the host does not allow it,
/// and shows that it waits while asked for and not yet granted.
class _ModeControl extends StatelessWidget {
  const _ModeControl({required this.desktop, required this.labels});

  final DesktopStore desktop;
  final bool labels;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: desktop.session,
    builder: (context, s, _) {
      final l10n = context.l10n;
      final control = GlassSegmented<bool>(
        key: const Key('viewer-mode'),
        height: 36,
        expand: false,
        selected: s.controlWanted,
        onChanged: desktop.control,
        segments: [
          GlassSegment(
            value: false,
            label: labels ? l10n.viewerModeView : '',
            icon: AppIcons.viewOnly,
          ),
          GlassSegment(
            value: true,
            label: labels ? l10n.viewerModeControl : '',
            // A segment draws a glyph, not a widget, so the wait is the
            // loader glyph rather than a turning spinner.
            icon: s.controlWanted && s.controlAllowed && !s.controlling
                ? AppIcons.pending
                : AppIcons.control,
            enabled: s.controlAllowed,
          ),
        ],
      );
      final tip = !s.controlAllowed && s.target != null
          ? l10n.viewerViewOnlyByHost
          : labels
          ? null
          : '${l10n.viewerModeView} / ${l10n.viewerModeControl}';
      return tip == null ? control : Tooltip(message: tip, child: control);
    },
  );
}

/// The path, round trip and frame rate, dashes while the connection is not
/// up; when the picture lags, its delay. A tap opens or closes the quality
/// panel.
class ToolbarStatusChip extends StatelessWidget {
  const ToolbarStatusChip({
    super.key,
    required this.session,
    required this.stats,
    required this.onTap,
  });

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopStats> stats;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([session, stats]),
    builder: (context, _) {
      final l10n = context.l10n;
      final s = session.value;
      final st = stats.value;
      final (:degraded, :dot) = viewerLink(s, st);
      final path = pathLabel(context, s.path);
      // No word while the path is unknown, only its figures' dashes.
      final label = degraded
          ? l10n.viewerSlow(path)
          : s.path == null
          ? null
          : path;
      // Each figure right-aligned in fixed mono cells: the bar is centred, so
      // a figure gaining a digit would shift every button.
      String cell(Object? figure, int width) =>
          '${figure ?? '—'}'.padLeft(width);
      final figures = degraded
          ? '${cell(st.pictureLatencyMs, 4)} ms'
          : s.connected
          ? '${cell(st.rttMs, 3)} ms · ${cell(st.presentedFps.round(), 3)} fps'
          : '${cell(null, 3)} ms · ${cell(null, 3)} fps';
      return _Pressable(
        onTap: onTap,
        tooltip: l10n.viewerQuality,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                width: 8,
                height: 8,
                decoration: BoxDecoration(color: dot, shape: BoxShape.circle),
              ),
              const SizedBox(width: 8),
              if (label != null) ...[
                Text(
                  label,
                  style: const TextStyle(
                    fontSize: 13,
                    color: ViewerPalette.text,
                  ),
                ),
                const SizedBox(width: 8),
              ],
              Text(
                figures,
                style: mono(
                  fontSize: 12,
                  color: degraded
                      ? ViewerPalette.caution
                      : ViewerPalette.textSecondary,
                ),
              ),
            ],
          ),
        ),
      );
    },
  );
}

/// The folded toolbar: a tab hanging from the top edge. A tap, or a hover
/// held [kHandleHoverExpand], brings the toolbar back (U7).
class ToolbarHandle extends StatelessWidget {
  const ToolbarHandle({
    super.key = const Key('viewer-toolbar-handle'),
    required this.controller,
  });

  final ToolbarController controller;

  @override
  Widget build(BuildContext context) {
    final label = context.l10n.viewerExpand;
    return Tooltip(
      message: label,
      excludeFromSemantics: true,
      child: Semantics(
        button: true,
        label: label,
        onTap: controller.handleTap,
        child: MouseRegion(
          cursor: SystemMouseCursors.click,
          onEnter: (_) => controller.handleEnter(),
          onExit: (_) => controller.handleExit(),
          child: Listener(
            onPointerUp: (e) {
              if (e.kind != ui.PointerDeviceKind.mouse) controller.touchEnded();
            },
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: controller.handleTap,
              child: ClipRect(
                child: SizedBox(
                  width: 72,
                  height: 20,
                  child: Stack(
                    clipBehavior: Clip.none,
                    children: [
                      // The top edge sits outside the clip: the tab hangs
                      // from the window's edge.
                      const Positioned(
                        left: 0,
                        right: 0,
                        top: -1,
                        bottom: 0,
                        child: DecoratedBox(
                          decoration: BoxDecoration(
                            color: ViewerPalette.chrome,
                            borderRadius: BorderRadius.vertical(
                              bottom: Radius.circular(10),
                            ),
                            border: Border.fromBorderSide(
                              BorderSide(color: ViewerPalette.edge),
                            ),
                          ),
                        ),
                      ),
                      const Center(
                        child: Icon(
                          AppIcons.chevronDown,
                          size: AppIconSize.inline,
                          color: ViewerPalette.text,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// A button on the dark toolbar: 36 tall, an icon alone (36 wide) or with
/// a label, filled while [selected] (pinned, or its menu open).
class ToolbarButton extends StatelessWidget {
  const ToolbarButton({
    super.key,
    required this.icon,
    this.label,
    this.tooltip,
    this.onPressed,
    this.selected = false,
    this.chevron = false,
    this.color = ViewerPalette.text,
  });

  final IconData icon;
  final String? label;
  final String? tooltip;
  final VoidCallback? onPressed;
  final bool selected, chevron;
  final Color color;

  @override
  Widget build(BuildContext context) {
    final label = this.label;
    final ink = onPressed == null ? ViewerPalette.textQuiet : color;
    final content = label == null
        ? SizedBox.square(
            dimension: 36,
            child: Icon(icon, size: AppIconSize.button, color: ink),
          )
        : Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(icon, size: AppIconSize.inline, color: ink),
                const SizedBox(width: 6),
                Text(label, style: TextStyle(fontSize: 13, color: ink)),
                if (chevron) ...[
                  const SizedBox(width: 4),
                  Icon(
                    AppIcons.chevronDown,
                    size: AppIconSize.inline,
                    color: ink,
                  ),
                ],
              ],
            ),
          );
    return _Pressable(
      onTap: onPressed,
      tooltip: tooltip,
      selected: selected,
      child: content,
    );
  }
}

/// 36 tall, r10; hover and selection fill white on the graphite.
class _Pressable extends StatelessWidget {
  const _Pressable({
    required this.onTap,
    required this.child,
    this.tooltip,
    this.selected = false,
  });

  final VoidCallback? onTap;
  final Widget child;
  final String? tooltip;
  final bool selected;

  @override
  Widget build(BuildContext context) {
    const radius = BorderRadius.all(Radius.circular(kControlRadius));
    final button = Material(
      type: MaterialType.transparency,
      child: InkWell(
        onTap: onTap,
        borderRadius: radius,
        mouseCursor: clickable,
        hoverColor: ViewerPalette.fill,
        highlightColor: ViewerPalette.fill,
        splashColor: Colors.transparent,
        child: Ink(
          height: 36,
          decoration: BoxDecoration(
            color: selected ? ViewerPalette.fill : null,
            borderRadius: radius,
          ),
          child: Center(widthFactor: 1, child: child),
        ),
      ),
    );
    final tip = tooltip;
    return tip == null ? button : Tooltip(message: tip, child: button);
  }
}
