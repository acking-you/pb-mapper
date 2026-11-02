/// The galaxy as a widget: drag to orbit, pinch or
/// scroll to zoom, tap a body to pick it, arrows and Enter to walk the bodies
/// by keyboard. After a few seconds without input the camera glides back to
/// the whole galaxy.
library;

import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_backdrop.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_frame.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_painter.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_atlas.dart';

class GalaxyCanvas extends StatefulWidget {
  const GalaxyCanvas({
    super.key,
    required this.view,
    required this.scene,
    required this.semanticLabel,
    this.onSelect,
  });

  /// The galaxy's state, owned above this widget so it outlives a visit.
  final GalaxyView view;

  /// What to show; a new instance is diffed against the last, so planets that
  /// appear spiral out and planets that change send a ripple round their ring.
  final GalaxyScene scene;
  final String semanticLabel;

  /// Told when the selection changes, including to nothing.
  final ValueChanged<String?>? onSelect;

  @override
  State<GalaxyCanvas> createState() => GalaxyCanvasState();
}

class GalaxyCanvasState extends State<GalaxyCanvas>
    with SingleTickerProviderStateMixin, WidgetsBindingObserver {
  final LabelCache _labels = LabelCache();
  final GalaxyGeometry _geometry = GalaxyGeometry();
  final Starfield _stars = Starfield();
  final BackdropCache _backdrop = BackdropCache();
  final PlanetAtlas _planets = PlanetAtlas();
  final FocusNode _focus = FocusNode(debugLabel: 'galaxy');
  late final Ticker _ticker = createTicker(_onTick);
  Duration _last = Duration.zero;
  double _touched = -6;
  Timer? _glide;
  bool _visible = true;
  bool _enabled = false;
  bool _settled = true;
  double _pinch = 1;
  Size _canvas = Size.zero;

  /// After this long without input the camera glides back to the galaxy.
  static const _glideAfter = Duration(seconds: 6);

  GalaxyView get _view => widget.view;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    final lifecycle = WidgetsBinding.instance.lifecycleState;
    _visible = _isVisible(lifecycle);
    // Frames are computed in Rust once the export is found.
    GalaxyNative.load();
    _take(widget.scene);
    _wake();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final still = MediaQuery.disableAnimationsOf(context);
    final changed = _view.still != still;
    _view.still = still;
    _view.expireTransitions();
    _syncActivity();
    if (changed) _wake();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _visible = _isVisible(state);
    _syncActivity();
  }

  // An inactive view may still be visible while another window has focus or
  // before a mobile app is hidden.
  static bool _isVisible(AppLifecycleState? state) =>
      state == null ||
      state == AppLifecycleState.resumed ||
      state == AppLifecycleState.inactive;

  void _syncActivity() {
    final enabled = _visible && TickerMode.valuesOf(context).enabled;
    if (enabled == _enabled) return;
    _enabled = enabled;
    _planets.enabled = enabled;
    _backdrop.enabled = enabled;
    if (enabled) {
      _wake();
    } else {
      _glide?.cancel();
      _glide = null;
      _ticker.stop();
      _last = Duration.zero;
      _geometry.dispose();
      _labels.dispose();
    }
  }

  @override
  void didUpdateWidget(GalaxyCanvas oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.view, widget.view) ||
        !identical(oldWidget.scene, widget.scene)) {
      _take(widget.scene);
      _wake();
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _glide?.cancel();
    _ticker.dispose();
    _focus.dispose();
    _labels.dispose();
    _geometry.dispose();
    _backdrop.dispose();
    _planets.dispose();
    super.dispose();
  }

  void _take(GalaxyScene scene) {
    final first = !_view.hasScene;
    final before = _view.scene.systems.length;
    _view.update(scene, first: first);
    if (first || before != scene.systems.length) {
      _refit(entrance: first);
    } else if (_view.selected == null && _settled) {
      // Bodies were added or removed: the disk may have grown.
      _refit(entrance: false);
    }
  }

  bool get _ambient =>
      !_view.still && _view.time - _touched < _glideAfter.inSeconds;

  bool get _needsFrames => _ambient || !_view.camera.settled || _view.busy;

  void _onTick(Duration elapsed) {
    if (!_enabled) return;
    final seconds = math.max(0.0, (elapsed - _last).inMicroseconds / 1e6);
    final dt = seconds.clamp(0.0, 0.05);
    _last = elapsed;
    final camera = _view.camera;
    final idle = _view.time - _touched >= _glideAfter.inSeconds;
    if (idle && !_settled && _view.selected == null) {
      camera.overview();
      _settled = true;
    }
    if (_view.selected case final id?) {
      final at = _view.positionOf(id);
      if (at != null) camera.goalTarget = at;
    }
    camera.step(dt);
    // A slow raster frame must not stretch transition lifetimes.
    _view.time += seconds;
    _view.expireTransitions();
    _view.tick();
    if (!_needsFrames) {
      _ticker.stop();
      _scheduleGlide();
    }
  }

  void _scheduleGlide() {
    if (_settled || _view.selected != null || !_enabled) return;
    final remaining = math.max(
      0.0,
      _glideAfter.inMicroseconds / 1e6 - (_view.time - _touched),
    );
    _glide = Timer(Duration(microseconds: (remaining * 1e6).round()), () {
      _glide = null;
      _settled = true;
      _view.camera.overview();
      _wake();
    });
  }

  /// Draw frames again: something moved, changed or was asked for.
  void _wake() {
    _glide?.cancel();
    _glide = null;
    if (!mounted || !_enabled) return;
    _view.tick();
    if (_ticker.isActive) return;
    if (!_needsFrames) {
      _scheduleGlide();
      return;
    }
    _last = Duration.zero;
    _ticker.start();
  }

  void _touch() {
    _touched = _view.time;
    _settled = false;
    _wake();
  }

  /// Frame every system again (the systems or the canvas changed).
  void _refit({required bool entrance}) {
    var reach = 2.0;
    for (final system in _view.scene.systems) {
      reach = math.max(reach, system.position.length + system.extent);
    }
    final camera = _view.camera..fit(reach, _canvas);
    if (entrance) {
      // The galaxy arrives from a little further out.
      camera
        ..overview()
        ..distance = camera.fitDistance * 1.35;
    } else if (_view.selected == null) {
      camera.overview();
    }
    _wake();
  }

  /// Pick body [id], or nothing; the camera eases to it.
  void select(String? id) {
    setState(() => _view.selected = id);
    widget.onSelect?.call(id);
    final camera = _view.camera;
    if (id == null) {
      camera.overview();
      _touch();
      return;
    }
    final system = _view.scene.systemOf(id);
    final at = _view.positionOf(id);
    if (at != null) {
      camera.focus(at, system != null ? 3.4 + system.extent * 1.6 : 2.6);
    }
    _touch();
  }

  /// Back to the whole galaxy, nothing picked.
  void overview() => select(null);

  void _tap(Offset point) {
    _focus.requestFocus();
    select(_view.hitAt(point)?.id);
  }

  void _hover(Offset point) {
    final id = _view.hitAt(point)?.id;
    if (id != _view.hovered) _mark(() => _view.hovered = id);
  }

  void _mark(VoidCallback change) {
    setState(change);
    _wake();
  }

  List<String> get _order => [
    for (final system in _view.scene.systems) ...[
      system.id,
      for (final planet in system.planets) planet.id,
    ],
  ];

  KeyEventResult _key(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    final order = _order;
    if (order.isEmpty) return KeyEventResult.ignored;
    final key = event.logicalKey;
    final at = order.indexOf(_view.focused ?? _view.selected ?? '');
    if (key == LogicalKeyboardKey.arrowRight ||
        key == LogicalKeyboardKey.arrowDown) {
      _mark(() => _view.focused = order[(at + 1) % order.length]);
    } else if (key == LogicalKeyboardKey.arrowLeft ||
        key == LogicalKeyboardKey.arrowUp) {
      _mark(
        () => _view.focused = order[(at - 1 + order.length) % order.length],
      );
    } else if (key == LogicalKeyboardKey.enter ||
        key == LogicalKeyboardKey.space) {
      select(_view.focused ?? order.first);
    } else if (key == LogicalKeyboardKey.escape && _view.selected != null) {
      select(null);
    } else {
      return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final still = MediaQuery.disableAnimationsOf(context);
    if (still != _view.still) {
      _view.still = still;
      _wake();
    }
    final palette = GalaxyPalette(
      dark: theme.brightness == Brightness.dark,
      ratio: MediaQuery.devicePixelRatioOf(context),
      scheme: theme.colorScheme,
      text: theme.textTheme.bodySmall ?? const TextStyle(),
    );
    _planets.palette(theme.colorScheme);
    return LayoutBuilder(
      builder: (context, constraints) {
        final canvas = constraints.biggest;
        if (canvas != _canvas) {
          _canvas = canvas;
          _refit(entrance: false);
        }
        return Semantics(
          label: widget.semanticLabel,
          child: Focus(
            focusNode: _focus,
            onKeyEvent: _key,
            child: Listener(
              onPointerSignal: (event) {
                if (event is PointerScrollEvent) {
                  _view.camera.zoom(event.scrollDelta.dy < 0 ? 1.12 : 1 / 1.12);
                  _touch();
                }
              },
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onScaleStart: (_) {
                  _pinch = 1;
                  _view.camera.spin = 0;
                  _touch();
                },
                onScaleUpdate: (details) {
                  if (details.pointerCount > 1) {
                    _view.camera.zoom(details.scale / _pinch);
                    _pinch = details.scale;
                  }
                  _view.camera.drag(
                    details.focalPointDelta.dx,
                    details.focalPointDelta.dy,
                  );
                  _touch();
                },
                onScaleEnd: (details) {
                  _view.camera.spin =
                      -details.velocity.pixelsPerSecond.dx * 0.004;
                  _touch();
                },
                onTapUp: (details) => _tap(details.localPosition),
                child: MouseRegion(
                  cursor: _view.hovered == null
                      ? SystemMouseCursors.grab
                      : SystemMouseCursors.click,
                  onHover: (event) => _hover(event.localPosition),
                  onExit: (_) => _mark(() => _view.hovered = null),
                  child: RepaintBoundary(
                    child: CustomPaint(
                      key: const Key('galaxy-canvas'),
                      size: Size.infinite,
                      painter: GalaxyPainter(
                        view: _view,
                        palette: palette,
                        labels: _labels,
                        geometry: _geometry,
                        starfield: _stars,
                        backdrop: _backdrop,
                        planets: _planets,
                        onBackdrop: _wake,
                      ),
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
}
