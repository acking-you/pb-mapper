//! The galaxy view's per-frame geometry: the camera's projection, every orbit,
//! planet and star of the scene, the plane's guides, the links, the background
//! stars, depth order and label placement, computed here so the Dart painter
//! only issues draw calls.
//!
//! The galaxy draws pb-mapper's topology. Each system is a star: this computer,
//! or the relay server it works through. Planets orbit them: around this
//! computer its tunnels (the services it shares and the connections it holds),
//! around the relay the services registered there. A link is a live tunnel,
//! drawn as a curve from a planet to another system's star — or, for one that
//! goes direct and so never crosses the relay, to the planet of the service it
//! reaches.
//!
//! The UI calls `pb_mapper_galaxy_frame_v1` (exported by `pb-mapper-ffi`, a
//! thin wrapper over [`compute`]) once per frame through a leaf `dart:ffi` call
//! with buffers it owns: the scene (encoded only when it changes), the frame
//! (camera, clock, selection) and the output, laid out as below. The UI keeps a
//! step-for-step Dart mirror of this arithmetic, so either draws the same
//! picture, and a parity test compares the two on the built library: change
//! them together.
//!
//! The geometry follows POCOX's galaxy engine; the layouts and the arithmetic
//! are kept identical to it, so the Dart mirror ported from there stays exact.
//!
//! # Scene
//!
//! All `f64`, in order:
//!
//! - the header: [`VERSION`], then the number of systems, planets, links and
//!   star layers;
//! - per system (9 values): x, y, z, home (1 for this computer), online (0/1),
//!   first planet, planet count (its planets are that contiguous range of the
//!   planet list), label width, label height (its label is always strong);
//! - per planet (14 values): its system, ring (one orbit is drawn per ring, from
//!   the ring's first planet), orbit radius, phase (radians at time 0), period
//!   (seconds per orbit), size, tilt axis and tilt (the orbit's plane is turned
//!   by tilt around the in-plane axis at that angle), appeared (when it
//!   appeared, so it spirals out of its star, or NaN), rippled (when its status
//!   last changed, so a ripple crosses it, or NaN), label width and height, then
//!   the strong label's width and height;
//! - per link (3 values): the planet it starts from, the body it ends at
//!   (numbered as in the frame: a system's star, or a planet), and direct
//!   (0/1), whose particles travel faster;
//! - per star layer: its parallax and number of buckets, then per bucket its
//!   twinkle phase, its number of stars and their unit coordinates (x, y each,
//!   in 0..1 of the view).
//!
//! # Frame
//!
//! [`FRAME_FIELDS`] `f64`: width, height, yaw, tilt, distance, target x, y, z,
//! time (seconds), still (1 freezes every motion), then the selected, hovered
//! and focused bodies. Bodies are numbered systems first, then planets; -1 is
//! none, and -2 one this scene does not hold.
//!
//! # Output
//!
//! All `f32`, in drawing order:
//!
//! - background stars: per layer and bucket its twinkle and points (count, then
//!   x, y each);
//! - the plane's glow: drawn (0/1), left, top, right, bottom, blur sigma;
//! - guides: count, then per guide its fade and runs (count, then per run its
//!   points: count, then x, y each);
//! - systems far to near: count, then per system its index, centre x, y, star
//!   radius, depth, opacity, back runs, front runs (each: count, then runs),
//!   and its planets: count, then per planet its index in the planet list, x,
//!   y, radius, depth, opacity, ripple progress (-1 none) and behind its star
//!   (0/1);
//! - links: count, then per link its index in the scene's links, its curve's
//!   points and its particles' points;
//! - labels: count, then per label the body (numbered as in the frame), strong
//!   (0/1), x, y.
mod scratch;
use scratch::Scratch;
#[cfg(test)]
mod tests;

use std::f64::consts::PI;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The buffer layouts' version; both ends name it.
pub const VERSION: f64 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct V3 {
    x: f64,
    y: f64,
    z: f64,
}

impl V3 {
    const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    fn mul(self, k: f64) -> Self {
        Self::new(self.x * k, self.y * k, self.z * k)
    }
    fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    fn length(self) -> f64 {
        self.dot(self).sqrt()
    }
    fn unit(self) -> Self {
        let l = self.length();
        if l == 0.0 { self } else { self.mul(1.0 / l) }
    }
    fn lerp(self, o: Self, t: f64) -> Self {
        self.add(o.sub(self).mul(t))
    }
}

