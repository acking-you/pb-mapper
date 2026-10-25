/// The galaxy's scene: this computer and the relay as two star systems, the
/// tunnels and the relay's services as their planets, live tunnels as arcs of
/// light between them. Pure data and arithmetic; nothing here paints.
///
/// Laid out after POCOX's galaxy, where every device is a star. pb-mapper has
/// two ends that matter — the computer in the user's hands and the relay every
/// tunnel crosses — so those are the stars, and each keeps its place between
/// visits because every position is drawn from a stable hash.
library;

import 'dart:math' as math;

import 'package:pb_mapper_ui/src/state/remote_service.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';

/// A point or direction in the galaxy's world (the disk is the x–z plane, y
/// points up out of it).
class Vec3 {
  const Vec3(this.x, this.y, this.z);

  final double x, y, z;

  static const zero = Vec3(0, 0, 0);

  Vec3 operator +(Vec3 o) => Vec3(x + o.x, y + o.y, z + o.z);
  Vec3 operator -(Vec3 o) => Vec3(x - o.x, y - o.y, z - o.z);
  Vec3 operator *(double k) => Vec3(x * k, y * k, z * k);

  double dot(Vec3 o) => x * o.x + y * o.y + z * o.z;

  Vec3 cross(Vec3 o) =>
      Vec3(y * o.z - z * o.y, z * o.x - x * o.z, x * o.y - y * o.x);

  double get length => math.sqrt(dot(this));

  Vec3 get unit {
    final l = length;
    return l == 0 ? this : this * (1 / l);
  }

  Vec3 lerp(Vec3 o, double t) => this + (o - this) * t;
}

/// A stable 32-bit hash of [text] (FNV-1a), so a body keeps its place between
/// visits and runs.
int stableHash(String text) {
  var hash = 0x811c9dc5;
  for (final unit in text.codeUnits) {
    hash ^= unit;
    hash = (hash * 0x01000193) & 0xffffffff;
  }
  return hash;
}

/// [hash] as a fraction in [0, 1).
double unitOf(int hash) => (hash & 0xffffff) / 0x1000000;

/// [p] rotated by [tilt] around the in-plane axis at angle [axis]
/// (Rodrigues' rotation around k = (cos axis, 0, sin axis)).
Vec3 tiltAround(Vec3 p, double axis, double tilt) {
  final k = Vec3(math.cos(axis), 0, math.sin(axis));
  final cos = math.cos(tilt), sin = math.sin(tilt);
  return p * cos + k.cross(p) * sin + k * (k.dot(p) * (1 - cos));
}

/// What a planet is.
enum BodyKind {
  /// A service this computer shares.
  share,

  /// A service this computer connects to.
  connect,

  /// A service the relay lists that this computer does not share.
  remote,
}

/// One service, orbiting its star.
class Planet {
  const Planet({
    required this.id,
    required this.key,
    required this.kind,
    required this.ring,
    required this.radius,
    required this.phase,
    required this.period,
    required this.size,
    required this.tiltAxis,
    required this.tilt,
    this.tunnel,
    this.inUse = false,
    String? label,
  }) : label = label ?? key;

  /// Stable id: the tunnel's id, or `remote:<key>`.
  final String id;

  /// The service key, or for a [BodyKind.remote] planet its id on the relay
  /// (see [relayServiceId]).
  final String key;

  /// What it is called in the sky: the service's name, and whose it is when
  /// it is another credential's.
  final String label;
  final BodyKind kind;

  /// The tunnel it stands for, for a share or a connection.
  final Tunnel? tunnel;

  /// For a [BodyKind.remote] planet: whether this computer is connected to
  /// it and the connection runs.
  final bool inUse;

  /// Its ring, counted out from the star.
  final int ring;

  /// Orbit radius in world units.
  final double radius;

  /// Where on the orbit it starts, in radians.
  final double phase;

  /// One orbit, in seconds.
  final double period;

  /// Body radius in world units.
  final double size;

  /// The orbit plane's tilt, around this direction in the disk.
  final double tiltAxis;

  /// The orbit plane's tilt, in radians.
  final double tilt;

  /// What changes its look, so a change can ripple across its ring.
  String get state => tunnel?.health.name ?? (inUse ? 'inUse' : 'available');

  /// Its position relative to its star at time [t] (seconds).
  Vec3 offsetAt(double t) {
    final angle = phase + (t / period) * math.pi * 2;
    final flat = Vec3(math.cos(angle) * radius, 0, math.sin(angle) * radius);
    return tiltAround(flat, tiltAxis, tilt);
  }
}

/// One star and its planets.
class StarSystem {
  const StarSystem({
    required this.id,
    required this.label,
    required this.position,
    required this.planets,
    required this.home,
    required this.online,
  });

  /// [homeId] or [relayId].
  final String id;
  final String label;
  final Vec3 position;
  final List<Planet> planets;

  /// Whether this is the computer in the user's hands.
  final bool home;
  final bool online;

  static const homeId = 'system:home';
  static const relayId = 'system:relay';

  /// The radius its outermost orbit reaches.
  double get extent => planets.isEmpty
      ? 0.4
      : planets.map((p) => p.radius).reduce(math.max) + 0.2;
}

/// A tunnel carrying traffic: an arc from its planet to the relay.
class Link {
  const Link({required this.planetId, required this.systemId});

  final String planetId;
  final String systemId;
}

/// Everything the galaxy shows.
class GalaxyScene {
  const GalaxyScene({required this.systems, required this.links});

