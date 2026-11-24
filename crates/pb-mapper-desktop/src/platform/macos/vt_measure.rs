//! Measurements of VideoToolbox's rate control on this Mac, as a Mac host
//! drives it: what it drops, how soon a dropped last picture reaches the
//! viewer, what it sustains at 120 FPS, and how it answers a bitrate cut. Each
//! is an ignored test that paces a real session on IOSurface pictures painted
//! before the run, so no Screen Recording grant is needed. Run one at a time,
//! in release mode, on a quiet machine:
//! `cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture
//! --test-threads=1 <name>`.
#![cfg(target_os = "macos")]

use super::{
    encoder::{Admit, Encoder, Options},
    tests::{quantile, report, surface},
    video::VT_IN_FLIGHT,
};
use crate::{
    VideoCodec, Waker,
    codec::pipeline::{LATE_OUTPUT, LIVENESS_INTERVAL},
    pacer::Pacer,
    scheduler::{Waiter, Wake},
};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use std::time::{Duration, Instant};

/// What a ring of pictures shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Content {
    /// Lines of text on white scrolling up, as a document or a terminal.
    Text,
    /// The text with image blocks, under fresh grain of ±6 over all of the
    /// luma, as photos scrolling: what no reference predicts.
    Busy,
    /// Fresh noise over all of the luma, from 64 to 191: nothing of one
    /// picture predicts the next, the worst a full-motion video gives.
    Noise,
}

/// Rows the content scrolls a picture.
const SCROLL: usize = 4;

/// Pictures of `content` at `width` x `height`, `len` of them, painted
/// before the run so that the pacing thread does nothing else.
fn ring(
    width: usize,
    height: usize,
    content: Content,
    len: usize,
) -> Vec<CFRetained<CVPixelBuffer>> {
    let document = document(width, height + len * SCROLL, content == Content::Busy);
    (0..len)
        .map(|frame| {
            let picture = surface(width, height);
            paint(&picture, &document, frame, content);
            picture
        })
        .collect()
}

/// A xorshift step.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The luma of a document `width` wide and `rows` tall: lines of glyphs of
/// random length, a glyph a block of random ink, and, when `images`, a block
/// of busy pattern beside one line in nine.
fn document(width: usize, rows: usize, images: bool) -> Vec<u8> {
    let mut luma = vec![235u8; width * rows];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let line = (width / 100).clamp(18, 40);
    let glyph = line / 2;
    let mut y = 0;
    while y + line <= rows {
        let length = (next(&mut state) % (width as u64 - 40)) as usize + 40;
        let mut x = 20;
        while x + glyph < length.min(width - 20) {
            let bits = next(&mut state);
            if bits & 7 != 0 {
                for gy in 2..line - 4 {
                    for gx in 1..glyph - 1 {
                        if (bits >> ((gy * 3 + gx) % 60)) & 1 == 1 {
                            luma[(y + gy) * width + x + gx] = 16;
                        }
                    }
                }
            }
            x += glyph;
        }
        if images && next(&mut state).is_multiple_of(9) {
            let (left, right) = (width / 2, width / 2 + width / 3);
            for row in y..(y + line * 4).min(rows) {
                for (column, pixel) in luma[row * width..][left..right].iter_mut().enumerate() {
                    *pixel =
                        ((left + column) * 3 + row * 5 + (next(&mut state) & 31) as usize) as u8;
                }
            }
        }
        y += line;
    }
    luma
}

