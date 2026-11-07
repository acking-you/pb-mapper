import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:window_manager/window_manager.dart';

/// Where the controls for a viewer of this computer are reachable right now
/// (D15). A capsule in a title bar says when it shows them; when none does,
/// or a dialog's barrier covers the title bar, the fallback layer above the
/// navigator carries them instead.
final class SharingChrome {
  SharingChrome();

  late final NavigatorObserver observer = _PopupObserver(this);
  final _fallbackNeeded = ValueNotifier(true);
  final _covered = ValueNotifier(false);
  final _reachable = <Object>{};
  int _popups = 0;
  bool _scheduled = false, _disposed = false;

  /// True when no capsule is reachable: none is mounted and showing its
  /// controls, or a popup route (dialog, sheet, menu) is on top of it.
  ValueListenable<bool> get fallbackNeeded => _fallbackNeeded;

  /// True while a popup route (dialog, sheet, menu) is on top: its barrier
  /// covers every title-bar capsule.
  ValueListenable<bool> get covered => _covered;

  /// A capsule or the host card's viewer panel says whether it shows the
  /// controls. Applied after the frame, never during build.
  void setReachable(Object owner, bool reachable) {
    final changed = reachable
        ? _reachable.add(owner)
        : _reachable.remove(owner);
    if (changed) _schedule();
  }

  void _popupsChanged(int by) {
    _popups = (_popups + by).clamp(0, 1 << 20);
    _schedule();
  }

  void _schedule() {
    if (_scheduled || _disposed) return;
    _scheduled = true;
    SchedulerBinding.instance
      ..addPostFrameCallback((_) {
        _scheduled = false;
        if (_disposed) return;
        // Together, so the pill and the capsule swap in one frame.
        _covered.value = _popups > 0;
        _fallbackNeeded.value = _reachable.isEmpty || _covered.value;
      })
      ..ensureVisualUpdate();
  }

  void dispose() {
    _disposed = true;
    _fallbackNeeded.dispose();
    _covered.dispose();
  }
}

/// Counts the popup routes on the root navigator: each one's barrier covers
/// the title bar.
class _PopupObserver extends NavigatorObserver {
  _PopupObserver(this._chrome);
  final SharingChrome _chrome;

  void _count(Route<dynamic>? route, int by) {
    if (route is PopupRoute) _chrome._popupsChanged(by);
  }

  @override
  void didPush(Route<dynamic> route, Route<dynamic>? previousRoute) =>
      _count(route, 1);
  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) =>
      _count(route, -1);
  @override
  void didRemove(Route<dynamic> route, Route<dynamic>? previousRoute) =>
      _count(route, -1);
  @override
  void didReplace({Route<dynamic>? newRoute, Route<dynamic>? oldRoute}) {
    _count(oldRoute, -1);
    _count(newRoute, 1);
  }
}

enum SharingIndicatorForm { full, icons, dot }

/// The capsule's form by the title bar's own width, never the window's.
SharingIndicatorForm sharingFormFor(double barWidth) => barWidth >= 960
    ? SharingIndicatorForm.full
    : barWidth >= 640
    ? SharingIndicatorForm.icons
    : SharingIndicatorForm.dot;

/// What a viewer of this computer is doing, in words.
String sharingLabel(BuildContext context, DesktopHostViewer viewer) {
  final l10n = context.l10n;
  return viewer.paused
      ? l10n.sharingViewerPaused
      : viewer.controlling
      ? l10n.sharingBeingControlled
      : l10n.sharingBeingViewed;
}

/// The dot that says this computer is watched: it breathes while the
/// picture flows, and rests while the viewer has paused it, because nothing
/// is captured then.
Widget _viewerDot(DesktopHostViewer viewer, Color color) => viewer.paused
    ? StatusDot(color: color, size: 7)
    : PulsingDot(color: color, size: 7);

/// The title-bar capsule while someone views this computer (D15), with
/// Disconnect viewer and Stop sharing. Nothing at all otherwise. Under a
/// popup it steps aside, keeping its place, for the fallback pill.
class SharingIndicator extends StatefulWidget {
  const SharingIndicator({
    super.key = const Key('sharing-indicator'),
    required this.form,
    this.tone = CaptionTone.surface,
  });

  final SharingIndicatorForm form;
  final CaptionTone tone;

  @override
  State<SharingIndicator> createState() => _SharingIndicatorState();
}

