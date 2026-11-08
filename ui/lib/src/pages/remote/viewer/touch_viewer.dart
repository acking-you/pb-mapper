import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' show PointerDeviceKind;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/motion.dart';
import '../../../theme/viewer_palette.dart';
import 'gesture_sheet.dart';
import 'minimap.dart';
import 'modifier_bar.dart';
import 'modifier_latch.dart';
import 'quality_panel.dart';
import 'remote_input.dart';
import 'remote_text_input.dart';
import 'remote_viewport.dart';
import 'side_panel.dart';
import 'touch_controls.dart';
import 'touch_gestures.dart';
import 'viewer_menus.dart';
import 'viewer_overlays.dart';
import 'viewer_stage.dart';
import 'virtual_mouse.dart';

/// After the soft keyboard's height last changed, the view pans to keep
/// the cursor in sight.
const kKeyboardSettle = Duration(milliseconds: 100);

/// The viewer on a phone or a tablet (ui §9): the picture across the
/// screen, its controls in the gutters beside it, the soft keyboard with a
/// modifier bar, a minimap while zoomed, and the gesture sheet.
///
/// Every dependency is explicit, as for the desktop viewer. It takes no
/// viewer demand: its host holds that while it is on screen. [desktop]
/// must stay the same for the widget's life.
class TouchViewer extends StatefulWidget {
  const TouchViewer({
    super.key,
    required this.desktop,
    required this.prefs,
    required this.title,
    required this.onBack,
    this.onDisconnect,
    this.onQuality,
    this.onReconnect,
    this.onPasteNewInvite,
  });

  final DesktopStore desktop;
  final ViewerPrefs prefs;
  final String title;

  /// Back to the list with the session kept.
  final VoidCallback onBack;

  /// Default: `desktop.disconnect()`.
  final Future<void> Function()? onDisconnect;

  /// Default: `desktop.setQuality`.
  final ValueChanged<DesktopQuality>? onQuality;

  /// Default: connects the session's target again.
  final VoidCallback? onReconnect;

  /// Null: the action is hidden.
  final VoidCallback? onPasteNewInvite;

  @override
  State<TouchViewer> createState() => _TouchViewerState();
}

class _TouchViewerState extends State<TouchViewer> {
  final _viewport = RemoteViewport();
  final _latch = ModifierLatch();
  final _picture = FocusNode(debugLabel: 'remote picture');
  final _modeHint = ValueNotifier(false);
  late final ValueNotifier<TouchMode> _touchMode;
  late final RemoteInput _input;
  late final TouchGestureMachine _machine;
  late final RemoteTextInput _textInput;
  late final AppLifecycleListener _lifecycle;
  late ViewerActions _actions;
  late Widget _stage;

  Timer? _settle, _hintTimer;
  double _inset = 0;
  bool _keyboard = false, _oriented = false, _sheetShown = false;

  /// Direct touch: where the right-click button clicks.
  Offset? _lastTouch;

  /// The virtual mouse's top-left, kept while the viewer is.
  final _padAt = ValueNotifier<Offset?>(null);

  DesktopStore get _desktop => widget.desktop;