/// [p] rotated by [tilt] around the in-plane axis at angle [axis]
/// (Rodrigues' rotation around k = (cos axis, 0, sin axis)).
fn tilt_around(p: V3, axis: f64, tilt: f64) -> V3 {
    let k = V3::new(axis.cos(), 0.0, axis.sin());
    let (cos, sin) = (tilt.cos(), tilt.sin());
    p.mul(cos)
        .add(k.cross(p).mul(sin))
        .add(k.mul(k.dot(p) * (1.0 - cos)))
}

/// [p] turned by [angle] around the disk's axis.
fn turned(p: V3, angle: f64) -> V3 {
    let (c, s) = (angle.cos(), angle.sin());
    V3::new(p.x * c - p.z * s, p.y, p.x * s + p.z * c)
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// One projected point.
#[derive(Clone, Copy, Debug)]
struct Projected {
    x: f64,
    y: f64,
    scale: f64,
    depth: f64,
}

/// The view of one frame: world points to screen points.
struct Camera {
    eye: V3,
    forward: V3,
    right: V3,
    up: V3,
    cx: f64,
    cy: f64,
    focal: f64,
}

impl Camera {
    fn new(frame: &Frame) -> Self {
        let dir = V3::new(
            frame.tilt.cos() * frame.yaw.sin(),
            frame.tilt.sin(),
            frame.tilt.cos() * frame.yaw.cos(),
        );
        let eye = frame.target.add(dir.mul(frame.distance));
        let forward = frame.target.sub(eye).unit();
        let right = forward.cross(V3::new(0.0, 1.0, 0.0)).unit();
        let up = right.cross(forward);
        Self {
            eye,
            forward,
            right,
            up,
            cx: frame.width / 2.0,
            cy: frame.height * 0.52,
            focal: frame.width.min(frame.height) * 0.92,
        }
    }

    /// [p] on screen, or nothing behind the camera.
    fn at(&self, p: V3) -> Option<Projected> {
        let v = p.sub(self.eye);
        let depth = v.dot(self.forward);
        if depth < 0.2 {
            return None;
        }
        let s = self.focal / depth;
        Some(Projected {
            x: self.cx + v.dot(self.right) * s,
            y: self.cy - v.dot(self.up) * s,
            scale: s,
            depth,
        })
    }
}

/// One system's star: this computer, or the relay server.
#[derive(Clone, Copy, Debug)]
struct System {
    position: V3,
    /// This computer, drawn as the larger star.
    home: bool,
    online: bool,
    first: usize,
    count: usize,
    /// Its label's size (always the strong style).
    label: (f64, f64),
}

/// One planet orbiting its system: a tunnel on this computer, or a service
/// registered on the relay.
#[derive(Clone, Copy, Debug)]
struct Planet {
    system: usize,
    ring: i64,
    radius: f64,
    phase: f64,
    period: f64,
    size: f64,
    tilt_axis: f64,
    tilt: f64,
    /// When it appeared (it spirals out of its star), or NaN.
    appeared: f64,
    /// When its status last changed (a ripple crosses it), or NaN.
    rippled: f64,
    /// Its label's size in the base and the strong style.
    label: (f64, f64),
    strong: (f64, f64),
}

impl Planet {
    fn offset_at(&self, t: f64) -> V3 {
        let angle = self.phase + (t / self.period) * PI * 2.0;
        let flat = V3::new(angle.cos() * self.radius, 0.0, angle.sin() * self.radius);
        tilt_around(flat, self.tilt_axis, self.tilt)
    }
}

/// One layer of background stars.
#[derive(Clone, Debug)]
struct StarLayer {
    parallax: f64,
    /// Per bucket: its twinkle phase and interleaved unit x, y.
    buckets: Vec<(f64, std::ops::Range<usize>)>,
}

/// The decoded scene (its layout is in the crate docs). The star coordinates
/// stay in the caller's buffer.
struct Scene<'a> {
    systems: Vec<System>,
    planets: Vec<Planet>,
    links: Vec<Link>,
    layers: Vec<StarLayer>,
    stars: &'a [f64],
}

/// A live tunnel: from a planet to a body.
#[derive(Clone, Copy, Debug)]
struct Link {
    /// The planet it starts from.
    from: usize,
    /// Systems first, then planets: a system's star, or a planet.
    to: usize,
    direct: bool,
}