class _SharingIndicatorState extends State<SharingIndicator> {
  SharingChrome? _chrome;
  bool _shown = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final chrome = AppScope.maybeOf(context)?.sharing;
    if (!identical(chrome, _chrome)) {
      _chrome?.setReachable(this, false);
      _chrome = chrome;
      if (_shown) chrome?.setReachable(this, true);
    }
  }

  void _report(bool shown) {
    if (shown == _shown) return;
    _shown = shown;
    _chrome?.setReachable(this, shown);
  }

  @override
  void dispose() {
    _chrome?.setReachable(this, false);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scope = AppScope.maybeOf(context);
    if (scope == null) return const SizedBox.shrink();
    final desktop = scope.store.desktop;
    return ValueListenableBuilder(
      valueListenable: desktop.hostView,
      builder: (context, host, _) {
        final viewer = host.viewer;
        _report(viewer != null);
        if (viewer == null) return const SizedBox.shrink();
        final label = sharingLabel(context, viewer);
        final capsule = Semantics(
          container: true,
          liveRegion: true,
          label: label,
          child: switch (widget.form) {
            SharingIndicatorForm.dot => _dot(context, desktop, viewer, label),
            _ => _capsule(context, desktop, viewer, label),
          },
        );
        // Under a popup's barrier the fallback pill carries the controls:
        // one set, never two.
        return ValueListenableBuilder(
          valueListenable: scope.sharing.covered,
          builder: (context, covered, child) => Visibility(
            visible: !covered,
            maintainSize: true,
            maintainAnimation: true,
            maintainState: true,
            child: child!,
          ),
          child: capsule,
        );
      },
    );
  }

  Color _fill(ColorScheme scheme) => widget.tone == CaptionTone.viewer
      ? const Color(0x297DB6F2)
      : scheme.tertiaryContainer;

  Color _ink(ColorScheme scheme) => widget.tone == CaptionTone.viewer
      ? ViewerPalette.text
      : scheme.onTertiaryContainer;

  Color _signal(ColorScheme scheme) => widget.tone == CaptionTone.viewer
      ? ViewerPalette.signal
      : signalColor(scheme);

  Widget _capsule(
    BuildContext context,
    DesktopStore desktop,
    DesktopHostViewer viewer,
    String label,
  ) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final ink = _ink(scheme);
    final full = widget.form == SharingIndicatorForm.full;
    final text = TextButton.styleFrom(
      foregroundColor: ink,
      minimumSize: const Size(0, 26),
      fixedSize: const Size.fromHeight(26),
      padding: const EdgeInsets.symmetric(horizontal: 10),
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(13)),
      // From the theme's label style: a bare TextStyle here would replace
      // it and lose the UI typeface.
      textStyle: Theme.of(context).textTheme.labelLarge?.copyWith(
        fontSize: 12.5,
        fontWeight: FontWeight.w500,
      ),
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      visualDensity: VisualDensity.compact,
    );
    final icon = IconButton.styleFrom(
      foregroundColor: ink,
      minimumSize: const Size.square(26),
      fixedSize: const Size.square(26),
      padding: EdgeInsets.zero,
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
    );
    return Container(
      height: 30,
      padding: const EdgeInsetsDirectional.fromSTEB(12, 0, 3, 0),
      decoration: BoxDecoration(
        color: _fill(scheme),
        borderRadius: BorderRadius.circular(15),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          _viewerDot(viewer, _signal(scheme)),
          const SizedBox(width: 8),
          if (full) ...[
            Text(
              label,
              maxLines: 1,
              style: TextStyle(
                fontSize: 12.5,
                fontWeight: FontWeight.w600,
                color: ink,
              ),
            ),
            const SizedBox(width: 8),
            TextButton(
              key: const Key('sharing-indicator-disconnect'),
              style: text,
              onPressed: desktop.endViewerSession,
              child: Text(l10n.hostDisconnectViewer),
            ),
            TextButton(
              key: const Key('sharing-indicator-stop'),
              style: text,
              onPressed: desktop.stopHost,
              child: Text(l10n.hostStop),
            ),
          ] else ...[
            IconButton(
              key: const Key('sharing-indicator-disconnect'),
              style: icon,
              tooltip: l10n.hostDisconnectViewer,
              onPressed: desktop.endViewerSession,
              icon: const Icon(AppIcons.disconnect, size: AppIconSize.inline),
            ),
            IconButton(
              key: const Key('sharing-indicator-stop'),
              style: icon,
              tooltip: l10n.hostStop,
              onPressed: desktop.stopHost,
              icon: const Icon(
                AppIcons.screenShareOff,
                size: AppIconSize.inline,
              ),
            ),
          ],
        ],
      ),
    );
  }

  Widget _dot(
    BuildContext context,
    DesktopStore desktop,
    DesktopHostViewer viewer,
    String label,
  ) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return MenuAnchor(
      menuChildren: [
        MenuItemButton(
          key: const Key('sharing-indicator-disconnect'),
          leadingIcon: const Icon(AppIcons.disconnect),
          onPressed: desktop.endViewerSession,
          child: Text(l10n.hostDisconnectViewer),
        ),
        MenuItemButton(
          key: const Key('sharing-indicator-stop'),
          leadingIcon: const Icon(AppIcons.screenShareOff),
          onPressed: desktop.stopHost,
          child: Text(l10n.hostStop),
        ),
      ],
      builder: (context, menu, _) => Tooltip(
        message: label,
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: () => menu.isOpen ? menu.close() : menu.open(),
          child: Container(
            width: 30,
            height: 30,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: _fill(scheme),
              shape: BoxShape.circle,
            ),
            child: _viewerDot(viewer, _signal(scheme)),
          ),
        ),
      ),
    );
  }
}