/// Picture `frame` of `content`: the document scrolled `frame * SCROLL`
/// rows, with fresh grain of ±6 when busy, or fresh noise; grey chroma.
fn paint(picture: &CVPixelBuffer, document: &[u8], frame: usize, content: Content) {
    let width = CVPixelBufferGetWidth(picture);
    let mut state = (frame as u64 + 1).wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    // SAFETY: locked around the writes.
    assert_eq!(
        unsafe { CVPixelBufferLockBaseAddress(picture, CVPixelBufferLockFlags(0)) },
        0
    );
    for plane in 0..2 {
        let stride = CVPixelBufferGetBytesPerRowOfPlane(picture, plane);
        let rows = CVPixelBufferGetHeightOfPlane(picture, plane);
        let base = CVPixelBufferGetBaseAddressOfPlane(picture, plane).cast::<u8>();
        assert!(!base.is_null() && stride >= width);
        // SAFETY: the locked plane's rows, `stride` bytes each.
        let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
        for (y, row) in bytes.chunks_exact_mut(stride).enumerate() {
            let row = &mut row[..width];
            if plane == 1 {
                row.fill(128);
                continue;
            }
            if content == Content::Noise {
                for chunk in row.chunks_mut(8) {
                    let noise = next(&mut state).to_le_bytes();
                    for (pixel, noise) in chunk.iter_mut().zip(noise) {
                        *pixel = 64 + (noise & 127);
                    }
                }
                continue;
            }
            let from = (y + frame * SCROLL) * width;
            row.copy_from_slice(&document[from..from + width]);
            if content == Content::Busy {
                for chunk in row.chunks_mut(8) {
                    let noise = next(&mut state).to_le_bytes();
                    for (pixel, noise) in chunk.iter_mut().zip(noise) {
                        *pixel =
                            (i16::from(*pixel) + i16::from(noise % 13) - 6).clamp(0, 255) as u8;
                    }
                }
            }
        }
    }
    // SAFETY: locked above.
    unsafe { CVPixelBufferUnlockBaseAddress(picture, CVPixelBufferLockFlags(0)) };
}

/// Bits a frame at 1080p, scaled by the encoded pixels to the power 0.75
/// within 0.4 and 3, as a session's QoS scales them.
fn scaled(bits_1080p: f64, width: usize, height: usize) -> f64 {
    let scale = (width as f64 * height as f64 / (1920.0 * 1080.0))
        .powf(0.75)
        .clamp(0.4, 3.0);
    bits_1080p * scale
}

/// Balanced's and Smooth's starting bits per frame at 1080p.
const BALANCED_BPF: f64 = 66_667.0;
const SMOOTH_BPF: f64 = 50_000.0;

/// One unit out of the encoder.
struct Out {
    at: Instant,
    bits: u64,
    keyframe: bool,
    pts: u64,
    dwell_us: u32,
    /// Luma mean squared error, where the session measures it.
    error: Option<f64>,
}

/// One encoder paced as thread C paces it: each tick taken at its start, two
/// never closer than three quarters of a period, a tick that finds
/// `VT_IN_FLIGHT` units in flight skipped (`MacVideo::full`), one that
/// finds the submit thread busy left armed until that thread wakes this one.
struct Rig {
    encoder: Encoder,
    waiter: Waiter,
    ring: Vec<CFRetained<CVPixelBuffer>>,
    /// The ring's next picture.
    next: usize,
    keyframe: bool,
    submitted: u64,
    skipped: u64,
    armed: u64,
    /// Each picture handed over: its presentation time and ring index.
    sent: Vec<(u64, usize)>,
    out: Vec<Out>,
}

impl Rig {
    /// A session of `codec` over `ring` at a nominal `fps` and `bitrate`,
    /// built with `options`; `None`, reported, where VideoToolbox refuses it.
    fn new(
        codec: VideoCodec,
        ring: Vec<CFRetained<CVPixelBuffer>>,
        fps: u32,
        bitrate: u32,
        options: Options,
        label: &str,
    ) -> Option<Self> {
        let width = CVPixelBufferGetWidth(&ring[0]) as u32;
        let height = CVPixelBufferGetHeight(&ring[0]) as u32;
        let wake = Wake::new();
        let waiter = Waiter::new(&wake).unwrap();
        match Encoder::tuned(
            codec,
            width,
            height,
            fps,
            bitrate,
            &Waker(wake.clone()),
            options,
        ) {
            Ok(encoder) => Some(Self {
                encoder,
                waiter,
                ring,
                next: 0,
                keyframe: true,
                submitted: 0,
                skipped: 0,
                armed: 0,
                sent: Vec::new(),
                out: Vec::new(),
            }),
            Err(failure) => {
                report(format_args!("{label} refused: {failure}"));
                None
            }
        }
    }

