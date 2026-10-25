/// One frame of the galaxy's geometry, computed in Rust: the scene is encoded
/// when it changes, the camera and clock every frame, and a single leaf
/// `dart:ffi` call (`pb_mapper_galaxy_frame_v1`, crate `pb-mapper-galaxy`)
/// fills a buffer the painter only reads. Where the library is not loaded
/// (`flutter test`), [computeFrameInDart], a step-for-step port of the Rust
/// code, fills the same buffer.
///
/// The geometry and both layouts follow POCOX's galaxy engine unchanged; see
/// `docs/ui-design.md`.
library;

import 'dart:ffi' hide Size;
import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui';

import 'package:ffi/ffi.dart' show malloc;
import 'package:pb_mapper_ui/src/ffi/pb_mapper_ffi.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';

/// The layouts' version, the same on both ends.
const double galaxyFrameVersion = 2;

/// The frame buffer's length.
const int galaxyFrameFields = 13;

const int _systemFields = 9;
const int _planetFields = 14;

typedef _FrameNative =
    Int64 Function(
      Pointer<Double>,
      Uint32,
      Pointer<Double>,
      Uint32,
      Pointer<Float>,
      Uint32,
    );
typedef _FrameDart =
    int Function(
      Pointer<Double>,
      int,
      Pointer<Double>,
      int,
      Pointer<Float>,
      int,
    );

/// The Rust computation, once the FFI library is found.
abstract final class GalaxyNative {
  static _FrameDart? _frame;
  static bool _tried = false;

  /// Whether frames are computed in Rust.
  static bool get available => _frame != null;

  /// Find the export in the library the app already loaded. A test without
  /// the library, or an older library without the export, keeps the Dart
  /// computation, which draws the same frames.
  static void load() {
    if (_tried) return;
    _tried = true;
    try {
      use(PbMapperFFI().lib);
    } catch (_) {
      _frame = null;
    }
  }

  /// Use `pb_mapper_galaxy_frame_v1` from [library].
  static void use(DynamicLibrary library) {
    _frame = library.lookupFunction<_FrameNative, _FrameDart>(
      'pb_mapper_galaxy_frame_v1',
      isLeaf: true,
    );
  }

  /// Back to the Dart computation (tests).
  static void reset() {
    _frame = null;
    _tried = false;
  }
}

/// Every body of a scene with its index in the buffers: systems first, then
/// their planets in order.
class GalaxyBodies {
  GalaxyBodies(this.scene) {
    for (final system in scene.systems) {
      _index[system.id] = _index.length;
    }
    for (final system in scene.systems) {
      for (final planet in system.planets) {
        _index[planet.id] = systems.length + planets.length;
        planets.add(planet);
        owners.add(system);
      }
    }
  }

  final GalaxyScene scene;

  List<StarSystem> get systems => scene.systems;

  /// Planets, system by system.
  final List<Planet> planets = [];

  /// Each planet's system.
  final List<StarSystem> owners = [];

  final Map<String, int> _index = {};

  /// The index of body [id]: -1 for none, -2 for one this scene lacks.
  int indexOf(String? id) => id == null ? -1 : (_index[id] ?? -2);
}

/// One layer of background stars: per bucket a twinkle phase and interleaved
/// unit coordinates.
typedef StarLayerData = ({
  double parallax,
  List<({double phase, Float32List points})> buckets,
});