/// The D15 controls wherever no capsule is reachable: the setup flow and
/// the viewer, which have no title-bar capsule, and under any dialog's
/// barrier. It sits above the navigator, so above every route; for the same
/// reason it has no overlay, and so no tooltips or menus.
class SharingFallbackLayer extends StatefulWidget {
  const SharingFallbackLayer({
    super.key,
    required this.desktop,
    required this.chrome,
  });

  final DesktopStore desktop;
  final SharingChrome chrome;

  @override
  State<SharingFallbackLayer> createState() => _SharingFallbackLayerState();
}

class _SharingFallbackLayerState extends State<SharingFallbackLayer>
    with WindowListener {
  bool _fullScreen = false;

  @override
  void initState() {
    super.initState();
    if (isFramelessDesktop) windowManager.addListener(this);
  }

  @override
  void dispose() {
    if (isFramelessDesktop) windowManager.removeListener(this);
    super.dispose();
  }

  @override
  void onWindowEnterFullScreen() => setState(() => _fullScreen = true);

  @override
  void onWindowLeaveFullScreen() => setState(() => _fullScreen = false);

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([
      widget.desktop.hostView,
      widget.chrome.fallbackNeeded,
    ]),
    builder: (context, _) {
      final viewer = widget.desktop.hostView.value.viewer;
      if (viewer == null || !widget.chrome.fallbackNeeded.value) {
        return const SizedBox.shrink();
      }
      final pill = _pill(context, viewer);
      if (_fullScreen) {
        // The viewer's toolbar owns the top centre.
        return Positioned(left: 12, top: 12, child: pill);
      }
      return Positioned(
        top: MediaQuery.paddingOf(context).top + 11,
        left: 0,
        right: 0,
        child: Center(child: pill),
      );
    },
  );

  Widget _pill(BuildContext context, DesktopHostViewer viewer) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final label = sharingLabel(context, viewer);
    const height = Size.fromHeight(28);
    return Semantics(
      key: const Key('sharing-fallback'),
      container: true,
      liveRegion: true,
      label: label,
      child: Glass(
        depth: GlassDepth.floating,
        borderRadius: BorderRadius.circular(18),
        child: Material(
          type: MaterialType.transparency,
          child: Container(
            height: 36,
            padding: const EdgeInsetsDirectional.fromSTEB(14, 0, 4, 0),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                _viewerDot(viewer, signalColor(scheme)),
                const SizedBox(width: 8),
                if (MediaQuery.sizeOf(context).width >= 640) ...[
                  Text(
                    label,
                    maxLines: 1,
                    style: const TextStyle(
                      fontSize: 12.5,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  const SizedBox(width: 8),
                ],
                TextButton(
                  key: const Key('sharing-fallback-disconnect'),
                  style: TextButton.styleFrom(
                    fixedSize: height,
                    visualDensity: VisualDensity.compact,
                  ),
                  onPressed: widget.desktop.endViewerSession,
                  child: Text(l10n.hostDisconnectViewer),
                ),
                const SizedBox(width: 4),
                FilledButton.tonal(
                  key: const Key('sharing-fallback-stop'),
                  style: FilledButton.styleFrom(
                    fixedSize: height,
                    visualDensity: VisualDensity.compact,
                  ),
                  onPressed: widget.desktop.stopHost,
                  child: Text(l10n.hostStop),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