    fn collect(&mut self) {
        while let Some(unit) = self.encoder.drain().unwrap() {
            let pts = unit.format.pts_us;
            let error = self.encoder.probe(|probe| {
                while probe.quality.front().is_some_and(|(at, _)| *at < pts) {
                    probe.quality.pop_front();
                }
                probe
                    .quality
                    .front()
                    .filter(|(at, _)| *at == pts)
                    .map(|(_, error)| *error)
            });
            self.out.push(Out {
                at: Instant::now(),
                bits: unit.bytes.len() as u64 * 8,
                keyframe: unit.format.keyframe,
                pts,
                dwell_us: unit.format.encode_us,
                error,
            });
        }
    }

    /// Hand the encoder ring picture `index`; the next tick's due time is the
    /// pacer's to say.
    fn hand(&mut self, index: usize, duplicate: bool) {
        let keyframe = std::mem::take(&mut self.keyframe);
        self.encoder
            .encode(&self.ring[index % self.ring.len()], keyframe, duplicate)
            .unwrap();
        self.sent.push((self.encoder.last_pts(), index));
        self.submitted += 1;
    }

    /// Until `until`: a new picture of the ring at each tick at `pace`.
    fn run(&mut self, pace: u32, until: Instant) {
        let mut pacer = Pacer::new(pace, Instant::now());
        loop {
            self.collect();
            let now = Instant::now();
            if now >= until {
                return;
            }
            if pacer.due(now) && !self.encoder.waiting() {
                self.keyframe |= self.encoder.keyframe_due();
                match self.encoder.admit(VT_IN_FLIGHT) {
                    Admit::Full => {
                        self.skipped += 1;
                        pacer.consume(now, None);
                    }
                    Admit::Busy => self.armed += 1,
                    Admit::Free => {
                        let index = self.next;
                        self.next += 1;
                        self.hand(index, false);
                        pacer.consume(now, None);
                    }
                }
            }
            let wait = if self.encoder.waiting() {
                now + LATE_OUTPUT
            } else {
                pacer.earliest()
            };
            self.waiter.wait(wait.min(until)).unwrap();
        }
    }

    /// What came out between `from` and `to`.
    fn window(&self, from: Instant, to: Instant) -> Window {
        let units: Vec<&Out> = self
            .out
            .iter()
            .filter(|out| (from..to).contains(&out.at))
            .collect();
        let errors: Vec<f64> = units.iter().filter_map(|out| out.error).collect();
        let mut dwell: Vec<u32> = units.iter().map(|out| out.dwell_us).collect();
        Window {
            seconds: to.saturating_duration_since(from).as_secs_f64(),
            units: units.len(),
            bits: units.iter().map(|out| out.bits).sum(),
            largest_p: units
                .iter()
                .filter(|out| !out.keyframe)
                .map(|out| out.bits)
                .max()
                .unwrap_or(0),
            keyframe_bits: units
                .iter()
                .filter(|out| out.keyframe)
                .map(|out| out.bits)
                .max(),
            psnr: psnr(&errors),
            dwell_p50_ms: f64::from(quantile(&mut dwell, 0.5)) / 1000.0,
            dwell_p95_ms: f64::from(quantile(&mut dwell, 0.95)) / 1000.0,
        }
    }

    fn dropped(&self) -> u64 {
        self.encoder.probe(|probe| probe.dropped)
    }
}