const SYSTEM_FIELDS: usize = 9;
const PLANET_FIELDS: usize = 14;

fn count(value: f64, limit: usize) -> Option<usize> {
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= limit as f64)
        .then_some(value as usize)
}

/// Reads a buffer's fields in order.
struct Fields<'a> {
    raw: &'a [f64],
    at: usize,
}

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [f64]> {
        let slice = self.raw.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(slice)
    }
}

impl<'a> Scene<'a> {
    /// Decodes `raw`, or nothing when it is malformed.
    fn decode(raw: &'a [f64]) -> Option<Self> {
        if raw.len() < 5 || raw[0] != VERSION {
            return None;
        }
        let limit = raw.len();
        let (n_systems, n_planets) = (count(raw[1], limit)?, count(raw[2], limit)?);
        let (n_links, n_layers) = (count(raw[3], limit)?, count(raw[4], limit)?);
        let mut fields = Fields { raw, at: 5 };
        let mut systems = Vec::with_capacity(n_systems);
        for _ in 0..n_systems {
            let f = fields.take(SYSTEM_FIELDS)?;
            systems.push(System {
                position: V3::new(f[0], f[1], f[2]),
                home: f[3] != 0.0,
                online: f[4] != 0.0,
                first: count(f[5], n_planets)?,
                count: count(f[6], n_planets)?,
                label: (f[7], f[8]),
            });
        }
        let mut planets = Vec::with_capacity(n_planets);
        for _ in 0..n_planets {
            let f = fields.take(PLANET_FIELDS)?;
            planets.push(Planet {
                system: count(f[0], n_systems.checked_sub(1)?)?,
                ring: f[1] as i64,
                radius: f[2],
                phase: f[3],
                period: f[4],
                size: f[5],
                tilt_axis: f[6],
                tilt: f[7],
                appeared: f[8],
                rippled: f[9],
                label: (f[10], f[11]),
                strong: (f[12], f[13]),
            });
        }
        if systems.iter().any(|s| s.first + s.count > n_planets) {
            return None;
        }
        let mut links = Vec::with_capacity(n_links);
        for _ in 0..n_links {
            let f = fields.take(3)?;
            links.push(Link {
                from: count(f[0], n_planets.checked_sub(1)?)?,
                to: count(f[1], (n_systems + n_planets).checked_sub(1)?)?,
                direct: f[2] != 0.0,
            });
        }
        let mut layers = Vec::with_capacity(n_layers);
        for _ in 0..n_layers {
            let head = fields.take(2)?;
            let (parallax, n_buckets) = (head[0], count(head[1], limit)?);
            let mut buckets = Vec::with_capacity(n_buckets);
            for _ in 0..n_buckets {
                let bucket = fields.take(2)?;
                let points = count(bucket[1], limit)?;
                let start = fields.at;
                fields.take(points.checked_mul(2)?)?;
                buckets.push((bucket[0], start..fields.at));
            }
            layers.push(StarLayer { parallax, buckets });
        }
        Some(Self {
            systems,
            planets,
            links,
            layers,
            stars: raw,
        })
    }
}

/// The decoded frame (its layout is in the crate docs).
#[derive(Clone, Copy, Debug)]
struct Frame {
    width: f64,
    height: f64,
    yaw: f64,
    tilt: f64,
    distance: f64,
    target: V3,
    time: f64,
    /// Every motion frozen (reduced motion).
    still: bool,
    selected: i64,
    hovered: i64,
    focused: i64,
}

/// The frame layout's length.
pub const FRAME_FIELDS: usize = 13;

impl Frame {
    /// Decodes `raw`, or nothing when it is malformed.
    fn decode(raw: &[f64]) -> Option<Self> {
        let f: &[f64; FRAME_FIELDS] = raw.try_into().ok()?;
        let frame = Self {
            width: f[0],
            height: f[1],
            yaw: f[2],
            tilt: f[3],
            distance: f[4],
            target: V3::new(f[5], f[6], f[7]),
            time: f[8],
            still: f[9] != 0.0,
            selected: f[10] as i64,
            hovered: f[11] as i64,
            focused: f[12] as i64,
        };
        let usable = frame.width > 0.0
            && frame.height > 0.0
            && f[..10].iter().all(|v| v.is_finite())
            && frame.distance > 0.0;
        usable.then_some(frame)
    }