/// The scene buffer. Layout (all f64): version, systems, planets, links, star
/// layers; per system x, y, z, home, online, first planet, planet count, label
/// width, label height; per planet system, ring, radius, phase, period, size,
/// tilt axis, tilt, appeared, rippled, label width and height, strong label
/// width and height; per link planet, the body it ends at (systems first,
/// then planets) and direct (0/1); per star layer parallax, buckets, then
/// per bucket phase, count and the unit coordinates.
Float64List encodeScene(
  GalaxyBodies bodies, {
  required List<StarLayerData> stars,
  required Size Function(StarSystem system) systemLabel,
  required Size Function(Planet planet, {required bool strong}) planetLabel,
  required Map<String, double> appeared,
  required Map<String, double> rippled,
}) {
  final values = <double>[
    galaxyFrameVersion,
    bodies.systems.length.toDouble(),
    bodies.planets.length.toDouble(),
    bodies.scene.links.length.toDouble(),
    stars.length.toDouble(),
  ];
  var first = 0;
  for (final system in bodies.systems) {
    final label = systemLabel(system);
    values.addAll([
      system.position.x,
      system.position.y,
      system.position.z,
      system.home ? 1 : 0,
      system.online ? 1 : 0,
      first.toDouble(),
      system.planets.length.toDouble(),
      label.width,
      label.height,
    ]);
    first += system.planets.length;
  }
  for (final (i, planet) in bodies.planets.indexed) {
    final base = planetLabel(planet, strong: false);
    final strong = planetLabel(planet, strong: true);
    values.addAll([
      bodies.indexOf(bodies.owners[i].id).toDouble(),
      planet.ring.toDouble(),
      planet.radius,
      planet.phase,
      planet.period,
      planet.size,
      planet.tiltAxis,
      planet.tilt,
      appeared[planet.id] ?? double.nan,
      rippled[planet.id] ?? double.nan,
      base.width,
      base.height,
      strong.width,
      strong.height,
    ]);
  }
  for (final link in bodies.scene.links) {
    values
      ..add((bodies.indexOf(link.planetId) - bodies.systems.length).toDouble())
      ..add(bodies.indexOf(link.targetId).toDouble())
      ..add(link.direct ? 1 : 0);
  }
  for (final layer in stars) {
    values
      ..add(layer.parallax)
      ..add(layer.buckets.length.toDouble());
    for (final bucket in layer.buckets) {
      values
        ..add(bucket.phase)
        ..add((bucket.points.length ~/ 2).toDouble());
      for (final unit in bucket.points) {
        values.add(unit);
      }
    }
  }
  return Float64List.fromList(values);
}

/// Write the frame buffer for this camera, clock and selection into [into].
/// Layout (all f64): width, height, yaw, tilt, distance, target x, y, z, time,
/// still, then the selected, hovered and focused bodies (systems first, then
/// planets; -1 none, -2 one this scene does not hold).
void encodeFrame(
  Float64List into, {
  required Size size,
  required double yaw,
  required double tilt,
  required double distance,
  required Vec3 target,
  required double time,
  required bool still,
  required int selected,
  required int hovered,
  required int focused,
}) {
  into
    ..[0] = size.width
    ..[1] = size.height
    ..[2] = yaw
    ..[3] = tilt
    ..[4] = distance
    ..[5] = target.x
    ..[6] = target.y
    ..[7] = target.z
    ..[8] = time
    ..[9] = still ? 1 : 0
    ..[10] = selected.toDouble()
    ..[11] = hovered.toDouble()
    ..[12] = focused.toDouble();
}

/// Computes frames into buffers it keeps, growing them when a frame needs
/// more room. In Rust the buffers are native memory: the scene is copied in
/// only when it changes, the frame's few values every frame, and the result is
/// read in place. Call [dispose] to free them.
class GalaxyFrameBuffer {
  /// The frame's values, filled by [encodeFrame].
  final Float64List frame = Float64List(galaxyFrameFields);

  Float32List _out = Float32List(4096);

  Float64List? _source;
  Pointer<Double> _scene = nullptr;
  int _sceneLength = 0;
  int _sceneCapacity = 0;
  Pointer<Double> _frame = nullptr;
  Pointer<Float> _result = nullptr;
  int _resultCapacity = 0;