/// What came out over a window.
struct Window {
    seconds: f64,
    units: usize,
    bits: u64,
    largest_p: u64,
    keyframe_bits: Option<u64>,
    psnr: f64,
    dwell_p50_ms: f64,
    dwell_p95_ms: f64,
}
impl Window {
    fn per_second(&self, count: f64) -> f64 {
        count / self.seconds.max(f64::EPSILON)
    }
}

/// The PSNR of the mean of 8-bit luma mean squared errors, NaN for none.
fn psnr(errors: &[f64]) -> f64 {
    if errors.is_empty() {
        return f64::NAN;
    }
    let mean = errors.iter().sum::<f64>() / errors.len() as f64;
    10.0 * (255.0 * 255.0 / mean.max(f64::MIN_POSITIVE)).log10()
}

/// The low-latency session a Mac host builds, measuring its luma error.
fn host_options() -> Options {
    Options {
        low_latency: true,
        capped: true,
        quality: true,
    }
}

/// What low-latency rate control drops, and how it codes what it keeps:
/// each codec at 1080p and 1440p, paced at 60 and 120 FPS at Balanced's
/// starting bits per frame scaled by the size, on scrolling text, the busy
/// pattern and fresh noise. Two windows a case: the 3 s from 3 s after the
/// session's IDR (steady state), and the 1.5 s after an IDR forced at 6 s.
/// Each prints the pictures handed over and VideoToolbox's `FrameDropped`
/// outputs, units a second, the largest unit that is not a keyframe, the
/// forced IDR's size, and PSNR from VideoToolbox's own luma error.
#[test]
#[ignore = "a measurement: about 3.5 minutes of encoding, in release mode"]
fn video_toolbox_drops() {
    for (width, height) in [(1920, 1080), (2560, 1440)] {
        for content in [Content::Noise, Content::Busy, Content::Text] {
            let painted = ring(width, height, content, 12);
            for codec in [VideoCodec::H264, VideoCodec::H265] {
                for pace in [60, 120] {
                    drops_case(codec, &painted, content, pace);
                }
            }
        }
    }
}

fn drops_case(
    codec: VideoCodec,
    painted: &[CFRetained<CVPixelBuffer>],
    content: Content,
    pace: u32,
) {
    let (width, height) = (
        CVPixelBufferGetWidth(&painted[0]),
        CVPixelBufferGetHeight(&painted[0]),
    );
    let bitrate = (scaled(BALANCED_BPF, width, height) * f64::from(pace)) as u32;
    let label = format!(
        "codec={codec:?} size={width}x{height} content={content:?} pace={pace} bitrate={bitrate}"
    );
    let Some(mut rig) = Rig::new(
        codec,
        painted.to_vec(),
        pace,
        bitrate,
        host_options(),
        &label,
    ) else {
        return;
    };
    let start = Instant::now();
    let steady = start + Duration::from_secs(3);
    rig.run(pace, steady);
    let (submitted, dropped) = (rig.submitted, rig.dropped());
    let forced = steady + Duration::from_secs(3);
    rig.run(pace, forced);
    let (steady_submitted, steady_dropped) = (rig.submitted - submitted, rig.dropped() - dropped);
    let (submitted, dropped) = (rig.submitted, rig.dropped());
    rig.keyframe = true;
    let after = forced + Duration::from_millis(1500);
    rig.run(pace, after);
    let (idr_submitted, idr_dropped) = (rig.submitted - submitted, rig.dropped() - dropped);
    for (name, from, to, submitted, dropped) in [
        ("steady", steady, forced, steady_submitted, steady_dropped),
        ("after_idr", forced, after, idr_submitted, idr_dropped),
    ] {
        let window = rig.window(from, to);
        report(format_args!(
            "{label} window={name} submitted={submitted} dropped={dropped} units_per_s={:.1} mbps={:.2} largest_p_bits={} idr_bits={} psnr={:.2}",
            window.per_second(window.units as f64),
            window.per_second(window.bits as f64) / 1e6,
            window.largest_p,
            window
                .keyframe_bits
                .map_or_else(|| "-".to_owned(), |bits| bits.to_string()),
            window.psnr,
        ));
    }
}

