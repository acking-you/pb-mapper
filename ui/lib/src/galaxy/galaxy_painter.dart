/// Paints one frame of the galaxy: sky, the disk's plane, star systems far to
/// near with their orbits split around each star, live tunnels as arcs of
/// light, and labels that keep out of each other's way. Where everything goes
/// is computed in Rust ([GalaxyGeometry], `galaxy_frame.dart`); this only
/// draws it.
library;

import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_backdrop.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_frame.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';

/// The colours of one theme.
class GalaxyPalette {
  const GalaxyPalette({
    required this.dark,
    required this.ratio,
    required this.scheme,
    required this.text,
  });

  /// Night sky or daylight orrery.
  final bool dark;

  /// Device pixel ratio.
  final double ratio;
  final ColorScheme scheme;

  /// The app's text style, for labels.
  final TextStyle text;

  /// The one signal colour: this computer's star, and live tunnels.
  Color get signal => signalColor(scheme);

  /// Ink for rings, labels and guides.
  Color get ink => dark ? Colors.white : const Color(0xFF26262A);

  /// The relay's star.
  Color get star => dark ? const Color(0xFFF4F4F2) : const Color(0xFFD9D9D4);

  /// A planet's colour: its tunnel's state, or for a relay service whether
  /// this computer uses it.
  Color planet(Planet planet) {
    if (planet.tunnel case final tunnel?) {
      return healthColor(scheme, tunnel.health);
    }
    return planet.inUse ? successColor(scheme) : availableColor(scheme);
  }

  /// Equal palettes paint alike.
  bool same(GalaxyPalette o) =>
      dark == o.dark && ratio == o.ratio && scheme == o.scheme;
}

/// Laid-out labels, reused across frames.
class LabelCache {
  final Map<String, TextPainter> _painters = {};

  TextPainter of(String text, TextStyle style) {
    final key =
        '${style.fontSize}|${style.fontWeight?.value}|${style.color?.toARGB32()}|$text';
    final cached = _painters[key];
    if (cached != null) return cached;
    if (_painters.length > 300) {
      for (final painter in _painters.values) {
        painter.dispose();
      }
      _painters.clear();
    }
    return _painters[key] = TextPainter(
      text: TextSpan(text: text, style: style),
      textDirection: TextDirection.ltr,
      maxLines: 1,
      ellipsis: '…',
    )..layout(maxWidth: 220);
  }

  void dispose() {
    for (final painter in _painters.values) {
      painter.dispose();
    }
    _painters.clear();
  }
}

/// The labels' styles: a planet's key in mono, brighter when the planet is
/// emphasized, and a star's name in the UI face.
class GalaxyLabelStyles {
  GalaxyLabelStyles(GalaxyPalette palette) {
    final dark = palette.dark;
    // A soft halo of the sky's own colour keeps a label legible over light.
    final halo = [
      Shadow(
        color: (dark ? Colors.black : Colors.white).withValues(
          alpha: dark ? 0.85 : 0.95,
        ),
        blurRadius: 6,
      ),
    ];
    base = palette.text.copyWith(
      fontFamily: monoFontFamily,
      fontFamilyFallback: monoFallback,
      fontSize: 11,
      color: palette.ink.withValues(alpha: dark ? 0.8 : 0.76),
      letterSpacing: 0,
      height: 1.2,
      shadows: halo,
    );
    strong = base.copyWith(
      fontWeight: FontWeight.w600,
      color: palette.ink.withValues(alpha: dark ? 0.94 : 0.9),
    );
    system = palette.text.copyWith(
      fontSize: 12.5,
      fontWeight: FontWeight.w600,
      color: palette.ink.withValues(alpha: dark ? 0.92 : 0.88),
      height: 1.2,
      shadows: halo,
    );
  }

  /// A planet's label.
  late final TextStyle base;

  /// An emphasized planet's label.
  late final TextStyle strong;

  /// A star's label.
  late final TextStyle system;
}

/// The scene's buffers, re-encoded only when the scene, a body's motion or
/// the label styles change.
class GalaxyGeometry {
  final GalaxyFrameBuffer frames = GalaxyFrameBuffer();

  /// The bodies of the encoded scene.
  GalaxyBodies? bodies;

