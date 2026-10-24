//! Times one galaxy frame, as the UI computes it every frame, on a typical
//! and a busy scene: the clock advances by a 60 Hz frame each time and the
//! camera's yaw turns slowly.
//!
//! ```text
//! cargo run -p pb-mapper-galaxy --example galaxy_bench --release
//! ```
//!
//! The scenes are encoded by hand following the scene layout in the crate
//! docs, as the UI encodes them.

use std::f64::consts::PI;
use std::hint::black_box;
use std::time::Instant;

use pb_mapper_galaxy::{FRAME_FIELDS, VERSION, compute};

const FRAMES: usize = 2000;

/// The background star layers: stars, parallax. Each splits into four
/// twinkle buckets.
const STAR_LAYERS: [(usize, f64); 3] = [(320, 0.12), (150, 0.3), (56, 0.62)];
const BUCKETS: usize = 4;

/// A deterministic xorshift, so every run times the same scenes.
struct Rng(u64);

impl Rng {
    /// Uniform in 0..1.
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One system to encode: where it is, whether it is this computer, and how
/// many planets it has on each of its rings.
struct SystemSpec {
    position: [f64; 3],
    home: bool,
    rings: &'static [usize],
    label: (f64, f64),
}

struct Bench {
    name: &'static str,
    systems: &'static [SystemSpec],
    /// Live tunnels: planet, system.
    links: &'static [(usize, usize)],
    distance: f64,
}

/// This computer with eight tunnels on two rings, the relay with six
/// registered services, four live tunnels.
const TYPICAL: Bench = Bench {
    name: "typical",
    systems: &[
        SystemSpec {
            position: [0.0, 0.12, 0.0],
            home: true,
            rings: &[4, 4],
            label: (92.0, 15.0),
        },
        SystemSpec {
            position: [3.4, 0.0, -1.2],
            home: false,
            rings: &[3, 3],
            label: (110.0, 15.0),
        },
    ],
    links: &[(0, 1), (5, 1), (8, 0), (11, 0)],
    distance: 9.0,
};

/// Sixty planets between the two systems, twenty live tunnels.
const BUSY: Bench = Bench {
    name: "busy",
    systems: &[
        SystemSpec {
            position: [0.0, 0.12, 0.0],
            home: true,
            rings: &[12, 12, 10],
            label: (92.0, 15.0),
        },
        SystemSpec {
            position: [4.2, 0.0, -1.6],
            home: false,
            rings: &[10, 10, 6],
            label: (110.0, 15.0),
        },
    ],
    links: &[
        (0, 1),
        (3, 1),
        (6, 1),
        (9, 1),
        (12, 1),
        (15, 1),
        (18, 1),
        (21, 1),
        (24, 1),
        (27, 1),
        (34, 0),
        (36, 0),
        (39, 0),
        (42, 0),
        (45, 0),
        (48, 0),
        (51, 0),
        (54, 0),
        (57, 0),
        (59, 0),
    ],
    distance: 11.0,
};

/// Encodes `bench`'s scene in the layout the crate docs describe.
fn encode(bench: &Bench, rng: &mut Rng) -> Vec<f64> {
    let n_planets: usize = bench
        .systems
        .iter()
        .map(|s| s.rings.iter().sum::<usize>())
        .sum();
    let mut scene = vec![
        VERSION,
        bench.systems.len() as f64,
        n_planets as f64,
        bench.links.len() as f64,
        STAR_LAYERS.len() as f64,
    ];
    // Per system: x, y, z, home, online, first planet, planet count, label.
    let mut first = 0;
    for system in bench.systems {
        let count: usize = system.rings.iter().sum();
        let [x, y, z] = system.position;
        scene.extend([
            x,
            y,
            z,
            f64::from(u8::from(system.home)),
            1.0,
            first as f64,
            count as f64,
            system.label.0,
            system.label.1,
        ]);
        first += count;
    }
    // Per planet: system, ring, radius, phase, period, size, tilt axis, tilt,
    // appeared, rippled, label, strong label.
    let mut planet = 0;
    for (index, system) in bench.systems.iter().enumerate() {
        for (ring, &on_ring) in system.rings.iter().enumerate() {
            let radius = 0.62 + 0.34 * ring as f64;
            let tilt_axis = rng.unit() * PI * 2.0;
            for k in 0..on_ring {
                let phase = (k as f64 + 0.3 * rng.unit()) / on_ring as f64 * PI * 2.0;
                // One planet arrives (spiralling out) and one changes status
                // early on, so those paths are timed too.
                let appeared = if planet == 2 { 9.8 } else { f64::NAN };
                let rippled = if planet == 4 { 10.2 } else { f64::NAN };
                let width = 40.0 + 40.0 * rng.unit();
                scene.extend([
                    index as f64,
                    ring as f64,
                    radius,
                    phase,
                    90.0 + 40.0 * ring as f64,
                    if system.home { 0.105 } else { 0.09 },
                    tilt_axis,
                    0.1,
                    appeared,
                    rippled,
                    width,
                    14.0,
                    width * 1.08,
                    15.0,
                ]);
                planet += 1;
            }
        }
    }
    // Per link: planet, system.
    for &(planet, system) in bench.links {
        scene.extend([planet as f64, system as f64]);
    }
    // Per star layer: parallax, buckets; per bucket phase, count, unit x, y.
    for (stars, parallax) in STAR_LAYERS {
        scene.extend([parallax, BUCKETS as f64]);
        for bucket in 0..BUCKETS {
            let count = stars / BUCKETS + usize::from(bucket < stars % BUCKETS);
            scene.extend([rng.unit() * PI * 2.0, count as f64]);
            for _ in 0..count {
                scene.extend([rng.unit(), rng.unit()]);
            }
        }
    }
    scene
}

/// Frame `i`: 1440 × 900, the clock a 60 Hz frame further on, the yaw a
/// little further round, nothing selected.
fn frame(bench: &Bench, i: usize) -> [f64; FRAME_FIELDS] {
    let time = 10.0 + i as f64 / 60.0;
    let yaw = 0.6 + i as f64 * 0.0015;
    [
        1440.0,
        900.0,
        yaw,
        0.52,
        bench.distance,
        0.0,
        0.0,
        0.0,
        time,
        0.0,
        -1.0,
        -1.0,
        -1.0,
    ]
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn run(bench: &Bench) {
    let scene = encode(bench, &mut Rng(0x9e37_79b9_7f4a_7c15));
    // A first pass learns the longest output, sizing the buffer once as the
    // UI does, and warms the caches and the scratch.
    let mut out = Vec::new();
    let (mut shortest, mut longest) = (usize::MAX, 0);
    for i in 0..FRAMES {
        let written = compute(&scene, &frame(bench, i), &mut []);
        if written == -1 {
            eprintln!("{}: the scene or frame was refused", bench.name);
            std::process::exit(1);
        }
        let needed = written.unsigned_abs() as usize;
        shortest = shortest.min(needed);
        longest = longest.max(needed);
    }
    out.resize(longest, 0f32);
    let mut micros = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        let values = frame(bench, i);
        let start = Instant::now();
        let written = compute(black_box(&scene), black_box(&values), black_box(&mut out));
        let elapsed = start.elapsed();
        if written <= 0 {
            eprintln!("{}: frame {i} returned {written}", bench.name);
            std::process::exit(1);
        }
        black_box(&out);
        micros.push(elapsed.as_secs_f64() * 1e6);
    }
    micros.sort_by(f64::total_cmp);
    let mean = micros.iter().sum::<f64>() / micros.len() as f64;
    println!(
        "{:<8} scene {:>5} f64  out {:>5}..{:<5} f32  p50 {:>6.1} us  p99 {:>6.1} us  \
         mean {:>6.1} us  max {:>6.1} us  ({FRAMES} frames)",
        bench.name,
        scene.len(),
        shortest,
        longest,
        percentile(&micros, 0.5),
        percentile(&micros, 0.99),
        mean,
        micros[micros.len() - 1],
    );
}

fn main() {
    run(&TYPICAL);
    run(&BUSY);
}