  /// The frame of [scene] for [frame], or null when either is malformed. The
  /// values stay valid until the next call.
  Float32List? compute(Float64List scene) {
    final native = GalaxyNative._frame;
    for (var attempt = 0; attempt < 3; attempt++) {
      final int written;
      if (native != null) {
        _upload(scene);
        written = native(
          _scene,
          _sceneLength,
          _frame,
          galaxyFrameFields,
          _result,
          _resultCapacity,
        );
        if (written >= 0) return _result.asTypedList(written);
      } else {
        written = computeFrameInDart(scene, frame, _out);
        if (written >= 0) return Float32List.sublistView(_out, 0, written);
      }
      if (written == -1) return null;
      _grow(-written + 1024);
    }
    return null;
  }

  void _upload(Float64List scene) {
    if (_frame == nullptr) {
      _frame = malloc<Double>(galaxyFrameFields);
      _grow(4096);
    }
    _frame.asTypedList(galaxyFrameFields).setAll(0, frame);
    if (identical(scene, _source)) return;
    if (scene.length > _sceneCapacity) {
      if (_scene != nullptr) malloc.free(_scene);
      _scene = malloc<Double>(scene.length);
      _sceneCapacity = scene.length;
    }
    _scene.asTypedList(scene.length).setAll(0, scene);
    _sceneLength = scene.length;
    _source = scene;
  }

  void _grow(int length) {
    _out = Float32List(length);
    if (_frame != nullptr) {
      if (_result != nullptr) malloc.free(_result);
      _result = malloc<Float>(length);
      _resultCapacity = length;
    }
  }

  /// Frees the native buffers.
  void dispose() {
    for (final pointer in [_scene, _frame, _result]) {
      if (pointer != nullptr) malloc.free(pointer);
    }
    _scene = _frame = nullptr;
    _result = nullptr;
    _source = null;
    _sceneCapacity = _resultCapacity = 0;
  }
}

/// Sequential reader of a frame's values.
class FrameReader {
  FrameReader(this.values);

  final Float32List values;

  /// The read position.
  int at = 0;

  double next() => values[at++];

  /// The next value as a count or index.
  int count() => values[at++].toInt();

  /// The next [n] points (interleaved x, y) as a view, without copying.
  Float32List points(int n) {
    final view = Float32List.sublistView(values, at, at + n * 2);
    at += n * 2;
    return view;
  }
}

// --- The Dart computation: a step-for-step port of `pb-mapper-galaxy`. ---

class _System {
  _System(
    this.position,
    this.home,
    this.online,
    this.first,
    this.count,
    this.label,
  );
  final Vec3 position;
  final bool home;
  final bool online;
  final int first;
  final int count;
  final (double, double) label;
}

class _Planet {
  _Planet(List<double> f)
    : system = f[0].toInt(),
      ring = f[1].toInt(),
      radius = f[2],
      phase = f[3],
      period = f[4],
      size = f[5],
      tiltAxis = f[6],
      tilt = f[7],
      appeared = f[8],
      rippled = f[9],
      label = (f[10], f[11]),
      strong = (f[12], f[13]);
  final int system;
  final int ring;
  final double radius, phase, period, size, tiltAxis, tilt, appeared, rippled;
  final (double, double) label;
  final (double, double) strong;

  Vec3 offsetAt(double t) {
    final angle = phase + (t / period) * math.pi * 2;
    final flat = Vec3(math.cos(angle) * radius, 0, math.sin(angle) * radius);
    return tiltAround(flat, tiltAxis, tilt);
  }
}

typedef _Projected = ({double x, double y, double scale, double depth});

class _Camera {
  _Camera(Float64List f)
    : cx = f[0] / 2,
      cy = f[1] * 0.52,
      focal = math.min(f[0], f[1]) * 0.92 {
    final target = Vec3(f[5], f[6], f[7]);
    final dir = Vec3(
      math.cos(f[3]) * math.sin(f[2]),
      math.sin(f[3]),
      math.cos(f[3]) * math.cos(f[2]),
    );
    eye = target + dir * f[4];
    forward = (target - eye).unit;
    right = forward.cross(const Vec3(0, 1, 0)).unit;
    up = right.cross(forward);
  }