  void dispose() => frames.dispose();

  Float64List? _scene;
  Object? _key;

  Float64List prepare(
    GalaxyView view, {
    required LabelCache labels,
    required GalaxyLabelStyles styles,
    required Starfield starfield,
  }) {
    final key = (
      view.scene,
      view.revision,
      styles.base,
      styles.strong,
      styles.system,
    );
    final cached = _scene;
    if (cached != null && key == _key) return cached;
    final bodies = this.bodies = GalaxyBodies(view.scene);
    _key = key;
    return _scene = encodeScene(
      bodies,
      stars: starfield.data,
      systemLabel: (system) => labels.of(system.label, styles.system).size,
      planetLabel: (planet, {required strong}) =>
          labels.of(planet.label, strong ? styles.strong : styles.base).size,
      appeared: view.appeared,
      rippled: view.rippled,
    );
  }
}

/// The painter. Repaints whenever [view] ticks.
class GalaxyPainter extends CustomPainter {
  GalaxyPainter({
    required this.view,
    required this.palette,
    required this.labels,
    required this.geometry,
    required this.starfield,
    required this.onBackdrop,
  }) : super(repaint: view);

  final GalaxyView view;
  final GalaxyPalette palette;
  final LabelCache labels;
  final GalaxyGeometry geometry;
  final Starfield starfield;

  /// Called when the backdrop image arrives.
  final VoidCallback onBackdrop;

  @override
  void paint(Canvas canvas, Size size) {
    if (size.isEmpty) return;
    if (view.still) view.stopMotion();
    final camera = view.camera;
    final styles = GalaxyLabelStyles(palette);
    final scene = geometry.prepare(
      view,
      labels: labels,
      styles: styles,
      starfield: starfield,
    );
    final bodies = geometry.bodies!;
    encodeFrame(
      geometry.frames.frame,
      size: size,
      yaw: camera.yaw,
      tilt: camera.tilt,
      distance: camera.distance,
      target: camera.target,
      time: view.time,
      still: view.still,
      selected: bodies.indexOf(view.selected),
      hovered: bodies.indexOf(view.hovered),
      focused: bodies.indexOf(view.focused),
    );
    Backdrop.of(size, palette.dark, palette.ratio, onBackdrop).paint(canvas);
    final values = geometry.frames.compute(scene);
    if (values == null) return;
    final reader = FrameReader(values);
    starfield.paint(canvas, reader, dark: palette.dark);
    final frame = _Frame(this, canvas, reader, bodies, styles, size);
    frame.plane();
    frame.systems();
    frame.links();
    frame.ghosts();
    frame.labels();
    view.hits = frame.hits;
  }

  @override
  bool shouldRepaint(GalaxyPainter old) =>
      old.view != view || !old.palette.same(palette);
}

/// One frame's drawing, reading the computed frame in order.
class _Frame {
  _Frame(
    this.painter,
    this.canvas,
    this.reader,
    this.bodies,
    this.styles,
    this.size,
  );

  final GalaxyPainter painter;
  final Canvas canvas;
  final FrameReader reader;
  final GalaxyBodies bodies;
  final GalaxyLabelStyles styles;
  final Size size;
  final List<Hit> hits = [];

  GalaxyView get view => painter.view;
  GalaxyPalette get palette => painter.palette;
  bool get dark => palette.dark;

  /// Runs of points (count, then each run's points) as views of the frame.
  List<Float32List> _runs() => [
    for (var n = reader.count(); n > 0; n--) reader.points(reader.count()),
  ];

  static Path _path(Float32List run, [Path? into]) {
    final path = into ?? Path();
    path.moveTo(run[0], run[1]);
    for (var i = 2; i < run.length; i += 2) {
      path.lineTo(run[i], run[i + 1]);
    }
    return path;
  }

