import 'dart:async';

import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

import '../../../shell/window_chrome.dart';
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/viewer_palette.dart';
import 'desktop_keyboard.dart';
import 'modifier_latch.dart';
import 'quality_panel.dart';
import 'remote_input.dart';
import 'remote_viewport.dart';
import 'touch_gestures.dart';
import 'viewer_menus.dart';
import 'viewer_overlays.dart';
import 'viewer_stage.dart';
import 'viewer_title_bar.dart';
import 'viewer_toolbar.dart';

/// The full-window viewer for a mouse and a keyboard (ui §8.1): a title
/// bar, the picture, and a floating toolbar over it. A touchscreen on such
/// a computer touches directly.
///
/// Every dependency is explicit, so the acceptance tool hosts it in a bare
/// `MaterialApp`. It takes no viewer demand: its host holds that while it
/// is on screen. [desktop] must stay the same for the widget's life.
class DesktopViewer extends StatefulWidget {
  const DesktopViewer({
    super.key,
    required this.desktop,
    required this.prefs,
    required this.title,
    this.onBack,
    this.onDisconnect,
    this.onQuality,
    this.onResolution,
    this.onDisplayMode,
    this.onMaxFps,
    this.onReconnect,
    this.onPasteNewInvite,
  });

  final DesktopStore desktop;
  final ViewerPrefs prefs;
  final String title;

  /// Back to the list with the session kept; null: no Back control.
  final VoidCallback? onBack;

  /// Default: `desktop.disconnect()`.
  final Future<void> Function()? onDisconnect;

  /// Default: `desktop.setQuality`.
  final ValueChanged<DesktopQuality>? onQuality;

  /// Defaults: `desktop.setResolution` and `desktop.setMaxFps`.
  final ValueChanged<DesktopResolution?>? onResolution;

  /// The remote resolution (R8); null: straight to the store.
  final ValueChanged<DisplayModeTarget>? onDisplayMode;
  final ValueChanged<int>? onMaxFps;

  /// Default: connects the session's target again.
  final VoidCallback? onReconnect;

  /// Null: the action is hidden.
  final VoidCallback? onPasteNewInvite;

  @override
  State<DesktopViewer> createState() => _DesktopViewerState();
}

class _DesktopViewerState extends State<DesktopViewer> with WindowListener {
  final _viewport = RemoteViewport();
  final _latch = ModifierLatch();
  final _touchMode = ValueNotifier(TouchMode.direct);
  final _picture = FocusNode(debugLabel: 'remote picture');
  late final RemoteInput _input;
  late final DesktopKeyboard _keyboard;
  late final TouchGestureMachine _machine;
  late final ToolbarController _toolbar;
  late final AppLifecycleListener _lifecycle;
  late ViewerActions _actions;
  bool _fullscreen = false, _panelHeld = false;

  DesktopStore get _desktop => widget.desktop;

  @override
  void initState() {
    super.initState();
    _input = RemoteInput(
      desktop: _desktop,
      latch: _latch,
      onLocalCancel: () => _machine.cancel(_input.clock()),
    );
    _machine = TouchGestureMachine(
      sink: _input,
      viewport: _viewport,
      mode: () => _touchMode.value,
      interactive: () => _input.ready.value,
      haptic: viewerHaptic,
    );
    _keyboard = DesktopKeyboard(input: _input, picture: _picture);
    _toolbar = ToolbarController(pinned: widget.prefs.toolbarPinned)..start();
    _actions = _buildActions();
    widget.prefs.addListener(_prefsChanged);
    _desktop.session.addListener(_keyboardSettingsChanged);
    _prefsChanged();
    // Local input only: what is on screen belongs to the viewer's host, and
    // pausing a hidden app to the store.
    _lifecycle = AppLifecycleListener(
      onInactive: _input.releaseAll,
      onHide: _input.releaseAll,
    );
    if (isFramelessDesktop) {
      windowManager.addListener(this);
      unawaited(_syncFullscreen());
    }
  }

  @override
  void didUpdateWidget(covariant DesktopViewer oldWidget) {
    super.didUpdateWidget(oldWidget);
    assert(identical(oldWidget.desktop, widget.desktop));
    if (!identical(oldWidget.prefs, widget.prefs)) {
      oldWidget.prefs.removeListener(_prefsChanged);
      widget.prefs.addListener(_prefsChanged);
      _prefsChanged();
    }
    // The screen's actions can change while the viewer is up: Paste new
    // invitation, once the computer is remembered.
    _actions = _buildActions();
  }

  @override
  void dispose() {
    if (isFramelessDesktop) {
      windowManager.removeListener(this);
      if (_fullscreen) unawaited(_setFullscreen(false));
    }
    widget.prefs.removeListener(_prefsChanged);
    _desktop.session.removeListener(_keyboardSettingsChanged);
    _lifecycle.dispose();
    _keyboard.dispose();
    // The input releases through the machine, so it goes first.
    _input.dispose();
    _machine.dispose();
    _toolbar.dispose();
    _viewport.dispose();
    _latch.dispose();
    _touchMode.dispose();
    _picture.dispose();
    super.dispose();
  }

  /// The keyboard is this computer's: chosen in the Shortcuts menu, and
  /// found again once the computer is remembered.
  void _keyboardSettingsChanged() {
    final keyboard = widget.prefs.keyboardFor(
      _desktop.session.value.target?.computerId,
    );
    if (keyboard == _input.keyboard) return;
    // The keys held or waiting went the old way.
    _input
      ..releaseAll()
      ..keyboard = keyboard;
    _keyboard.sync();
  }