  late final Vec3 eye, forward, right, up;
  final double cx, cy, focal;

  _Projected? at(Vec3 p) {
    final v = p - eye;
    final depth = v.dot(forward);
    if (depth < 0.2) return null;
    final s = focal / depth;
    return (
      x: cx + v.dot(right) * s,
      y: cy - v.dot(up) * s,
      scale: s,
      depth: depth,
    );
  }
}

class _Out {
  _Out(this.buf);
  final Float32List buf;
  int len = 0;

  void push(double value) {
    if (len < buf.length) buf[len] = value;
    len++;
  }

  int hole() {
    push(0);
    return len - 1;
  }

  void fill(int at, double value) {
    if (at < buf.length) buf[at] = value;
  }

  void points(List<(double, double)> points) {
    push(points.length.toDouble());
    for (final (x, y) in points) {
      push(x);
      push(y);
    }
  }
}

double _easeOut(double t) => 1 - math.pow(1 - t.clamp(0.0, 1.0), 3).toDouble();

Vec3 _turned(Vec3 p, double angle) {
  final c = math.cos(angle), s = math.sin(angle);
  return Vec3(p.x * c - p.z * s, p.y, p.x * s + p.z * c);
}

bool _count(double value) =>
    value.isFinite && value >= 0 && value == value.truncateToDouble();