  void plane() {
    final glow = reader.next() != 0;
    final oval = Rect.fromLTRB(
      reader.next(),
      reader.next(),
      reader.next(),
      reader.next(),
    );
    final blur = reader.next();
    // A soft glow over the plane, with no edge to it.
    if (glow) {
      canvas.drawOval(
        oval,
        Paint()
          ..color = palette.ink.withValues(alpha: dark ? 0.035 : 0.03)
          ..maskFilter = MaskFilter.blur(BlurStyle.normal, math.max(1, blur)),
      );
    }
    // Faint guide circles give the plane its perspective, fading outwards.
    for (var guides = reader.count(); guides > 0; guides--) {
      final fade = reader.next();
      final path = Path();
      for (final run in _runs()) {
        if (run.isNotEmpty) _path(run, path);
      }
      canvas.drawPath(
        path,
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 0.8
          ..color = palette.ink.withValues(alpha: (dark ? 0.06 : 0.07) * fade),
      );
    }
  }

  void systems() {
    for (var n = reader.count(); n > 0; n--) {
      final system = bodies.systems[reader.count()];
      final c = Offset(reader.next(), reader.next());
      final radius = reader.next();
      final depth = reader.next();
      final base = reader.next();
      final back = _runs();
      final front = _runs();
      final planets = [
        for (var p = reader.count(); p > 0; p--)
          (
            planet: bodies.planets[reader.count()],
            center: Offset(reader.next(), reader.next()),
            radius: reader.next(),
            depth: reader.next(),
            opacity: reader.next(),
            ripple: reader.next(),
            behind: reader.next() != 0,
          ),
      ];
      // One orbit per ring, split at the star so the front half covers it.
      _strokeRuns(back, base * 0.55);
      for (final planet in planets) {
        if (planet.behind) _planet(planet);
      }
      _core(system, c, radius, depth, base);
      _strokeRuns(front, base);
      for (final planet in planets) {
        if (!planet.behind) _planet(planet);
      }
    }
  }

  void _strokeRuns(List<Float32List> runs, double opacity) {
    if (runs.isEmpty || opacity <= 0) return;
    final paint = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = dark ? 0.9 : 0.8
      ..color = palette.ink.withValues(alpha: (dark ? 0.16 : 0.22) * opacity);
    for (final run in runs) {
      if (run.length < 4) continue;
      canvas.drawPath(_path(run), paint);
    }
  }

  void _core(
    StarSystem system,
    Offset c,
    double radius,
    double depth,
    double opacity,
  ) {
    final online = system.online;
    final color = system.home
        ? palette.signal
        : online
        ? palette.star
        : palette.scheme.outline;
    if (dark && !online) {
      // An unreachable relay is a dim ember: no light of its own.
      paintSphere(
        canvas,
        c,
        radius,
        color,
        dark: true,
        glow: 0,
        opacity: opacity,
      );
    } else if (dark) {
      for (final (scale, alpha) in [(7.0, 0.06), (3.4, 0.16), (1.8, 0.32)]) {
        softDisc(
          canvas,
          c,
          radius * scale,
          radius * scale * 0.5,
          color.withValues(alpha: alpha * opacity),
        );
      }
      if (online) _spikes(c, radius * 3.2, 0.28 * opacity);
      // A white-hot core fading into the star's colour.
      canvas.drawCircle(
        c,
        radius,
        Paint()
          ..shader = RadialGradient(
            colors: [
              Colors.white.withValues(alpha: opacity),
              color.withValues(alpha: opacity),
              color.withValues(alpha: 0.85 * opacity),
            ],
            stops: const [0, 0.55, 1],
          ).createShader(Rect.fromCircle(center: c, radius: radius)),
      );
    } else {
      softDisc(
        canvas,
        c,
        radius * 2.6,
        radius * 1.4,
        color.withValues(alpha: 0.14 * opacity),
      );
      paintSphere(canvas, c, radius, color, dark: false, opacity: opacity);
    }
    _focusRings(system.id, c, radius);
    hits.add(Hit(system.id, c, radius * 1.6, depth));
  }

  /// Faint diffraction spikes on a bright star.
  void _spikes(Offset c, double length, double alpha) {
    for (final angle in [0.0, math.pi / 2]) {
      final d = Offset(math.cos(angle), math.sin(angle)) * length;
      canvas.drawLine(
        c - d,
        c + d,
        Paint()
          ..strokeWidth = 0.8
          ..shader = LinearGradient(
            colors: [
              Colors.white.withValues(alpha: 0),
              Colors.white.withValues(alpha: alpha),
              Colors.white.withValues(alpha: 0),
            ],
          ).createShader(Rect.fromPoints(c - d, c + d)),
      );
    }
  }