/// How soon the last of a few new pictures reaches the viewer after an IDR
/// on a slow link, where low-latency rate control drops about every other
/// picture while it pays the IDR back. Each codec at 1080p, paced at 60 FPS,
/// at 1, 2 and 4 Mbit/s: a trial lets the screen stand still for 2 s (a
/// repeat every `LIVENESS_INTERVAL`), forces an IDR of the still picture as
/// a control grant does, hands over `k` new pictures of scrolling text, one
/// a tick, and stops. It prints, for each `k` from 1 to 12, the time from
/// the last new picture's submit to the first unit kept that carries it,
/// as `MacVideo` would send it, and then a summary of each bitrate.
#[test]
#[ignore = "a measurement: about 3 minutes of encoding, in release mode"]
fn video_toolbox_resubmit() {
    let painted = ring(1920, 1080, Content::Text, 16);
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        for bitrate in [1_000_000, 2_000_000, 4_000_000] {
            resubmit_case(codec, &painted, bitrate);
        }
    }
}

/// The pace and nominal rate of `video_toolbox_resubmit`.
const RESUBMIT_PACE: u32 = 60;
/// How long a trial waits for the last new picture.
const RESUBMIT_WAIT: Duration = Duration::from_millis(600);

fn resubmit_case(codec: VideoCodec, painted: &[CFRetained<CVPixelBuffer>], bitrate: u32) {
    let label = format!("codec={codec:?} size=1920x1080 pace={RESUBMIT_PACE} bitrate={bitrate}");
    let Some(mut rig) = Rig::new(
        codec,
        painted.to_vec(),
        RESUBMIT_PACE,
        bitrate,
        host_options(),
        &label,
    ) else {
        return;
    };
    // The session's own IDR and a second of motion before the first trial.
    rig.run(RESUBMIT_PACE, Instant::now() + Duration::from_secs(1));
    let mut delays = Vec::new();
    for k in 1..=12 {
        let mut host = Emulated::new(&rig);
        host.still(&mut rig, Instant::now() + Duration::from_secs(2));
        rig.keyframe = true;
        let delay = host.trial(&mut rig, k);
        report(format_args!(
            "{label} k={k} delay_ms={} dropped_total={}",
            delay.map_or_else(
                || "none".to_owned(),
                |delay| format!("{:.1}", delay.as_secs_f64() * 1e3)
            ),
            rig.dropped(),
        ));
        delays.push(delay.unwrap_or(RESUBMIT_WAIT));
    }
    let tick = Duration::from_secs(1) / RESUBMIT_PACE;
    report(format_args!(
        "{label} summary trials={} at_least_250ms={} within_2_ticks={} max_ms={:.1}",
        delays.len(),
        delays
            .iter()
            .filter(|delay| **delay >= LIVENESS_INTERVAL)
            .count(),
        delays.iter().filter(|delay| **delay <= tick * 2).count(),
        delays
            .iter()
            .max()
            .copied()
            .unwrap_or_default()
            .as_secs_f64()
            * 1e3,
    ));
}

/// `MacVideo::submit`'s policy over the rig's encoder, as a still screen
/// with `fresh` new pictures waiting meets it: a new picture each due tick
/// while there are any, else the last again for a keyframe or after
/// `LIVENESS_INTERVAL`, marked a duplicate.
struct Emulated {
    last_submit: Instant,
    /// The ring index of the last picture handed over.
    last: usize,
    fresh: usize,
    pacer: Pacer,
}

impl Emulated {
    fn new(rig: &Rig) -> Self {
        let now = Instant::now();
        Self {
            last_submit: now,
            last: rig.next.saturating_sub(1),
            fresh: 0,
            pacer: Pacer::new(RESUBMIT_PACE, now),
        }
    }