    fn orbit_time(&self) -> f64 {
        if self.still { 0.0 } else { self.time }
    }
}

/// Writes into the caller's buffer and keeps counting past its end, so a
/// short buffer learns the length it needs.
struct Out<'a> {
    buf: &'a mut [f32],
    len: usize,
}

impl<'a> Out<'a> {
    /// A writer into `buf`.
    fn new(buf: &'a mut [f32]) -> Self {
        Self { buf, len: 0 }
    }

    /// Everything written (or that would have been).
    fn len(&self) -> usize {
        self.len
    }

    /// Whether it all fit.
    fn fits(&self) -> bool {
        self.len <= self.buf.len()
    }

    fn push(&mut self, value: f64) {
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = value as f32;
        }
        self.len += 1;
    }

    /// A slot to fill in later (a count known only afterwards).
    fn hole(&mut self) -> usize {
        self.push(0.0);
        self.len - 1
    }

    fn fill(&mut self, at: usize, value: f64) {
        if let Some(slot) = self.buf.get_mut(at) {
            *slot = value as f32;
        }
    }

    fn points(&mut self, points: &[(f64, f64)]) {
        self.push(points.len() as f64);
        for &(x, y) in points {
            self.push(x);
            self.push(y);
        }
    }
}

/// Computes one galaxy frame of `scene` for `frame` into `out` (the layouts
/// are in the crate docs).
///
/// Returns the number of values written to `out`; when `out` is too short,
/// minus the number needed (and `out` holds nothing usable); -1 for a
/// malformed scene or frame, or if the computation panics. A frame is never
/// shorter than ten values, so minus the needed length is never -1.
///
/// It allocates nothing the caller frees, takes no lock and makes no callback;
/// its temporaries live in a thread-local scratch that is reused across frames.
pub fn compute(scene: &[f64], frame: &[f64], out: &mut [f32]) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(scene), Some(frame)) = (Scene::decode(scene), Frame::decode(frame)) else {
            return -1;
        };
        let mut writer = Out::new(out);
        self::frame(&scene, &frame, &mut writer);
        if writer.fits() {
            writer.len() as i64
        } else {
            -(writer.len() as i64)
        }
    }))
    .unwrap_or(-1)
}

/// A body drawn this frame, in drawing order, for the labels.
struct Hit {
    body: usize,
    x: f64,
    y: f64,
    radius: f64,
}

/// Computes one frame of `scene` into `out`, in drawing order (the output
/// layout is in the crate docs).
fn frame(scene: &Scene, frame: &Frame, out: &mut Out) {
    thread_local! {
        static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::new(Scratch::default());
    }
    SCRATCH.with_borrow_mut(|scratch| {
        let camera = Camera::new(frame);
        stars(scene, frame, out);
        plane(scene, &camera, out, &scratch.circle);
        systems(scene, frame, &camera, out, scratch);
        links(scene, frame, &camera, out);
        labels(scene, frame, out, scratch);
        scratch.trim();
    });
}

fn extent(scene: &Scene, system: &System) -> f64 {
    let planets = &scene.planets[system.first..system.first + system.count];
    if planets.is_empty() {
        0.4
    } else {
        planets
            .iter()
            .map(|p| p.radius)
            .fold(f64::NEG_INFINITY, f64::max)
            + 0.2
    }
}

/// Stream projected runs into the caller's output, backfilling their counts.
fn runs(camera: &Camera, steps: usize, point: impl Fn(usize) -> V3, out: &mut Out) {
    let total = out.hole();
    let (mut runs, mut count, mut points) = (0, None, 0);
    for i in 0..=steps {
        match camera.at(point(i)) {
            None => {
                if let Some(at) = count.take() {
                    out.fill(at, points as f64);
                }
            }
            Some(p) => {
                if count.is_none() {
                    count = Some(out.hole());
                    points = 0;
                    runs += 1;
                }
                out.push(p.x);
                out.push(p.y);
                points += 1;
            }
        }
    }
    if let Some(at) = count {
        out.fill(at, points as f64);
    }
    out.fill(total, runs as f64);
}