  void _planet(
    ({
      Planet planet,
      Offset center,
      double radius,
      double depth,
      double opacity,
      double ripple,
      bool behind,
    })
    body,
  ) {
    final planet = body.planet;
    final (c, radius, opacity) = (body.center, body.radius, body.opacity);
    final color = palette.planet(planet);
    // A ripple crosses the ring when the state changes.
    final t = body.ripple;
    if (t >= 0) {
      canvas.drawCircle(
        c,
        radius * (1.2 + t * 3.2),
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1.4
          ..color = color.withValues(alpha: (1 - t) * 0.7 * opacity),
      );
    }
    paintSphere(
      canvas,
      c,
      radius,
      color,
      dark: dark,
      opacity: opacity,
      glow: 0.9,
    );
    // A connection is drawn as a moon-ringed body, so the two ends of a
    // tunnel are told apart before a label is read.
    if (planet.kind == BodyKind.connect) {
      canvas.drawCircle(
        c,
        radius * 1.75,
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 0.9
          ..color = palette.ink.withValues(
            alpha: (dark ? 0.3 : 0.34) * opacity,
          ),
      );
    }
    _focusRings(planet.id, c, radius);
    hits.add(Hit(planet.id, c, radius, body.depth));
  }

  void _focusRings(String id, Offset c, double radius) {
    final pulse = view.still ? 0.0 : (math.sin(view.time * 3) + 1) / 2;
    if (view.selected == id || view.focused == id) {
      canvas.drawCircle(
        c,
        radius + 5 + pulse * 2,
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1.5
          ..color = palette.ink.withValues(alpha: 0.75),
      );
    } else if (view.hovered == id) {
      canvas.drawCircle(
        c,
        radius + 4,
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1
          ..color = palette.ink.withValues(alpha: 0.4),
      );
    }
  }

  void links() {
    final light = palette.signal;
    for (var n = reader.count(); n > 0; n--) {
      final curve = reader.points(reader.count());
      final particles = reader.points(reader.count());
      final last = curve.length - 2;
      final bounds = Rect.fromPoints(
        Offset(curve[0], curve[1]),
        Offset(curve[last], curve[last + 1]),
      ).inflate(1);
      canvas.drawPath(
        _path(curve),
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1.6
          ..strokeCap = StrokeCap.round
          ..shader = LinearGradient(
            colors: [
              light.withValues(alpha: dark ? 0.7 : 0.55),
              light.withValues(alpha: dark ? 0.15 : 0.12),
            ],
          ).createShader(bounds),
      );
      // Particles run along the tunnel, through the relay.
      for (var i = 0; i < particles.length; i += 2) {
        softDisc(
          canvas,
          Offset(particles[i], particles[i + 1]),
          dark ? 2.2 : 1.8,
          dark ? 2.2 : 1,
          light.withValues(alpha: 0.9),
        );
      }
    }
  }

  void ghosts() {
    view.ghosts.removeWhere((g) => view.time - g.since > 1.0);
    if (view.ghosts.isEmpty) return;
    final projection = view.camera.project(size);
    for (final ghost in view.ghosts) {
      final p = projection.at(ghost.position);
      if (p == null) continue;
      final t = (view.time - ghost.since).clamp(0.0, 1.0);
      paintSphere(
        canvas,
        p.point,
        math.max(2.0, ghost.size * p.scale) * (1 + t * 0.4),
        palette.scheme.outline,
        dark: dark,
        opacity: 1 - t,
      );
    }
  }

  void labels() {
    for (var n = reader.count(); n > 0; n--) {
      final body = reader.count();
      final strong = reader.next() != 0;
      final offset = Offset(reader.next(), reader.next());
      if (body < bodies.systems.length) {
        painter.labels
            .of(bodies.systems[body].label, styles.system)
            .paint(canvas, offset);
      } else {
        painter.labels
            .of(
              bodies.planets[body - bodies.systems.length].label,
              strong ? styles.strong : styles.base,
            )
            .paint(canvas, offset);
      }
    }
  }
}