    /// One pass of thread C: a submit when due, then a wait for the next.
    fn step(&mut self, rig: &mut Rig, until: Instant) {
        rig.collect();
        let now = Instant::now();
        if now >= until {
            return;
        }
        if self.pacer.due(now) && !rig.encoder.waiting() {
            rig.keyframe |= rig.encoder.keyframe_due();
            match rig.encoder.admit(VT_IN_FLIGHT) {
                Admit::Full => self.pacer.consume(now, None),
                Admit::Busy => {}
                Admit::Free => {
                    let repeat = rig.keyframe || now >= self.last_submit + LIVENESS_INTERVAL;
                    if self.fresh > 0 {
                        self.fresh -= 1;
                        self.last = rig.next;
                        rig.next += 1;
                        rig.hand(self.last, false);
                        self.last_submit = now;
                        self.pacer.consume(now, None);
                    } else if repeat {
                        rig.hand(self.last, true);
                        self.last_submit = now;
                        self.pacer.consume(now, None);
                    }
                }
            }
        }
        // `MacVideo::idle_until`: nothing new is due before the liveness
        // repeat unless a keyframe is.
        let idle = self.fresh == 0 && !rig.keyframe && !rig.encoder.wants_keyframe();
        let wait = if rig.encoder.waiting() {
            now + LATE_OUTPUT
        } else if idle {
            (self.last_submit + LIVENESS_INTERVAL).max(self.pacer.earliest())
        } else {
            self.pacer.earliest()
        };
        rig.waiter.wait(wait.min(until)).unwrap();
    }

    /// A still screen until `until`.
    fn still(&mut self, rig: &mut Rig, until: Instant) {
        while Instant::now() < until {
            self.step(rig, until);
        }
    }