fn plane(scene: &Scene, camera: &Camera, out: &mut Out, circle: &[(f64, f64); 97]) {
    if scene.systems.is_empty() {
        for _ in 0..6 {
            out.push(0.0);
        }
        out.push(0.0);
        return;
    }
    let mut reach: f64 = 3.0;
    for system in &scene.systems {
        reach = reach.max(system.position.length() + extent(scene, system));
    }
    // A soft glow over the plane.
    let (mut l, mut t, mut r, mut b) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    let mut visible = 0;
    for i in 0..48 {
        let a = i as f64 / 48.0 * PI * 2.0;
        if let Some(p) = camera.at(V3::new(a.cos() * reach, 0.0, a.sin() * reach)) {
            l = l.min(p.x);
            t = t.min(p.y);
            r = r.max(p.x);
            b = b.max(p.y);
            visible += 1;
        }
    }
    if visible > 8 {
        let shortest = (r - l).min(b - t);
        let inset = shortest * 0.12;
        for value in [
            1.0,
            l + inset,
            t + inset,
            r - inset,
            b - inset,
            shortest * 0.22,
        ] {
            out.push(value);
        }
    } else {
        for _ in 0..6 {
            out.push(0.0);
        }
    }
    // Guide circles fading outwards.
    let guides = out.hole();
    let mut n = 0;
    let mut r = 2.6;
    while r < reach {
        out.push(1.0 - r / (reach * 1.15));
        runs(
            camera,
            96,
            |i| {
                let (x, z) = circle[i];
                V3::new(x * r, 0.0, z * r)
            },
            out,
        );
        n += 1;
        r += 2.6;
    }
    out.fill(guides, n as f64);
}

/// Whether body `body` (of system `system`) stays bright while something is
/// selected.
fn lit(scene: &Scene, frame: &Frame, body: usize, system: usize) -> bool {
    let selected = frame.selected;
    if selected == -1 {
        return true;
    }
    if selected == body as i64 || selected == system as i64 {
        return true;
    }
    let s = &scene.systems[system];
    let n = scene.systems.len() as i64;
    body == system && selected >= n + s.first as i64 && selected < n + (s.first + s.count) as i64
}

fn fade(frame: &Frame, depth: f64) -> f64 {
    let reference = frame.distance;
    (1.25 - (depth - reference) / (reference * 1.6)).clamp(0.42, 1.0)
}