/// The frame of [scene] for [frame] into [out], with the same result
/// convention as the Rust export: the length written, minus the length needed
/// when [out] is too short, or -1 for a malformed buffer.
int computeFrameInDart(Float64List scene, Float64List frame, Float32List out) {
  if (scene.length < 5 ||
      scene[0] != galaxyFrameVersion ||
      frame.length != galaxyFrameFields ||
      !(frame[0] > 0 && frame[1] > 0 && frame[4] > 0)) {
    return -1;
  }
  for (var i = 1; i < 5; i++) {
    if (!_count(scene[i])) return -1;
  }
  final nSystems = scene[1].toInt(), nPlanets = scene[2].toInt();
  final nLinks = scene[3].toInt(), nLayers = scene[4].toInt();
  var at = 5;
  List<double>? take(int n) {
    if (at + n > scene.length) return null;
    final slice = scene.sublist(at, at + n);
    at += n;
    return slice;
  }

  final systems = <_System>[];
  for (var i = 0; i < nSystems; i++) {
    final f = take(_systemFields);
    if (f == null || !_count(f[5]) || !_count(f[6])) return -1;
    systems.add(
      _System(
        Vec3(f[0], f[1], f[2]),
        f[3] != 0,
        f[4] != 0,
        f[5].toInt(),
        f[6].toInt(),
        (f[7], f[8]),
      ),
    );
  }
  final planets = <_Planet>[];
  for (var i = 0; i < nPlanets; i++) {
    final f = take(_planetFields);
    if (f == null || !_count(f[0]) || f[0] >= nSystems) return -1;
    planets.add(_Planet(f));
  }
  if (systems.any((s) => s.first + s.count > nPlanets)) return -1;
  final links = <(int, int, bool)>[];
  for (var i = 0; i < nLinks; i++) {
    final f = take(3);
    if (f == null || !_count(f[0]) || !_count(f[1])) return -1;
    if (f[0] >= nPlanets || f[1] >= nSystems + nPlanets) return -1;
    links.add((f[0].toInt(), f[1].toInt(), f[2] != 0));
  }
  final layers = <(double, List<(double, int, int)>)>[];
  for (var i = 0; i < nLayers; i++) {
    final head = take(2);
    if (head == null || !_count(head[1])) return -1;
    final buckets = <(double, int, int)>[];
    for (var b = 0; b < head[1].toInt(); b++) {
      final bucket = take(2);
      if (bucket == null || !_count(bucket[1])) return -1;
      final start = at;
      if (take(bucket[1].toInt() * 2) == null) return -1;
      buckets.add((bucket[0], start, start + bucket[1].toInt() * 2));
    }
    layers.add((head[0], buckets));
  }

  final width = frame[0], height = frame[1], distance = frame[4];
  final time = frame[8];
  final still = frame[9] != 0;
  final selected = frame[10].toInt();
  final hovered = frame[11].toInt();
  final focused = frame[12].toInt();
  final orbitTime = still ? 0.0 : time;
  final camera = _Camera(frame);
  final o = _Out(out);

  double extent(_System s) {
    if (s.count == 0) return 0.4;
    var max = double.negativeInfinity;
    for (var p = s.first; p < s.first + s.count; p++) {
      max = math.max(max, planets[p].radius);
    }
    return max + 0.2;
  }

  List<List<(double, double)>> runs(int steps, Vec3 Function(int) point) {
    final runs = <List<(double, double)>>[];
    var open = false;
    for (var i = 0; i <= steps; i++) {
      final p = camera.at(point(i));
      if (p == null) {
        open = false;
      } else if (open) {
        runs.last.add((p.x, p.y));
      } else {
        runs.add([(p.x, p.y)]);
        open = true;
      }
    }
    return runs;
  }

  // Background stars.
  for (final (parallax, buckets) in layers) {
    final shiftX = (frame[2] / (math.pi * 2)) * width * parallax * 2.2;
    final shiftY = (frame[3] - 0.52) * height * parallax * 0.6;
    for (final (b, (phase, start, end)) in buckets.indexed) {
      o.push(
        still ? 0.8 : 0.7 + 0.3 * math.sin(time * (0.6 + b * 0.23) + phase),
      );
      o.push(((end - start) ~/ 2).toDouble());
      for (var i = start; i < end; i += 2) {
        o.push((scene[i] * width + shiftX) % width);
        o.push((scene[i + 1] * height + shiftY) % height);
      }
    }
  }

  // The plane.
  if (systems.isEmpty) {
    for (var i = 0; i < 7; i++) {
      o.push(0);
    }
  } else {
    var reach = 3.0;
    for (final s in systems) {
      reach = math.max(reach, s.position.length + extent(s));
    }
    final rim = <(double, double)>[];
    for (var i = 0; i < 48; i++) {
      final a = i / 48 * math.pi * 2;
      final p = camera.at(Vec3(math.cos(a) * reach, 0, math.sin(a) * reach));
      if (p != null) rim.add((p.x, p.y));
    }
    if (rim.length > 8) {
      var (l, t, r, b) = (rim[0].$1, rim[0].$2, rim[0].$1, rim[0].$2);
      for (final (x, y) in rim) {
        l = math.min(l, x);
        t = math.min(t, y);
        r = math.max(r, x);
        b = math.max(b, y);
      }
      final shortest = math.min(r - l, b - t);
      final inset = shortest * 0.12;
      for (final v in [1.0, l + inset, t + inset, r - inset, b - inset]) {
        o.push(v);
      }
      o.push(shortest * 0.22);
    } else {
      for (var i = 0; i < 6; i++) {
        o.push(0);
      }
    }
    final guides = o.hole();
    var n = 0;
    for (var r = 2.6; r < reach; r += 2.6) {
      o.push(1 - r / (reach * 1.15));
      final guide = runs(96, (i) {
        final a = i / 96 * math.pi * 2;
        return Vec3(math.cos(a) * r, 0, math.sin(a) * r);
      });
      o.push(guide.length.toDouble());
      for (final run in guide) {
        o.points(run);
      }
      n++;
    }
    o.fill(guides, n.toDouble());
  }

  bool lit(int body, int system) {
    if (selected == -1) return true;
    if (selected == body || selected == system) return true;
    final s = systems[system];
    return body == system &&
        selected >= nSystems + s.first &&
        selected < nSystems + s.first + s.count;
  }

  double fade(double depth) =>
      (1.25 - (depth - distance) / (distance * 1.6)).clamp(0.42, 1.0);

  // Systems, far to near.
  final ordered = <(int, _Projected)>[
    for (final (i, s) in systems.indexed)
      if (camera.at(s.position) case final p?) (i, p),
  ];
  _stableSort(ordered, (a, b) => b.$2.depth.compareTo(a.$2.depth));
  final hits = <(int, double, double, double)>[];
  o.push(ordered.length.toDouble());
  for (final (index, core) in ordered) {
    final system = systems[index];
    final online = system.online;
    final base =
        (online ? 1.0 : 0.45) *
        (lit(index, index) ? 1.0 : 0.3) *
        fade(core.depth);
    final breath = still
        ? 1.0
        : 1 + 0.04 * math.sin(time * 0.9 + system.position.x);
    final ceiling = system.home ? 26.0 : (online ? 15.0 : 11.0);
    final radius =
        ((system.home ? 0.22 : 0.15) * core.scale).clamp(3.0, ceiling) * breath;
    for (final v in [
      index.toDouble(),
      core.x,
      core.y,
      radius,
      core.depth,
      base,
    ]) {
      o.push(v);
    }
    final own = planets.sublist(system.first, system.first + system.count);
    final rings = <_Planet>[];
    for (final planet in own) {
      if (!rings.any((r) => r.ring == planet.ring)) rings.add(planet);
    }
    final back = <List<(double, double)>>[];
    final front = <List<(double, double)>>[];
    for (final ring in rings) {
      List<(double, double)>? run;
      bool? behindRun;
      for (var i = 0; i <= 96; i++) {
        final a = i / 96 * math.pi * 2;
        final local = tiltAround(
          Vec3(math.cos(a) * ring.radius, 0, math.sin(a) * ring.radius),
          ring.tiltAxis,
          ring.tilt,
        );
        final p = camera.at(system.position + local);
        if (p == null) {
          run = null;
          continue;
        }
        final behind = p.depth > core.depth;
        if (run == null || behind != behindRun) {
          final previous = run?.last;
          run = [?previous, (p.x, p.y)];
          (behind ? back : front).add(run);
          behindRun = behind;
        } else {
          run.add((p.x, p.y));
        }
      }
    }
    for (final side in [back, front]) {
      o.push(side.length.toDouble());
      for (final run in side) {
        o.points(run);
      }
    }
    final count = o.hole();
    final placed = <(int, _Projected, double, bool)>[];
    for (final (offset, planet) in own.indexed) {
      final p = system.first + offset;
      final appear = still || planet.appeared.isNaN
          ? 1.0
          : _easeOut((time - planet.appeared) / 1.6);
      final orbit = planet.offsetAt(orbitTime);
      final local = appear >= 1
          ? orbit
          : _turned(orbit, (1 - appear) * math.pi) * appear +
                Vec3(0, 0.25 * (1 - appear), 0);
      final at = camera.at(system.position + local);
      if (at == null) continue;
      final opacity =
          (online ? 1.0 : 0.45) *
          (lit(nSystems + p, index) ? 1.0 : 0.3) *
          fade(at.depth) *
          appear;
      final radius =
          (planet.size * at.scale).clamp(2.5, 13.0) * (0.4 + 0.6 * appear);
      var ripple = -1.0;
      if (!still && !planet.rippled.isNaN) {
        final t = (time - planet.rippled) / 1.4;
        if (t < 1) ripple = t;
      }
      final behind = at.depth > core.depth;
      for (final v in [
        p.toDouble(),
        at.x,
        at.y,
        radius,
        at.depth,
        opacity,
        ripple,
        behind ? 1.0 : 0.0,
      ]) {
        o.push(v);
      }
      placed.add((nSystems + p, at, radius, behind));
    }
    o.fill(count, placed.length.toDouble());
    for (final (body, at, r, behind) in placed) {
      if (behind) hits.add((body, at.x, at.y, r));
    }
    hits.add((index, core.x, core.y, radius * 1.6));
    for (final (body, at, r, behind) in placed) {
      if (!behind) hits.add((body, at.x, at.y, r));
    }
  }

  // Links.
  final linkCount = o.hole();
  var drawnLinks = 0;
  Vec3 planetAt(int index) {
    final p = planets[index];
    return systems[p.system].position + p.offsetAt(orbitTime);
  }

  for (final (index, (planet, toIndex, direct)) in links.indexed) {
    final from = planetAt(planet);
    final to = toIndex >= nSystems
        ? planetAt(toIndex - nSystems)
        : systems[toIndex].position;
    final rise = (to - from).length * 0.42 + 0.4;
    final control = from.lerp(to, 0.5) + Vec3(0, rise, 0);
    Vec3 curve(double t) =>
        from * ((1 - t) * (1 - t)) + control * (2 * (1 - t) * t) + to * (t * t);
    final points = <(double, double)>[
      for (var i = 0; i <= 32; i++)
        if (camera.at(curve(i / 32)) case final q?) (q.x, q.y),
    ];
    if (points.length < 2) continue;
    o.push(index.toDouble());
    o.points(points);
    final speed = direct ? 0.34 : 0.22;
    o.points([
      for (var k = 0; k < 3; k++)
        if (camera.at(curve(still ? (k + 1) / 4 : (time * speed + k / 3) % 1.0))
            case final q?)
          (q.x, q.y),
    ]);
    drawnLinks++;
  }
  o.fill(linkCount, drawnLinks.toDouble());

  // Labels.
  bool emphasized(int body) =>
      body == selected || body == hovered || body == focused;
  int rank((int, double, double, double) hit) => emphasized(hit.$1)
      ? 3
      : hit.$1 < nSystems
      ? 2
      : 1;
  final order = [for (var i = 0; i < hits.length; i++) i];
  _stableSort(order, (a, b) {
    final byRank = rank(hits[b]).compareTo(rank(hits[a]));
    return byRank != 0 ? byRank : hits[b].$4.compareTo(hits[a].$4);
  });
  final stars = [
    for (final hit in hits)
      if (hit.$1 < nSystems)
        Rect.fromCircle(center: Offset(hit.$2, hit.$3), radius: hit.$4 * 0.7),
  ];
  final placedLabels = <Rect>[];
  final labelCount = o.hole();
  var shown = 0;
  for (final i in order) {
    final (body, x, y, radius) = hits[i];
    final device = body < nSystems;
    final emphasis = emphasized(body);
    if (!emphasis && !device && radius < 5) continue;
    if (shown >= 28 && !emphasis) break;
    final strong = device || emphasis;
    final (w, h) = device
        ? systems[body].label
        : (strong
              ? planets[body - nSystems].strong
              : planets[body - nSystems].label);
    final left = x - w / 2;
    final top = y + radius + (device ? 8 : 5);
    final rect = Rect.fromLTWH(left, top, w, h).inflate(3);
    if (!emphasis &&
        (placedLabels.any(rect.overlaps) ||
            (!device && stars.any(rect.overlaps)))) {
      continue;
    }
    if (rect.right < 0 || rect.left > width || rect.top > height) continue;
    placedLabels.add(rect);
    for (final v in [body.toDouble(), strong ? 1.0 : 0.0, left, top]) {
      o.push(v);
    }
    shown++;
  }
  o.fill(labelCount, shown.toDouble());
  return o.len <= out.length ? o.len : -o.len;
}

/// Insertion sort: stable, as Rust's `sort_by`, so equal keys keep their order
/// on both ends (Dart's own sort is not stable past 32 elements).
void _stableSort<T>(List<T> list, int Function(T a, T b) compare) {
  for (var i = 1; i < list.length; i++) {
    final item = list[i];
    var j = i - 1;
    while (j >= 0 && compare(list[j], item) > 0) {
      list[j + 1] = list[j];
      j--;
    }
    list[j + 1] = item;
  }
}