    /// `k` new pictures after the IDR the rig asks for, then a still
    /// screen: the time from the last new picture's submit to the first
    /// unit kept that carries it, if one came within `RESUBMIT_WAIT`.
    fn trial(&mut self, rig: &mut Rig, k: usize) -> Option<Duration> {
        // The IDR of the still picture, then the new pictures.
        let start = Instant::now();
        while rig.keyframe {
            self.step(rig, start + Duration::from_secs(5));
        }
        self.fresh = k;
        while self.fresh > 0 {
            self.step(rig, start + Duration::from_secs(5));
        }
        let submitted = self.last_submit;
        let last = self.last;
        let deadline = submitted + RESUBMIT_WAIT;
        loop {
            let carried = rig.out.iter().find(|out| {
                out.at >= submitted
                    && rig
                        .sent
                        .iter()
                        .any(|(pts, index)| *pts == out.pts && *index == last)
            });
            if let Some(out) = carried {
                return Some(out.at.saturating_duration_since(submitted));
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.step(rig, deadline);
        }
    }
}

/// What VideoToolbox sustains at a nominal 120 FPS at a Mac's own sizes, on
/// the settings a Mac host encodes with (presentation times from the wall
/// clock, the submit thread, two units in flight), each codec, low latency
/// against standard rate control with and without DataRateLimits, at the
/// target and at 0.85 and 0.75 of it, on scrolling text and the busy
/// pattern. The target is Smooth's starting bits per frame scaled by the
/// size, times the pace under the `timestamp` rate model, times the nominal
/// rate under `nominal`; pace 120 is both. Each case runs 1 s and is
/// measured over the next 2 s: units a second, VideoToolbox's drops, the
/// dwell (p50, p95), the encode call on the submit thread (p50), what it
/// produced against the target, and PSNR.
#[test]
#[ignore = "a measurement: about 11 minutes of encoding, in release mode"]
fn video_toolbox_120() {
    for (width, height) in [(2560, 1440), (2560, 1664), (3024, 1964), (3840, 2160)] {
        for content in [Content::Text, Content::Busy] {
            let painted = ring(width, height, content, 12);
            for codec in [VideoCodec::H265, VideoCodec::H264] {
                for (pace, model) in [(120, "both"), (60, "timestamp"), (60, "nominal")] {
                    for mode in modes(codec) {
                        case_120(codec, &painted, content, pace, model, mode);
                    }
                }
            }
        }
    }
}

/// A rate-control arm of `video_toolbox_120`: low latency or not, capped or
/// not, and the share of the target the session is set to.
#[derive(Clone, Copy)]
struct Mode {
    name: &'static str,
    low_latency: bool,
    capped: bool,
    factor: f64,
}

/// H.265's arms; H.264 gains nothing from standard rate control, so only
/// its baseline arms run.
fn modes(codec: VideoCodec) -> Vec<Mode> {
    let mode = |name, low_latency, capped, factor| Mode {
        name,
        low_latency,
        capped,
        factor,
    };
    let mut modes = vec![
        mode("ll", true, true, 1.0),
        mode("std_capped", false, true, 1.0),
        mode("std_1.00", false, false, 1.0),
    ];
    if codec == VideoCodec::H265 {
        modes.push(mode("std_0.85", false, false, 0.85));
        modes.push(mode("std_0.75", false, false, 0.75));
    }
    modes
}

/// `video_toolbox_120`'s nominal rate.
const NOMINAL_120: u32 = 120;

fn case_120(
    codec: VideoCodec,
    painted: &[CFRetained<CVPixelBuffer>],
    content: Content,
    pace: u32,
    model: &str,
    mode: Mode,
) {
    let (width, height) = (
        CVPixelBufferGetWidth(&painted[0]),
        CVPixelBufferGetHeight(&painted[0]),
    );
    let bpf = scaled(SMOOTH_BPF, width, height);
    let target = bpf * f64::from(pace);
    let budget = if model == "nominal" {
        bpf * f64::from(NOMINAL_120)
    } else {
        target
    };
    let bitrate = (budget * mode.factor) as u32;
    let label = format!(
        "codec={codec:?} size={width}x{height} content={content:?} pace={pace} model={model} mode={}",
        mode.name
    );
    let options = Options {
        low_latency: mode.low_latency,
        capped: mode.capped,
        quality: true,
    };
    let Some(mut rig) = Rig::new(
        codec,
        painted.to_vec(),
        NOMINAL_120,
        bitrate,
        options,
        &label,
    ) else {
        return;
    };
    let start = Instant::now();
    let (from, to) = (
        start + Duration::from_secs(1),
        start + Duration::from_secs(3),
    );
    rig.run(pace, from);
    let (submitted, dropped) = (rig.submitted, rig.dropped());
    rig.encoder.probe(|probe| probe.calls.clear());
    rig.run(pace, to);
    let mut calls = rig.encoder.probe(|probe| std::mem::take(&mut probe.calls));
    let window = rig.window(from, to);
    report(format_args!(
        "{label} bitrate={bitrate} submitted_per_s={:.1} units_per_s={:.1} dropped={} dwell_p50_ms={:.2} dwell_p95_ms={:.2} call_p50_ms={:.2} produced_over_target={:.2} psnr={:.2}",
        window.per_second((rig.submitted - submitted) as f64),
        window.per_second(window.units as f64),
        rig.dropped() - dropped,
        window.dwell_p50_ms,
        window.dwell_p95_ms,
        f64::from(quantile(&mut calls, 0.5)) / 1000.0,
        window.per_second(window.bits as f64) / target,
        window.psnr,
    ));
}

/// How low-latency rate control answers a bitrate cut and its return: each
/// codec at 1080p on the busy pattern, paced at a nominal 60 FPS, set to
/// 8 Mbit/s, cut to 2 at picture 60 and raised to 8 again at picture 180,
/// until picture 300. It prints, after each change, how many pictures came
/// before one was within 1.2 times the new budget, VideoToolbox's drops, the
/// smallest and largest unit, and the first 20 units' sizes against the new
/// budget (`-` for a dropped picture).
#[test]
#[ignore = "a measurement: about 15 seconds of encoding, in release mode"]
fn video_toolbox_rate_step() {
    let painted = ring(1920, 1080, Content::Busy, 12);
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        rate_step_case(codec, &painted);
    }
}

