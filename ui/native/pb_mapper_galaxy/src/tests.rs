use super::{
    Camera, DecodedScene, Frame, Link, Out, Planet, SCRATCH_LIMIT_BYTES, Scene, System, V3,
    VERSION, compute, links, runs,
};

/// This computer with two tunnels on one ring, the relay with one registered
/// service, a second relay that is offline with nothing registered, a live
/// tunnel through the relay and a direct one, and one star layer.
fn scene() -> Vec<f64> {
    let mut s = vec![VERSION, 3.0, 3.0, 2.0, 1.0];
    // Systems: x, y, z, home, online, first planet, planet count, label.
    s.extend([0.0, 0.12, 0.0, 1.0, 1.0, 0.0, 2.0, 60.0, 15.0]);
    s.extend([3.4, 0.0, -1.2, 0.0, 1.0, 2.0, 1.0, 48.0, 15.0]);
    s.extend([-2.8, 0.0, 2.5, 0.0, 0.0, 3.0, 0.0, 30.0, 15.0]);
    // Planets: system, ring, radius, phase, period, size, tilt axis, tilt,
    // appeared, rippled, label, strong label.
    let planet = |system: f64, ring: f64, phase: f64| {
        [
            system,
            ring,
            0.62,
            phase,
            90.0,
            0.105,
            0.4,
            0.1,
            f64::NAN,
            f64::NAN,
            40.0,
            14.0,
            44.0,
            15.0,
        ]
    };
    s.extend(planet(0.0, 0.0, 0.3));
    s.extend(planet(0.0, 0.0, 3.4));
    s.extend(planet(1.0, 0.0, 1.0));
    // A live tunnel from the relay's service to this computer's star, and a
    // direct one from this computer's first planet to that service (body 5:
    // three systems, then planet 2).
    s.extend([2.0, 0.0, 0.0]);
    s.extend([0.0, 5.0, 1.0]);
    // One star layer with one bucket of two stars.
    s.extend([0.3, 1.0, 0.5, 2.0, 0.1, 0.2, 0.9, 0.8]);
    s
}

/// Camera at rest over the disk, 1440 × 900, nothing selected.
fn frame(time: f64) -> Vec<f64> {
    vec![
        1440.0, 900.0, 0.6, 0.52, 9.0, 0.0, 0.0, 0.0, time, 0.0, -1.0, -1.0, -1.0,
    ]
}

fn run(scene: &[f64], frame: &[f64], capacity: usize) -> (i64, Vec<f32>) {
    let mut out = vec![0f32; capacity];
    let written = compute(scene, frame, &mut out);
    (written, out)
}

struct Cursor<'a>(&'a [f32], usize);

impl Cursor<'_> {
    fn next(&mut self) -> f32 {
        self.1 += 1;
        self.0[self.1 - 1]
    }
    fn points(&mut self) -> Vec<(f32, f32)> {
        let n = self.next() as usize;
        (0..n).map(|_| (self.next(), self.next())).collect()
    }
    fn runs(&mut self) -> Vec<Vec<(f32, f32)>> {
        let n = self.next() as usize;
        (0..n).map(|_| self.points()).collect()
    }
}

#[test]
fn a_short_buffer_learns_the_length_it_needs() {
    let (scene, frame) = (scene(), frame(12.5));
    let (needed, _) = run(&scene, &frame, 0);
    assert!(needed < -1, "{needed}");
    let (written, out) = run(&scene, &frame, (-needed) as usize);
    assert_eq!(written, -needed);
    // A larger buffer holds exactly the same frame.
    let (again, roomy) = run(&scene, &frame, 4 * (-needed) as usize);
    assert_eq!(again, written);
    assert_eq!(out[..], roomy[..written as usize]);
}

#[test]
fn systems_come_far_to_near_with_orbits_split_at_their_star() {
    let (scene, frame) = (scene(), frame(30.0));
    let (written, out) = run(&scene, &frame, 1 << 16);
    assert!(written > 0);
    let mut c = Cursor(&out, 0);
    // One star layer with one bucket: its twinkle and two stars.
    c.next();
    assert_eq!(c.points().len(), 2);
    assert_eq!(c.next(), 1.0, "the plane's glow is drawn");
    for _ in 0..5 {
        c.next();
    }
    let guides = c.next() as usize;
    for _ in 0..guides {
        c.next();
        c.runs();
    }
    let systems = c.next() as usize;
    assert_eq!(systems, 3);
    let mut depths = Vec::new();
    let mut service = None;
    for _ in 0..systems {
        let index = c.next();
        let (_x, _y, radius, depth, _opacity) = (c.next(), c.next(), c.next(), c.next(), c.next());
        assert!(radius >= 3.0);
        depths.push(depth);
        let (back, front) = (c.runs(), c.runs());
        let planets = c.next() as usize;
        for _ in 0..planets {
            let (planet, x, y) = (c.next(), c.next(), c.next());
            if planet == 2.0 {
                service = Some((x, y));
            }
            for _ in 0..5 {
                c.next();
            }
        }
        if index == 0.0 {
            // Seen from above, this computer's ring passes behind and in front
            // of its star, each half starting where the other ended.
            assert!(!back.is_empty() && !front.is_empty());
            let ends: Vec<_> = back
                .iter()
                .chain(&front)
                .map(|r| *r.last().unwrap())
                .collect();
            let starts: Vec<_> = back.iter().chain(&front).map(|r| r[0]).collect();
            assert!(starts.iter().filter(|s| ends.contains(s)).count() >= 1);
            assert_eq!(planets, 2);
        }
    }
    assert!(depths.windows(2).all(|d| d[0] >= d[1]), "{depths:?}");
    // Two links, each named by its place in the scene, with three particles.
    assert_eq!(c.next(), 2.0);
    assert_eq!(c.next(), 0.0);
    assert!(c.points().len() >= 2);
    assert_eq!(c.points().len(), 3);
    // The direct one ends on the service it reaches, not on the relay.
    assert_eq!(c.next(), 1.0);
    let curve = c.points();
    assert_eq!(c.points().len(), 3);
    let (end, service) = (curve.last().unwrap(), service.unwrap());
    assert!(
        (end.0 - service.0).abs() < 0.5 && (end.1 - service.1).abs() < 0.5,
        "{end:?} vs {service:?}"
    );
}