fn systems(scene: &Scene, frame: &Frame, camera: &Camera, out: &mut Out, scratch: &mut Scratch) {
    let Scratch {
        ordered,
        hits,
        back,
        front,
        placed,
        circle,
        ..
    } = scratch;
    ordered.clear();
    ordered.extend(
        scene
            .systems
            .iter()
            .enumerate()
            .filter_map(|(i, s)| camera.at(s.position).map(|p| (i, p))),
    );
    ordered.sort_unstable_by(|a, b| b.1.depth.total_cmp(&a.1.depth).then(a.0.cmp(&b.0)));
    let n_systems = scene.systems.len();
    hits.clear();
    out.push(ordered.len() as f64);
    for &(index, core) in ordered.iter() {
        let system = &scene.systems[index];
        let online = system.online;
        let base = (if online { 1.0 } else { 0.45 })
            * (if lit(scene, frame, index, index) {
                1.0
            } else {
                0.3
            })
            * fade(frame, core.depth);
        let breath = if frame.still {
            1.0
        } else {
            1.0 + 0.04 * (frame.time * 0.9 + system.position.x).sin()
        };
        let ceiling = if system.home {
            26.0
        } else if online {
            15.0
        } else {
            11.0
        };
        let radius =
            ((if system.home { 0.22 } else { 0.15 }) * core.scale).clamp(3.0, ceiling) * breath;
        for value in [index as f64, core.x, core.y, radius, core.depth, base] {
            out.push(value);
        }
        // One orbit per ring, split at the star so the front half covers it.
        let planets = &scene.planets[system.first..system.first + system.count];
        back.clear();
        front.clear();
        for (i, ring) in planets.iter().enumerate() {
            if planets[..i].iter().any(|p| p.ring == ring.ring) {
                continue;
            }
            let mut run: Option<(bool, usize)> = None;
            for &(x, z) in circle.iter() {
                let local = tilt_around(
                    V3::new(x * ring.radius, 0.0, z * ring.radius),
                    ring.tilt_axis,
                    ring.tilt,
                );
                let Some(p) = camera.at(system.position.add(local)) else {
                    run = None;
                    continue;
                };
                let behind = p.depth > core.depth;
                match run {
                    Some((side, at)) if side == behind => {
                        let list = if side { &mut *back } else { &mut *front };
                        list.push(at, (p.x, p.y));
                    }
                    _ => {
                        let previous = run.and_then(|(side, at)| {
                            let list = if side { &*back } else { &*front };
                            list.last(at)
                        });
                        let list = if behind { &mut *back } else { &mut *front };
                        let at = list.begin(previous, (p.x, p.y));
                        run = Some((behind, at));
                    }
                }
            }
        }
        back.write(out);
        front.write(out);
        // Planets where they are drawn now.
        let count = out.hole();
        placed.clear();
        for (offset, planet) in planets.iter().enumerate() {
            let p = system.first + offset;
            let appear = if frame.still || planet.appeared.is_nan() {
                1.0
            } else {
                ease_out((frame.time - planet.appeared) / 1.6)
            };
            let orbit = planet.offset_at(frame.orbit_time());
            // A new planet spirals out of its star.
            let local = if appear >= 1.0 {
                orbit
            } else {
                turned(orbit, (1.0 - appear) * PI).mul(appear).add(V3::new(
                    0.0,
                    0.25 * (1.0 - appear),
                    0.0,
                ))
            };
            let Some(at) = camera.at(system.position.add(local)) else {
                continue;
            };
            let opacity = (if online { 1.0 } else { 0.45 })
                * (if lit(scene, frame, n_systems + p, index) {
                    1.0
                } else {
                    0.3
                })
                * fade(frame, at.depth)
                * appear;
            let radius = (planet.size * at.scale).clamp(2.5, 13.0) * (0.4 + 0.6 * appear);
            let ripple = if frame.still || planet.rippled.is_nan() {
                -1.0
            } else {
                let t = (frame.time - planet.rippled) / 1.4;
                if t >= 1.0 { -1.0 } else { t }
            };
            let behind = at.depth > core.depth;
            for value in [
                p as f64,
                at.x,
                at.y,
                radius,
                at.depth,
                opacity,
                ripple,
                f64::from(u8::from(behind)),
            ] {
                out.push(value);
            }
            placed.push((n_systems + p, at, radius, behind));
        }
        out.fill(count, placed.len() as f64);
        // Hits in drawing order: planets behind, the star, planets in front.
        for &(body, at, radius, behind) in placed.iter() {
            if behind {
                hits.push(Hit {
                    body,
                    x: at.x,
                    y: at.y,
                    radius,
                });
            }
        }
        hits.push(Hit {
            body: index,
            x: core.x,
            y: core.y,
            radius: radius * 1.6,
        });
        for &(body, at, radius, behind) in placed.iter() {
            if !behind {
                hits.push(Hit {
                    body,
                    x: at.x,
                    y: at.y,
                    radius,
                });
            }
        }
    }
}

/// Each live tunnel as a raised quadratic curve from its planet to the body it
/// ends at, with three particles travelling along it: faster on a direct one.
fn links(scene: &Scene, frame: &Frame, camera: &Camera, out: &mut Out) {
    let count = out.hole();
    let mut n = 0;
    let planet_at = |index: usize| {
        let p = &scene.planets[index];
        scene.systems[p.system]
            .position
            .add(p.offset_at(frame.orbit_time()))
    };
    for (index, link) in scene.links.iter().enumerate() {
        let from = planet_at(link.from);
        let to = match link.to.checked_sub(scene.systems.len()) {
            Some(planet) => planet_at(planet),
            None => scene.systems[link.to].position,
        };
        let rise = to.sub(from).length() * 0.42 + 0.4;
        let control = from.lerp(to, 0.5).add(V3::new(0.0, rise, 0.0));
        let at = |t: f64| {
            from.mul((1.0 - t) * (1.0 - t))
                .add(control.mul(2.0 * (1.0 - t) * t))
                .add(to.mul(t * t))
        };
        let mut points = [(0.0, 0.0); 33];
        let mut len = 0;
        for i in 0..=32 {
            if let Some(p) = camera.at(at(i as f64 / 32.0)) {
                points[len] = (p.x, p.y);
                len += 1;
            }
        }
        if len < 2 {
            continue;
        }
        out.push(index as f64);
        out.points(&points[..len]);
        let speed = if link.direct { 0.34 } else { 0.22 };
        let mut particles = [(0.0, 0.0); 3];
        let mut len = 0;
        for k in 0..3 {
            let t = if frame.still {
                (k + 1) as f64 / 4.0
            } else {
                (frame.time * speed + k as f64 / 3.0).rem_euclid(1.0)
            };
            if let Some(p) = camera.at(at(t)) {
                particles[len] = (p.x, p.y);
                len += 1;
            }
        }
        out.points(&particles[..len]);
        n += 1;
    }
    out.fill(count, n as f64);
}