  static const empty = GalaxyScene(systems: [], links: []);

  /// Star systems, this computer first.
  final List<StarSystem> systems;
  final List<Link> links;

  ({StarSystem system, Planet planet})? planetOf(String id) {
    for (final system in systems) {
      for (final planet in system.planets) {
        if (planet.id == id) return (system: system, planet: planet);
      }
    }
    return null;
  }

  StarSystem? systemOf(String id) {
    for (final system in systems) {
      if (system.id == id) return system;
    }
    return null;
  }
}

/// Up to this many bodies share a ring; more open the next one out.
const int _perRing = 6;

/// The line through the two stars, in the disk: across the resting camera's
/// view (its yaw is 0.6, so screen-right is at -0.6), turned 0.35 rad away
/// from it.
const double _pairAngle = -0.95;

/// Lay out this computer's tunnels and the relay's services as a scene.
///
/// [relayAddress] null leaves the relay out (none configured). Labels are
/// passed in so the model needs no localisation.
GalaxyScene buildScene({
  required List<Tunnel> shares,
  required List<Tunnel> connections,
  required String? relayAddress,
  required bool relayOnline,
  required List<RemoteService> remote,
  required String homeLabel,
  int ownNamespace = 0,
}) {
  final homePlanets = _planets(StarSystem.homeId, [
    (
      BodyKind.share,
      [for (final t in shares) (key: t.key, label: t.key, tunnel: t)],
    ),
    (
      BodyKind.connect,
      [
        for (final t in connections)
          (key: t.key, label: remoteLabel(t.key, t.namespace), tunnel: t),
      ],
    ),
  ]);
  if (relayAddress == null) {
    final home = StarSystem(
      id: StarSystem.homeId,
      label: homeLabel,
      position: const Vec3(0, 0.12, 0),
      planets: homePlanets,
      home: true,
      online: true,
    );
    return GalaxyScene(systems: [home], links: const []);
  }
  final running = {
    for (final c in connections)
      if (c.health == TunnelHealth.running)
        relayServiceId(c.namespace ?? ownNamespace, c.key),
  };
  final relayPlanets = _planets(StarSystem.relayId, [
    (
      BodyKind.remote,
      [
        for (final s in remote)
          (
            key: s.id,
            label: remoteLabel(
              s.name,
              s.namespace == ownNamespace ? null : s.namespace,
            ),
            tunnel: null,
          ),
      ],
    ),
  ], inUse: running);
  final homeExtent = _extent(homePlanets);
  final relayExtent = _extent(relayPlanets);
  // The two stars sit either side of the disk's centre, far enough apart that
  // their orbits never cross, so the camera frames the pair rather than one
  // star with the other pushed to an edge. They start across the camera's
  // first view with a little depth — this computer nearer, the relay further
  // back — and the disk's slow turn moves them from there.
  final apart = math.max(3.0, homeExtent + relayExtent + 1.1);
  final dir = Vec3(math.cos(_pairAngle), 0, math.sin(_pairAngle));
  final home = StarSystem(
    id: StarSystem.homeId,
    label: homeLabel,
    position: dir * (-apart / 2) + const Vec3(0, 0.12, 0),
    planets: homePlanets,
    home: true,
    online: true,
  );
  final relay = StarSystem(
    id: StarSystem.relayId,
    label: relayAddress,
    position: dir * (apart / 2),
    planets: relayPlanets,
    home: false,
    online: relayOnline,
  );
  final links = [
    for (final planet in homePlanets)
      if (planet.tunnel?.health == TunnelHealth.running)
        Link(planetId: planet.id, systemId: StarSystem.relayId),
  ];
  return GalaxyScene(systems: [home, relay], links: links);
}

double _extent(List<Planet> planets) =>
    planets.isEmpty ? 0.4 : planets.map((p) => p.radius).reduce(math.max) + 0.2;

List<Planet> _planets(
  String systemId,
  List<(BodyKind, List<({String key, String label, Tunnel? tunnel})>)> groups, {
  Set<String> inUse = const {},
}) {
  final planets = <Planet>[];
  var ring = 0;
  for (final (kind, entries) in groups) {
    final sorted = [...entries]..sort((a, b) => a.key.compareTo(b.key));
    for (var start = 0; start < sorted.length; start += _perRing) {
      final group = sorted.sublist(
        start,
        math.min(start + _perRing, sorted.length),
      );
      final radius = 0.62 + ring * 0.34;
      // Kepler: the outer rings turn slower; the slowest takes about 90 s.
      final period = 90 * math.pow(radius / 1.3, 1.5).toDouble();
      final seed = stableHash('$systemId/$ring');
      for (var i = 0; i < group.length; i++) {
        final entry = group[i];
        planets.add(
          Planet(
            id: entry.tunnel?.id ?? 'remote:${entry.key}',
            key: entry.key,
            label: entry.label,
            kind: kind,
            tunnel: entry.tunnel,
            inUse: inUse.contains(entry.key),
            ring: ring,
            radius: radius,
            phase: unitOf(seed) * math.pi * 2 + i * math.pi * 2 / group.length,
            period: period,
            size: switch (kind) {
              BodyKind.share => 0.105,
              BodyKind.connect => 0.092,
              BodyKind.remote => 0.082,
            },
            tiltAxis: unitOf(seed >> 3) * math.pi * 2,
            tilt: (unitOf(seed >> 7) - 0.5) * 0.28,
          ),
        );
      }
      ring++;
    }
  }
  return planets;
}