  /// The quality panel holds the toolbar out while it is open.
  void _prefsChanged() {
    _keyboardSettingsChanged();
    final panel = widget.prefs.qualityPanel;
    if (panel != _panelHeld) {
      _panelHeld = panel;
      panel ? _toolbar.hold() : _toolbar.unhold();
    }
  }

  Future<void> _syncFullscreen() async {
    try {
      final fullscreen = await windowManager.isFullScreen();
      if (mounted && fullscreen != _fullscreen) {
        setState(() => _fullscreen = fullscreen);
      }
    } catch (_) {
      // No window to ask (a test, or a platform that refuses).
    }
  }

  Future<void> _setFullscreen(bool value) async {
    try {
      await windowManager.setFullScreen(value);
    } catch (_) {
      // Best effort: the window stays as it is.
    }
  }

  @override
  void onWindowEnterFullScreen() => setState(() => _fullscreen = true);

  @override
  void onWindowLeaveFullScreen() => setState(() => _fullscreen = false);

  Future<void> _disconnect() =>
      (widget.onDisconnect ?? _desktop.disconnect).call();

  void _reconnect() {
    final target = _desktop.session.value.target;
    if (target != null) unawaited(_desktop.connect(target));
  }

  /// The dialog holds the toolbar out, and the picture takes the keyboard
  /// back when it closes.
  Future<void> _typeText() async {
    _toolbar.hold();
    final String? text;
    try {
      text = await TypeTextDialog.show(context);
    } finally {
      _toolbar.unhold();
      if (mounted) _picture.requestFocus();
    }
    if (text != null && text.isNotEmpty) _input.typeText(text);
  }

  ViewerActions _buildActions() => ViewerActions(
    desktop: _desktop,
    input: _input,
    prefs: widget.prefs,
    viewport: _viewport,
    onDisconnect: _disconnect,
    onQuality: (q) => (widget.onQuality ?? _desktop.setQuality)(q),
    onResolution: (r) => (widget.onResolution ?? _desktop.setResolution)(r),
    onDisplayMode: (target) =>
        (widget.onDisplayMode ?? _desktop.setDisplayMode)(target),
    onMaxFps: (fps) => (widget.onMaxFps ?? _desktop.setMaxFps)(fps),
    onTypeText: () => unawaited(_typeText()),
    onBack: widget.onBack == null ? null : () => widget.onBack?.call(),
    onReconnect: () => (widget.onReconnect ?? _reconnect)(),
    onPasteNewInvite: widget.onPasteNewInvite == null
        ? null
        : () => widget.onPasteNewInvite?.call(),
  );

  @override
  Widget build(BuildContext context) => Material(
    color: ViewerPalette.ground,
    child: SafeArea(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (!_fullscreen)
            ViewerTitleBar(title: widget.title, onBack: widget.onBack),
          Expanded(
            child: Stack(
              children: [
                // The keyboard belongs to the picture alone: a toolbar menu
                // or a dialog takes it away, which releases what is held.
                Positioned.fill(
                  child: Focus(
                    focusNode: _picture,
                    autofocus: true,
                    onKeyEvent: _input.handleHardwareKey,
                    onFocusChange: (focused) {
                      if (!focused) _input.releaseAll();
                    },
                    child: ViewerStage.of(
                      _desktop,
                      viewport: _viewport,
                      input: _input,
                      machine: _machine,
                      touchMode: _touchMode,
                      title: widget.title,
                      actions: _actions,
                      toolbarInset: _toolbar.inset,
                    ),
                  ),
                ),
                Positioned.fill(
                  child: RemoteComposition(
                    text: _keyboard.text,
                    input: _input,
                    viewport: _viewport,
                  ),
                ),
                Positioned(
                  top: 0,
                  left: 0,
                  right: 0,
                  child: ViewerToolbar(
                    controller: _toolbar,
                    actions: _actions,
                    pictureFocus: _picture,
                    fullscreen: _fullscreen,
                    onFullscreen: isFramelessDesktop
                        ? () => unawaited(_setFullscreen(!_fullscreen))
                        : null,
                  ),
                ),
                ListenableBuilder(
                  listenable: Listenable.merge([widget.prefs, _toolbar.inset]),
                  builder: (context, _) => !widget.prefs.qualityPanel
                      ? const SizedBox.shrink()
                      : PositionedDirectional(
                          top: _toolbar.inset.value,
                          end: 14,
                          bottom: 14,
                          child: Align(
                            alignment: AlignmentDirectional.topEnd,
                            child: QualityPanel(
                              session: _desktop.session,
                              stats: _desktop.stats,
                              decoder: _desktop.decoderStats,
                            ),
                          ),
                        ),
                ),
                // At the top right, beside the panel while that is open.
                ListenableBuilder(
                  listenable: Listenable.merge([widget.prefs, _toolbar.inset]),
                  builder: (context, _) => !widget.prefs.qualityMonitor
                      ? const SizedBox.shrink()
                      : PositionedDirectional(
                          top: _toolbar.inset.value,
                          end: widget.prefs.qualityPanel
                              ? 14 + QualityPanel.width + 8
                              : 14,
                          child: QualityMonitor(
                            session: _desktop.session,
                            stats: _desktop.stats,
                          ),
                        ),
                ),
                Positioned(
                  left: 12,
                  right: 12,
                  bottom: 64,
                  child: Center(
                    child: ControlHint(
                      session: _desktop.session,
                      title: widget.title,
                    ),
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    ),
  );
}