fn stars(scene: &Scene, frame: &Frame, out: &mut Out) {
    let (w, h) = (frame.width, frame.height);
    for layer in &scene.layers {
        let shift_x = (frame.yaw / (PI * 2.0)) * w * layer.parallax * 2.2;
        let shift_y = (frame.tilt - 0.52) * h * layer.parallax * 0.6;
        for (b, (phase, range)) in layer.buckets.iter().enumerate() {
            let twinkle = if frame.still {
                0.8
            } else {
                0.7 + 0.3 * (frame.time * (0.6 + b as f64 * 0.23) + phase).sin()
            };
            out.push(twinkle);
            let units = &scene.stars[range.clone()];
            out.push((units.len() / 2) as f64);
            for unit in units.as_chunks::<2>().0 {
                out.push((unit[0] * w + shift_x).rem_euclid(w));
                out.push((unit[1] * h + shift_y).rem_euclid(h));
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Rect {
    l: f64,
    t: f64,
    r: f64,
    b: f64,
}

impl Rect {
    fn overlaps(&self, o: &Rect) -> bool {
        !(self.r <= o.l || o.r <= self.l || self.b <= o.t || o.b <= self.t)
    }
}

fn labels(scene: &Scene, frame: &Frame, out: &mut Out, scratch: &mut Scratch) {
    let Scratch {
        hits,
        label_order: order,
        label_stars: stars,
        label_rects: placed,
        ..
    } = scratch;
    let n_systems = scene.systems.len();
    let emphasized =
        |body: usize| [frame.selected, frame.hovered, frame.focused].contains(&(body as i64));
    let rank = |hit: &Hit| {
        if emphasized(hit.body) {
            3
        } else if hit.body < n_systems {
            2
        } else {
            1
        }
    };
    order.clear();
    order.extend(0..hits.len());
    // Stable: equals keep their drawing order.
    order.sort_unstable_by(|&a, &b| {
        rank(&hits[b])
            .cmp(&rank(&hits[a]))
            .then(hits[b].radius.total_cmp(&hits[a].radius))
            .then(a.cmp(&b))
    });
    // Stars are obstacles: a planet's label never lies over one.
    stars.clear();
    stars.extend(hits.iter().filter(|h| h.body < n_systems).map(|h| Rect {
        l: h.x - h.radius * 0.7,
        t: h.y - h.radius * 0.7,
        r: h.x + h.radius * 0.7,
        b: h.y + h.radius * 0.7,
    }));
    placed.clear();
    let count = out.hole();
    let mut shown = 0;
    let mut n = 0;
    for &i in order.iter() {
        let hit = &hits[i];
        let system = hit.body < n_systems;
        let emphasis = emphasized(hit.body);
        if !emphasis && !system && hit.radius < 5.0 {
            continue;
        }
        if shown >= 28 && !emphasis {
            break;
        }
        let strong = system || emphasis;
        let (w, h) = if system {
            scene.systems[hit.body].label
        } else {
            let planet = &scene.planets[hit.body - n_systems];
            if strong { planet.strong } else { planet.label }
        };
        let (x, y) = (
            hit.x - w / 2.0,
            hit.y + hit.radius + if system { 8.0 } else { 5.0 },
        );
        let rect = Rect {
            l: x - 3.0,
            t: y - 3.0,
            r: x + w + 3.0,
            b: y + h + 3.0,
        };
        if !emphasis
            && (placed.iter().any(|p| rect.overlaps(p))
                || (!system && stars.iter().any(|s| rect.overlaps(s))))
        {
            continue;
        }
        if rect.r < 0.0 || rect.l > frame.width || rect.t > frame.height {
            continue;
        }
        placed.push(rect);
        for value in [hit.body as f64, f64::from(u8::from(strong)), x, y] {
            out.push(value);
        }
        shown += 1;
        n += 1;
    }
    out.fill(count, n as f64);
}
