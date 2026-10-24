use super::{VERSION, compute};

/// This computer with two tunnels on one ring, the relay with one registered
/// service, a second relay that is offline with nothing registered, a live
/// tunnel and one star layer.
fn scene() -> Vec<f64> {
    let mut s = vec![VERSION, 3.0, 3.0, 1.0, 1.0];
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
    // A live tunnel from the relay's service to this computer.
    s.extend([2.0, 0.0]);
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
    for _ in 0..systems {
        let index = c.next();
        let (_x, _y, radius, depth, _opacity) = (c.next(), c.next(), c.next(), c.next(), c.next());
        assert!(radius >= 3.0);
        depths.push(depth);
        let (back, front) = (c.runs(), c.runs());
        let planets = c.next() as usize;
        for _ in 0..planets {
            for _ in 0..8 {
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
    // One link with its three particles.
    assert_eq!(c.next(), 1.0);
    assert!(c.points().len() >= 2);
    assert_eq!(c.points().len(), 3);
}

#[test]
fn a_malformed_scene_or_frame_is_refused() {
    let good = scene();
    let mut wrong_version = good.clone();
    wrong_version[0] = 2.0;
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