#[test]
fn a_malformed_scene_or_frame_is_refused() {
    let good = scene();
    // The layout before links could end on a planet.
    let mut wrong_version = good.clone();
    wrong_version[0] = 1.0;
    let mut stray_planet = good.clone();
    // The third planet names a fourth system.
    stray_planet[5 + 3 * 9 + 2 * 14] = 3.0;
    for bad in [
        &wrong_version,
        &stray_planet,
        &good[..good.len() - 1].to_vec(),
    ] {
        assert_eq!(run(bad, &frame(1.0), 1 << 16).0, -1);
    }
    assert_eq!(run(&good, &frame(1.0)[..12], 1 << 16).0, -1);
}

#[test]
fn decoding_reuses_buffers_and_recovers_after_a_malformed_scene() {
    let raw = scene();
    let mut storage = DecodedScene::default();
    assert!(Scene::decode(&raw, &mut storage).is_some());
    let systems = storage.systems.as_ptr();
    let planets = storage.planets.as_ptr();
    let buckets = storage.layers[0].buckets.as_ptr();
    assert!(Scene::decode(&raw, &mut storage).is_some());
    assert_eq!(storage.systems.as_ptr(), systems);
    assert_eq!(storage.planets.as_ptr(), planets);
    assert_eq!(storage.layers[0].buckets.as_ptr(), buckets);
    assert!(Scene::decode(&raw[..raw.len() - 1], &mut storage).is_none());
    let decoded = Scene::decode(&raw, &mut storage).unwrap();
    assert_eq!(
        (
            decoded.systems.len(),
            decoded.planets.len(),
            decoded.layers.len()
        ),
        (3, 3, 1)
    );

    // An exceptional scene must not retain more than the normal scratch cap.
    storage
        .systems
        .reserve(SCRATCH_LIMIT_BYTES / std::mem::size_of::<System>() + 1);
    storage.trim();
    assert_eq!(storage.systems.capacity(), 0);
}

#[test]
fn lines_crossing_the_near_plane_are_clipped_into_separate_runs() {
    let frame = Frame::decode(&frame(1.0)).unwrap();
    let camera = Camera::new(&frame);
    let depths = [0.1, 1.0, 0.1, 1.0];
    let mut buffer = [0.0; 64];
    let mut out = Out::new(&mut buffer);
    runs(
        &camera,
        3,
        |i| {
            camera
                .eye
                .add(camera.forward.mul(depths[i]))
                .add(camera.right.mul(i as f64 * 0.2))
        },
        &mut out,
    );
    let mut cursor = Cursor(&buffer, 0);
    let segments = cursor.runs();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].len(), 3);
    assert_eq!(segments[1].len(), 2);
    assert!(
        segments
            .iter()
            .flatten()
            .all(|(x, y)| x.is_finite() && y.is_finite())
    );
}

#[test]
fn tunnel_curve_does_not_join_visible_sections_across_the_camera() {
    let systems = [
        System {
            position: V3::new(-5.0, 0.0, 0.0),
            home: true,
            online: true,
            first: 0,
            count: 1,
            label: (0.0, 0.0),
        },
        System {
            position: V3::new(5.0, 0.0, 0.0),
            home: false,
            online: true,
            first: 1,
            count: 0,
            label: (0.0, 0.0),
        },
    ];
    let planets = [Planet {
        system: 0,
        ring: 0,
        radius: 0.0,
        phase: 0.0,
        period: 90.0,
        size: 0.1,
        tilt_axis: 0.0,
        tilt: 0.0,
        appeared: f64::NAN,
        rippled: f64::NAN,
        label: (0.0, 0.0),
        strong: (0.0, 0.0),
    }];
    let connections = [Link {
        from: 0,
        to: 1,
        direct: true,
    }];
    let scene = Scene {
        systems: &systems,
        planets: &planets,
        links: &connections,
        layers: &[],
        stars: &[],
    };
    let frame = Frame::decode(&[
        800.0, 600.0, 0.0, 1.3, 2.2, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, -1.0, -1.0,
    ])
    .unwrap();
    let camera = Camera::new(&frame);
    let mut buffer = [0.0; 1024];
    let mut out = Out::new(&mut buffer);
    links(&scene, &frame, &camera, &mut out);
    let mut cursor = Cursor(&buffer, 0);
    assert_eq!(cursor.next(), 2.0);
    assert_eq!(cursor.next(), 0.0);
    assert!(cursor.points().len() >= 2);
    assert!(cursor.points().is_empty());
    assert_eq!(cursor.next(), 0.0);
    assert!(cursor.points().len() >= 2);
    assert!(cursor.points().len() <= 3);
}
