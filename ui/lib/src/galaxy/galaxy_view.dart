/// What the galaxy painter reads each frame: the scene, the camera, the clock,
/// what is selected or hovered, and the motion of bodies that just appeared,
/// changed or left.
///
/// Owned by the shell rather than the page, so leaving the overview and coming
/// back finds the galaxy where it was instead of flying in again.
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_camera.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';

/// A body drawn this frame, for hit testing.
class Hit {
  const Hit(this.id, this.center, this.radius, this.depth);

  final String id;
  final Offset center;
  final double radius;
  final double depth;
}

/// A body that left, fading where it was.
class Ghost {
  const Ghost(this.position, this.size, this.since);

  final Vec3 position;

  /// Body radius in world units.
  final double size;

  /// When it left (scene seconds).
  final double since;
}

class GalaxyView extends ChangeNotifier {
  GalaxyScene scene = GalaxyScene.empty;

  final GalaxyCamera camera = GalaxyCamera();

  /// Scene time in seconds.
  double time = 0;

  /// Whether motion is reduced: bodies rest where their orbits start.
  bool still = false;

  double get orbitTime => still ? 0 : time;

  String? selected;
  String? hovered;

  /// The body with keyboard focus.
  String? focused;

  /// When each body appeared (it spirals out of its star).
  final Map<String, double> appeared = {};

  /// When each body's state last changed (a ripple crosses its ring).
  final Map<String, double> rippled = {};

  /// Bodies that just left.
  final List<Ghost> ghosts = [];

  /// Bodies drawn in the last frame, front last.
  List<Hit> hits = const [];

  /// Moves on whenever the scene or a body's motion changes, so the encoded
  /// scene is redone only then.
  int revision = 0;

  /// Whether a scene has ever arrived; the first one flies in rather than
  /// having every body spiral out at once.
  bool get hasScene => scene.systems.isNotEmpty;

  /// A still scene has no entrances, ripples or fading ghosts.
  void stopMotion() {
    if (appeared.isEmpty && rippled.isEmpty && ghosts.isEmpty) return;
    appeared.clear();
    rippled.clear();
    ghosts.clear();
    revision++;
  }

  void tick() => notifyListeners();

  /// Take [next] as the scene, noting what appeared, changed or left.
  void update(GalaxyScene next, {required bool first}) {
    final animate = !first && !still;
    final before = <String, ({String state, Vec3 at, double size})>{};
    for (final system in scene.systems) {
      for (final planet in system.planets) {
        before[planet.id] = (
          state: planet.state,
          at: system.position + planet.offsetAt(orbitTime),
          size: planet.size,
        );
      }
    }
    final now = <String>{};
    for (final system in next.systems) {
      for (final planet in system.planets) {
        now.add(planet.id);
        final old = before[planet.id];
        if (old == null) {
          if (animate) appeared[planet.id] = time;
        } else if (old.state != planet.state && animate) {
          rippled[planet.id] = time;
        }
      }
    }
    for (final entry in before.entries) {
      if (!now.contains(entry.key) && animate) {
        ghosts.add(Ghost(entry.value.at, entry.value.size, time));
      }
    }
    // Entrances and ripples that finished long ago need not stay encoded.
    appeared.removeWhere((_, at) => time - at > 3);
    rippled.removeWhere((_, at) => time - at > 3);
    scene = next;
    if (selected != null &&
        next.systemOf(selected!) == null &&
        next.planetOf(selected!) == null) {
      selected = null;
    }
    revision++;
    notifyListeners();
  }

  /// The world position of body [id] as it is drawn now.
  Vec3? positionOf(String id) {
    for (final system in scene.systems) {
      if (system.id == id) return system.position;
      for (final planet in system.planets) {
        if (planet.id == id) {
          return system.position + planet.offsetAt(orbitTime);
        }
      }
    }
    return null;
  }

  /// The front-most body drawn under [point]; else the nearest one within
  /// reach, so a small body still takes a fingertip.
  Hit? hitAt(Offset point) {
    Hit? drawn;
    Hit? near;
    var nearest = double.infinity;
    for (final hit in hits) {
      final distance = (hit.center - point).distance;
      if (distance <= hit.radius &&
          (drawn == null || hit.depth < drawn.depth)) {
        drawn = hit;
      }
      final reach = math.max(22.0, hit.radius + 12);
      if (distance <= reach && distance < nearest) {
        near = hit;
        nearest = distance;
      }
    }
    return drawn ?? near;
  }
}