  @override
  void initState() {
    super.initState();
    _touchMode = ValueNotifier(widget.prefs.touchMode);
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
    _textInput = RemoteTextInput(input: _input);
    _textInput.attached.addListener(_attachedChanged);
    widget.prefs.addListener(_prefsChanged);
    _desktop.session.addListener(_sessionChanged);
    // Local input only: what is on screen belongs to the viewer's host, and
    // pausing a hidden app to the store.
    _lifecycle = AppLifecycleListener(
      onInactive: _input.releaseAll,
      onHide: _input.releaseAll,
    );
    _actions = _buildActions();
    _stage = _buildStage();
    _showModeHint();
    _system(SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky));
    _sessionChanged();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // Phones turn the picture landscape; tablets rotate freely.
    if (!_oriented) {
      _oriented = true;
      if (MediaQuery.sizeOf(context).shortestSide < 600) {
        _system(
          SystemChrome.setPreferredOrientations(const [
            DeviceOrientation.landscapeLeft,
            DeviceOrientation.landscapeRight,
          ]),
        );
      }
    }
    _inset = MediaQuery.viewInsetsOf(context).bottom;
    _keyboardChanged();
  }

  @override
  void didUpdateWidget(covariant TouchViewer oldWidget) {
    super.didUpdateWidget(oldWidget);
    assert(identical(oldWidget.desktop, widget.desktop));
    final newPrefs = !identical(oldWidget.prefs, widget.prefs);
    if (newPrefs) {
      oldWidget.prefs.removeListener(_prefsChanged);
      widget.prefs.addListener(_prefsChanged);
    }
    // The screen's actions can change while the viewer is up: Paste new
    // invitation, once the computer is remembered.
    if (newPrefs ||
        (oldWidget.onPasteNewInvite == null) !=
            (widget.onPasteNewInvite == null)) {
      _actions = _buildActions();
      _stage = _buildStage();
    } else if (oldWidget.title != widget.title) {
      _stage = _buildStage();
    }
    if (newPrefs) _prefsChanged();
  }

  @override
  void dispose() {
    widget.prefs.removeListener(_prefsChanged);
    _desktop.session.removeListener(_sessionChanged);
    _textInput.attached.removeListener(_attachedChanged);
    _lifecycle.dispose();
    _settle?.cancel();
    _hintTimer?.cancel();
    _textInput.dispose();
    // The input releases through the machine, so it goes first.
    _input.dispose();
    _machine.dispose();
    _viewport.dispose();
    _latch.dispose();
    _touchMode.dispose();
    _modeHint.dispose();
    _padAt.dispose();
    _picture.dispose();
    _system(SystemChrome.setEnabledSystemUIMode(SystemUiMode.edgeToEdge));
    _system(SystemChrome.setPreferredOrientations(const []));
    super.dispose();
  }

  /// Best effort: a platform that refuses keeps its bars and orientation.
  static void _system(Future<void> call) => unawaited(
    call.catchError((Object error) {
      debugPrint('The viewer could not set the system UI: $error');
    }),
  );

  ViewerActions _buildActions() => ViewerActions(
    desktop: _desktop,
    input: _input,
    prefs: widget.prefs,
    viewport: _viewport,
    onDisconnect: () => (widget.onDisconnect ?? _desktop.disconnect)(),
    onQuality: (q) => (widget.onQuality ?? _desktop.setQuality)(q),
    onTypeText: _textInput.show,
    onBack: () => widget.onBack(),
    onGestureSheet: () => unawaited(_showGestures()),
    onReconnect: () => (widget.onReconnect ?? _reconnect)(),
    onPasteNewInvite: widget.onPasteNewInvite == null
        ? null
        : () => widget.onPasteNewInvite?.call(),
  );

  /// Built once, and again only for a new title, preferences or actions, so
  /// the keyboard, the hints and the gutters never rebuild the picture.
  Widget _buildStage() => Listener(
    onPointerDown: _touched,
    // A Bluetooth keyboard goes through the picture as on desktop; the
    // soft keyboard's client never takes focus.
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
        compact: true,
      ),
    ),
  );

  void _reconnect() {
    final target = _desktop.session.value.target;
    if (target != null) unawaited(_desktop.connect(target));
  }

  void _prefsChanged() {
    final mode = widget.prefs.touchMode;
    if (mode != _touchMode.value) {
      _touchMode.value = mode;
      _showModeHint();
    }
    if (mounted) setState(() {});
  }

  /// The gesture sheet shows itself once, after the first live picture
  /// ever.
  void _sessionChanged() {
    if (_sheetShown || widget.prefs.gestureSheetSeen) return;
    if (_desktop.session.value.phase != DesktopPhase.live) return;
    _sheetShown = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) unawaited(_showGestures(first: true));
    });
  }

  /// Shown by itself, the sheet counts as seen however it is closed.
  Future<void> _showGestures({bool first = false}) async {
    final prefs = widget.prefs;
    await GestureSheet.show(context, prefs);
    if (first) unawaited(prefs.setGestureSheetSeen(true));
    _refocus();
  }

  /// The picture takes the keyboard back once nothing is over it.
  void _refocus() {
    if (!mounted || ModalRoute.of(context)?.isCurrent == false) return;
    _picture.requestFocus();
  }

  void _panel(String title, List<ViewerMenuEntry> entries) => unawaited(
    showViewerSidePanel(
      context,
      title: title,
      entries: entries,
    ).whenComplete(_refocus),
  );

  void _showModeHint() {
    _hintTimer?.cancel();
    _hintTimer = Timer(kHintDuration, () => _modeHint.value = false);
    _modeHint.value = true;
  }

  void _touched(PointerDownEvent e) {
    if (e.kind == PointerDeviceKind.mouse) return;
    if (!_viewport.visibleRect.contains(e.localPosition)) return;
    final n = _viewport.toNormalized(e.localPosition);
    if (n != null) _lastTouch = n;
  }

  /// At the cursor in trackpad mode, where the finger last was in direct
  /// touch; latched modifiers apply.
  void _rightClick() {
    final at = _touchMode.value == TouchMode.direct
        ? (_lastTouch ?? _input.cursor)
        : _input.cursor;
    _input.click(RemoteButton.right, at);
  }

  // ------------------------------------------------------------ keyboard

  void _attachedChanged() => setState(_keyboardChanged);

  /// The soft keyboard and the bar over it only occlude the picture: the
  /// scene keeps its size and scale, and the view pans so the cursor stays
  /// in sight once the keyboard settles. When it goes, the zoom and pan
  /// from before it come back, unless the user moved the picture meanwhile
  /// (ui §10.6).
  void _keyboardChanged() {
    final open = _inset > 0 && _textInput.attached.value;
    if (open) {
      if (!_keyboard) _viewport.beginKeyboard();
      _viewport.setOcclusion(
        EdgeInsets.only(bottom: _inset + kModifierBarHeight),
      );
      _settle?.cancel();
      _settle = Timer(
        kKeyboardSettle,
        () => _viewport.ensureVisible(_input.cursor),
      );
    } else if (_keyboard) {
      _settle?.cancel();
      _viewport.endKeyboard();
    }
    _keyboard = open;
  }

  // --------------------------------------------------------------- build

  @override
  Widget build(BuildContext context) {
    // The viewer fills the window, as its padding and insets assume. Not a
    // LayoutBuilder: it lays itself out again for any rebuild beneath it,
    // and so repaints the whole viewer at the rate of a stroke or a pan.
    final size = MediaQuery.sizeOf(context);
    final padding = MediaQuery.paddingOf(context);
    return Scaffold(
      backgroundColor: ViewerPalette.black,
      // The keyboard never resizes the picture: it occludes it.
      resizeToAvoidBottomInset: false,
      body: ValueListenableBuilder(
        valueListenable: _desktop.picture,
        builder: (context, picture, _) => _layers(
          size,
          padding,
          picture?.aspectRatio ?? _viewport.aspectRatio,
        ),
      ),
    );
  }

  Widget _layers(Size size, EdgeInsets padding, double aspect) {
    final prefs = widget.prefs;
    final gutters = gutterLayoutFor(size, aspect, padding);
    // Kept while the keyboard hides the gutters: it only occludes.
    final frame = gutters.frame(size);
    _viewport.setFrame(frame);
    final occluded = _inset + kModifierBarHeight;
    // Where hints, the minimap and panels go: between the gutters, or
    // within the frame above the keyboard while it is up.
    final framed = frame.deflateRect(Offset.zero & size);
    final content = _keyboard
        ? Rect.fromLTRB(
            math.max(padding.left, framed.left),
            math.max(padding.top, framed.top),
            math.min(size.width - padding.right, framed.right),
            size.height - occluded,
          )
        : gutters.content(size);
    return Stack(
      fit: StackFit.expand,
      children: [
        _stage,
        ..._strips(gutters, size),
        if (!_keyboard) ..._gutters(gutters),
        Positioned(
          top: content.top + 10,
          right: size.width - content.right + 12,
          child: ValueListenableBuilder(
            valueListenable: _viewport.visibleRegion,
            builder: (context, region, _) => region == null
                ? const SizedBox.shrink()
                : Minimap(region: region, aspectRatio: aspect),
          ),
        ),
        Positioned(
          left: content.left,
          width: content.width,
          top: content.top + 12,
          child: Center(
            child: ValueListenableBuilder(
              valueListenable: _modeHint,
              builder: (context, shown, _) =>
                  shown ? ModeHint(mode: prefs.touchMode) : const SizedBox(),
            ),
          ),
        ),
        if (prefs.touchMode == TouchMode.direct && prefs.virtualMouse)
          _mousePad(size, content),
        Positioned(
          left: content.left + 12,
          bottom: size.height - content.bottom + 10,
          child: ModifierPill(latch: _latch),
        ),
        if (_keyboard) ...[
          Positioned(
            left: content.left,
            width: content.width,
            bottom: size.height - content.bottom + 48,
            child: Center(
              child: ComposingPreview(composing: _textInput.composing),
            ),
          ),
          Positioned(
            left: 0,
            right: 0,
            bottom: _inset,
            child: ModifierBar(
              input: _input,
              latch: _latch,
              onCollapse: _textInput.hide,
              padding: EdgeInsets.only(
                left: padding.left,
                right: padding.right,
              ),
            ),
          ),
        ],
        // Scrolls within the content's height on a landscape phone.
        if (prefs.qualityPanel)
          Positioned(
            left: content.left + 12,
            top: content.top + 12,
            bottom: size.height - content.bottom + 12,
            child: Align(
              alignment: Alignment.topLeft,
              child: QualityPanel(
                session: _desktop.session,
                stats: _desktop.stats,
                decoder: _desktop.decoderStats,
              ),
            ),
          ),
      ],
    );
  }

  /// The dark strips under side gutters and bars, over what a zoomed
  /// picture has under them; kept while the keyboard hides the controls,
  /// since the picture keeps its frame. They let touches through, so a
  /// trackpad stroke or a pinch can start on them; painted always, they
  /// never repaint with a pan.
  List<Widget> _strips(GutterLayout layout, Size size) {
    final strips = layout.strips(size);
    if (strips == null) return const [];
    Widget strip(Key key, Rect rect) => Positioned.fromRect(
      key: key,
      rect: rect,
      child: const IgnorePointer(
        child: ColoredBox(color: ViewerPalette.gutter),
      ),
    );
    return [
      strip(const Key('touch-gutter-lead'), strips.lead),
      strip(const Key('touch-gutter-trail'), strips.trail),
    ];
  }

  /// Back, quality and mode lead; the keyboard, shortcuts, right click
  /// and more trail (ui §9.2). An empty gutter is not a control, so a
  /// trackpad stroke can start anywhere.
  List<Widget> _gutters(GutterLayout layout) {
    final l10n = context.l10n;
    final prefs = widget.prefs;
    final plate = layout.kind == GutterKind.overlay;
    final axis = layout.kind == GutterKind.bars
        ? Axis.horizontal
        : Axis.vertical;
    final trackpad = prefs.touchMode == TouchMode.trackpad;
    final modeName = trackpad ? l10n.touchModeTrackpad : l10n.touchModeDirect;
    final lead = [
      GutterButton(
        key: const Key('touch-back'),
        icon: AppIcons.back,
        semanticLabel: l10n.touchBack,
        plate: plate,
        onTap: widget.onBack,
      ),
      QualityGutterButton(
        key: const Key('touch-quality'),
        session: _desktop.session,
        stats: _desktop.stats,
        selected: prefs.qualityPanel,
        plate: plate,
        onTap: () => unawaited(prefs.setQualityPanel(!prefs.qualityPanel)),
      ),
      GutterButton(
        key: const Key('touch-mode-toggle'),
        icon: trackpad ? AppIcons.trackpad : AppIcons.directTouch,
        label: modeName,
        semanticLabel: l10n.touchModeToggleA11y(modeName),
        fill: trackpad
            ? ViewerPalette.signal.withValues(alpha: 0.2)
            : ViewerPalette.fill,
        iconColor: trackpad ? ViewerPalette.signal : ViewerPalette.text,
        plate: plate,
        onTap: () => unawaited(
          prefs.setTouchMode(trackpad ? TouchMode.direct : TouchMode.trackpad),
        ),
        onLongPress: () => unawaited(_showGestures()),
      ),
    ];
    final trail = ValueListenableBuilder(
      valueListenable: _input.ready,
      builder: (context, ready, _) {
        final buttons = [
          GutterButton(
            key: const Key('touch-keyboard'),
            icon: AppIcons.keyboard,
            label: l10n.touchKeyboard,
            semanticLabel: l10n.touchKeyboard,
            enabled: ready,
            plate: plate,
            onTap: _textInput.show,
          ),
          GutterButton(
            key: const Key('touch-shortcuts'),
            icon: AppIcons.shortcuts,
            label: l10n.viewerShortcuts,
            semanticLabel: l10n.viewerShortcuts,
            enabled: ready,
            plate: plate,
            onTap: () =>
                _panel(l10n.viewerShortcuts, shortcutsMenu(l10n, _actions)),
          ),
          GutterButton(
            key: const Key('touch-right-click'),
            icon: AppIcons.rightClick,
            label: l10n.touchRightClick,
            semanticLabel: l10n.touchRightClick,
            enabled: ready,
            plate: plate,
            onTap: _rightClick,
          ),
          GutterButton(
            key: const Key('touch-more'),
            icon: AppIcons.more,
            label: l10n.more,
            semanticLabel: l10n.more,
            plate: plate,
            onTap: () =>
                _panel(l10n.more, moreMenu(l10n, _actions, touch: true)),
          ),
        ];
        return Flex(
          direction: axis,
          mainAxisSize: MainAxisSize.min,
          children: [
            for (var i = 0; i < buttons.length; i++) ...[
              if (i > 0) const SizedBox.square(dimension: 14),
              buttons[i],
            ],
          ],
        );
      },
    );
    return [
      Positioned.fromRect(
        rect: layout.lead,
        child: Padding(
          padding: axis == Axis.vertical
              ? const EdgeInsets.symmetric(vertical: 16)
              : const EdgeInsets.symmetric(horizontal: 16),
          child: Flex(
            direction: axis,
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: lead,
          ),
        ),
      ),
      Positioned.fromRect(
        rect: layout.trail,
        child: Semantics(
          container: true,
          explicitChildNodes: true,
          label: l10n.touchControls,
          child: Center(
            child: FittedBox(fit: BoxFit.scaleDown, child: trail),
          ),
        ),
      ),
    ];
  }

  /// Bottom right of the picture, at first; then wherever its grip puts
  /// it, within the screen.
  Widget _mousePad(Size size, Rect content) {
    const pad = VirtualMousePad.size;
    final bounds = Rect.fromLTWH(
      0,
      0,
      size.width - pad.width,
      size.height - pad.height,
    );
    Offset clamp(Offset p) => Offset(
      p.dx.clamp(bounds.left, bounds.right < 0 ? 0 : bounds.right),
      p.dy.clamp(bounds.top, bounds.bottom < 0 ? 0 : bounds.bottom),
    );
    final home = content.bottomRight - Offset(pad.width + 12, pad.height + 12);
    Offset place(Offset? p) => clamp(p ?? home);
    // A frame can bring several moves: each adds to where the pad is now,
    // and rebuilds its position alone.
    return ValueListenableBuilder(
      valueListenable: _padAt,
      builder: (context, p, child) {
        final at = place(p);
        return Positioned(left: at.dx, top: at.dy, child: child!);
      },
      child: VirtualMousePad(
        input: _input,
        onMove: (delta) => _padAt.value = clamp(place(_padAt.value) + delta),
      ),
    );
  }
}

/// The input mode in words, for [kHintDuration] when the viewer opens and
/// whenever the mode changes.
class ModeHint extends StatelessWidget {
  const ModeHint({
    super.key = const Key('touch-mode-hint'),
    required this.mode,
  });

  final TouchMode mode;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final trackpad = mode == TouchMode.trackpad;
    return IgnorePointer(
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: 0, end: 1),
        duration: Motion.of(context, Motion.medium),
        builder: (context, t, child) => Opacity(opacity: t, child: child),
        child: ViewerPill(
          height: 32,
          leading: Icon(
            trackpad ? AppIcons.trackpad : AppIcons.directTouch,
            size: AppIconSize.inline,
            color: ViewerPalette.signal,
          ),
          text: trackpad ? l10n.touchTrackpadHint : l10n.touchDirectHint,
        ),
      ),
    );
  }
}

/// What the soft keyboard is composing, before it is committed and sent:
/// CJK is visible while it is typed.
class ComposingPreview extends StatelessWidget {
  const ComposingPreview({super.key, required this.composing});

  final ValueListenable<String> composing;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: composing,
    builder: (context, text, _) => text.isEmpty
        ? const SizedBox.shrink()
        : IgnorePointer(child: ViewerPill(height: 32, text: text)),
  );
}