fn rate_step_case(codec: VideoCodec, painted: &[CFRetained<CVPixelBuffer>]) {
    const PACE: u32 = 60;
    const STEPS: [(usize, u32); 3] = [(0, 8_000_000), (60, 2_000_000), (180, 8_000_000)];
    const END: usize = 300;
    let label = format!("codec={codec:?} size=1920x1080 content=Busy pace={PACE}");
    let Some(mut rig) = Rig::new(
        codec,
        painted.to_vec(),
        PACE,
        STEPS[0].1,
        host_options(),
        &label,
    ) else {
        return;
    };
    let mut drops_at = vec![0u64; END + 1];
    let mut pacer = Pacer::new(PACE, Instant::now());
    while (rig.submitted as usize) < END {
        rig.collect();
        let now = Instant::now();
        if pacer.due(now) && !rig.encoder.waiting() {
            rig.keyframe |= rig.encoder.keyframe_due();
            match rig.encoder.admit(VT_IN_FLIGHT) {
                Admit::Full => {
                    rig.skipped += 1;
                    pacer.consume(now, None);
                }
                Admit::Busy => rig.armed += 1,
                Admit::Free => {
                    let frame = rig.submitted as usize;
                    if let Some((_, bitrate)) = STEPS.iter().find(|(at, _)| *at == frame) {
                        rig.encoder.set_bitrate(*bitrate);
                    }
                    drops_at[frame] = rig.dropped();
                    let index = rig.next;
                    rig.next += 1;
                    rig.hand(index, false);
                    pacer.consume(now, None);
                }
            }
        }
        let wait = if rig.encoder.waiting() {
            now + LATE_OUTPUT
        } else {
            pacer.earliest()
        };
        rig.waiter.wait(wait).unwrap();
    }
    let settle = Instant::now() + Duration::from_millis(300);
    while Instant::now() < settle {
        rig.collect();
        rig.waiter.wait(settle).unwrap();
    }
    drops_at[END] = rig.dropped();
    // Each picture's unit size, by its presentation time; none for a drop.
    let sizes: Vec<Option<u64>> = rig
        .sent
        .iter()
        .map(|(pts, _)| {
            rig.out
                .iter()
                .find(|out| out.pts == *pts)
                .map(|out| out.bits)
        })
        .collect();
    let spans = [
        (STEPS[0].0, STEPS[0].1, STEPS[1].0),
        (STEPS[1].0, STEPS[1].1, STEPS[2].0),
        (STEPS[2].0, STEPS[2].1, END),
    ];
    for (from, bitrate, to) in spans {
        let budget = f64::from(bitrate) / f64::from(PACE);
        let kept: Vec<u64> = sizes[from..to].iter().flatten().copied().collect();
        let within = sizes[from..to]
            .iter()
            .position(|size| size.is_some_and(|bits| bits as f64 <= 1.2 * budget));
        let first: Vec<String> = sizes[from..(from + 20).min(to)]
            .iter()
            .map(|size| {
                size.map_or_else(
                    || "-".to_owned(),
                    |bits| format!("{:.2}", bits as f64 / budget),
                )
            })
            .collect();
        report(format_args!(
            "{label} from_picture={from} bitrate={bitrate} budget_bits={budget:.0} pictures_until_within_1.2={} dropped={} smallest_over_budget={:.2} largest_over_budget={:.2} mean_over_budget={:.2} first_20=[{}]",
            within.map_or_else(|| "never".to_owned(), |at| at.to_string()),
            drops_at[to] - drops_at[from],
            kept.iter().min().map_or(0.0, |bits| *bits as f64 / budget),
            kept.iter().max().map_or(0.0, |bits| *bits as f64 / budget),
            kept.iter().sum::<u64>() as f64 / kept.len().max(1) as f64 / budget,
            first.join(" "),
        ));
    }
}
