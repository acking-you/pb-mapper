//! A deterministic closed loop of one host session's QoS controller and
//! credit ledger, as `session.rs` wires them, around a simulated encoder, a
//! FIFO bottleneck and a viewer that receipts every unit and echoes every
//! probe. Time is virtual, so a case runs in milliseconds and the same every
//! time. The encoders: one that budgets per frame and follows a new bitrate
//! at once, one whose rate trails the bitrate it is given (a VBV), one that
//! budgets per second of presentation time, and a still picture. The link:
//! its rate, its delay each way, and stalls that hold both directions, each
//! scheduled over time.
//!
//! The slow-link suite (`slow_link.rs`) runs the real session over a relay
//! and TLS; this loop carries the controller's logic, and the cases the suite
//! cannot shape deterministically.

use super::*;
use crate::credit::CreditLedger;
use std::{cmp::Reverse, collections::BinaryHeap};

/// Virtual microseconds since the session began.
type Us = u64;

const MS: Us = 1_000;
const S: Us = 1_000_000;
/// A link with no limit carries this.
const UNLIMITED_BPS: u64 = 1_000_000_000;
/// The synthetic encoder's latency, as the slow-link suite's.
const ENCODE: Us = 8 * MS;
/// A probe's size on the wire.
const PROBE_BYTES: u64 = 64;
/// The viewer's clock: its arrival times are this far from the host's.
const VIEWER_OFFSET: Us = 7 * S;
/// Capture waits while this many units wait for credit, as thread C does.
const HANDOFF_WAITING: usize = 2;

/// How the encoder sizes each picture.
#[derive(Clone, Copy, Debug)]
pub(super) enum Encoder {
    /// `encoder_bps / nominal` a frame, at once; an IDR `idr` times that.
    PerFrame { idr: f64 },
    /// The same, at a rate that trails `encoder_bps` with this time
    /// constant, as a VBV does.
    Lagging { tau: Us, idr: f64 },
    /// `encoder_bps` over the time since the previous picture.
    PerSecond { idr: f64 },
    /// A still picture: a few hundred bytes a frame, whatever the bitrate.
    Still,
    /// Per frame, as `PerFrame`, but never below this many bytes: an encoder
    /// at the moving picture's content floor.
    Floor { bytes: u64 },
}

/// What the link does from an instant on.
#[derive(Clone, Copy, Debug)]
pub(super) enum Change {
    /// The bottleneck's rate toward the viewer, in bit/s; 0 is unlimited.
    Rate(u64),
    /// The delay each way, in ms.
    Delay { forward: Us, reverse: Us },
    /// Nothing crosses either way for this long, then all of it at once.
    Stall(Us),
    /// The picture changes: the encoder sizes it so from now on.
    Picture(Encoder),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Capture,
    Encoded { idr: bool, bytes: u64 },
    UnitArrived { seq: u64, bytes: u64 },
    ProbeArrived { nonce: u64 },
    Receipt { seq: u64, at_us: u64 },
    Echo { nonce: u64, at_us: u64 },
    Tick,
    Wake,
}

/// A unit waiting for credit.
#[derive(Clone, Copy, Debug)]
struct Waiting {
    idr: bool,
    bytes: u64,
    ready: Us,
}

/// One second of the run, as the trace would show it at its end.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Second {
    pub pace: u32,
    pub pace_min: u32,
    pub bpf: f64,
    pub expected_bps: u64,
    pub encoder_bps: u32,
    pub rate_fps: Option<u32>,
    pub rebase: Option<Duration>,
    pub cuts: u64,
    /// Units the viewer read in the second.
    pub received: u32,
    /// The bits the viewer read in the second.
    pub received_bits: u64,
    /// The longest a unit read in the second waited at the bottleneck,
    /// stalls included.
    pub queue_max: Duration,
    pub clamped: bool,
}

pub(super) struct Sim {
    t0: Instant,
    now: Us,
    pub qos: Qos,
    credit: CreditLedger,
    one_way: bool,
    encoder: Encoder,
    nominal: u32,
    /// The lagging encoder's rate.
    lagged_bps: f64,
    last_capture: Option<Us>,
    idr_due: bool,
    idr_every: Option<Us>,
    next_idr: Us,
    handoff: VecDeque<Waiting>,
    blocked: bool,
    produced: ProducedSnapshot,
    events: BinaryHeap<Reverse<(Us, u64, Event)>>,
    serial: u64,
    /// When B-send is woken next, if it waits for credit or a probe.
    wake: Option<Us>,
    /// What the link does from when, in order.
    changes: Vec<(Us, Change)>,
    rate_bps: u64,
    forward: Us,
    reverse: Us,
    stalls: Vec<(Us, Us)>,
    /// When the bottleneck is free, and the last arrival each way.
    free: Us,
    last_forward: Us,
    last_reverse: Us,
    /// The units in flight on the bottleneck: when each entered it.
    entered: VecDeque<(u64, Us)>,
    pub seconds: Vec<Second>,
    current: Second,
}
impl Sim {
    /// A Balanced H.264 session at 1080p60 over a link `rtt_ms` long, with
    /// arrival times when `one_way`.
    pub fn new(encoder: Encoder, rtt_ms: Us, one_way: bool) -> Self {
        Self::with(
            encoder,
            rtt_ms,
            one_way,
            QualityPreset::Balanced,
            RateModel::Nominal,
        )
    }

    pub fn with(
        encoder: Encoder,
        rtt_ms: Us,
        one_way: bool,
        quality: QualityPreset,
        rate_model: RateModel,
    ) -> Self {
        Self::build(encoder, rtt_ms, one_way, quality, rate_model, None)
    }

    /// `new`, for a session a closed Balanced session's `bpf` at 1080p, just
    /// ended, starts from (`ClosingRate`).
    pub fn after_closing(encoder: Encoder, rtt_ms: Us, one_way: bool, bpf: f64) -> Self {
        let record = ClosingRate {
            preset: QualityPreset::Balanced,
            bpf,
            ended: Instant::now(),
            ended_unix_ms: pb_mapper_core::time::unix_millis(),
        };
        Self::build(
            encoder,
            rtt_ms,
            one_way,
            QualityPreset::Balanced,
            RateModel::Nominal,
            Some(record),
        )
    }

    fn build(
        encoder: Encoder,
        rtt_ms: Us,
        one_way: bool,
        quality: QualityPreset,
        rate_model: RateModel,
        previous: Option<ClosingRate>,
    ) -> Self {
        let t0 = Instant::now();
        let limits = VideoLimits {
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
        };
        let mut qos = Qos::new(
            limits,
            VideoCodec::H264,
            quality,
            false,
            rate_model,
            previous,
            t0,
        );
        if one_way {
            qos.use_one_way();
        }
        let mut sim = Self {
            t0,
            now: 0,
            qos,
            credit: CreditLedger::new(t0, one_way),
            one_way,
            encoder,
            nominal: limits.max_fps,
            lagged_bps: 0.0,
            last_capture: None,
            idr_due: true,
            idr_every: None,
            next_idr: 0,
            handoff: VecDeque::new(),
            blocked: false,
            produced: ProducedSnapshot::default(),
            events: BinaryHeap::new(),
            serial: 0,
            wake: None,
            changes: Vec::new(),
            rate_bps: UNLIMITED_BPS,
            forward: rtt_ms * MS / 2,
            reverse: rtt_ms * MS / 2,
            stalls: Vec::new(),
            free: 0,
            last_forward: 0,
            last_reverse: 0,
            entered: VecDeque::new(),
            seconds: Vec::new(),
            current: Second {
                pace_min: u32::MAX,
                ..Second::default()
            },
        };
        sim.lagged_bps = f64::from(sim.qos.targets(t0).encoder_bps);
        // The first probe goes out before anything is captured.
        sim.probe();
        sim.at(ENCODE, Event::Capture);
        sim.at(QOS_TICK.as_micros() as Us, Event::Tick);
        sim
    }

    /// An IDR every `every`, besides the first.
    pub fn idr_every(&mut self, every: Us) {
        self.idr_every = Some(every);
        self.next_idr = every;
    }

    /// `change` at `at_s` seconds in.
    pub fn change(&mut self, at: Us, change: Change) {
        self.changes.push((at, change));
        self.changes.sort_by_key(|(at, _)| *at);
    }

    fn instant(&self, at: Us) -> Instant {
        self.t0 + Duration::from_micros(at)
    }

    fn at(&mut self, at: Us, event: Event) {
        self.serial += 1;
        self.events.push(Reverse((at, self.serial, event)));
    }

    /// Wake B-send at `at`, unless it is woken sooner.
    fn wake_at(&mut self, at: Us) {
        if self.wake.is_none_or(|wake| at < wake) {
            self.wake = Some(at);
            self.at(at, Event::Wake);
        }
    }

    /// Run until `until` seconds in.
    pub fn run(&mut self, until_s: u64) {
        let until = until_s * S;
        while let Some(Reverse((at, serial, event))) = self.events.pop() {
            if at > until {
                self.events.push(Reverse((at, serial, event)));
                break;
            }
            // A wake a sooner one replaced is stale: the sooner one is due.
            if event == Event::Wake && self.wake == Some(at) {
                self.wake = None;
            }
            self.apply_changes(at);
            self.close_seconds(at);
            self.now = at;
            self.handle(event);
            self.send();
        }
        self.close_seconds(until);
        self.now = until;
    }

    fn apply_changes(&mut self, at: Us) {
        while let Some(&(when, change)) = self.changes.first()
            && when <= at
        {
            self.changes.remove(0);
            match change {
                Change::Rate(bps) => {
                    self.rate_bps = if bps == 0 { UNLIMITED_BPS } else { bps };
                }
                Change::Delay { forward, reverse } => {
                    self.forward = forward * MS;
                    self.reverse = reverse * MS;
                }
                Change::Stall(length) => self.stalls.push((when, when + length)),
                Change::Picture(encoder) => self.encoder = encoder,
            }
        }
    }

    /// Close every whole second up to `at`.
    fn close_seconds(&mut self, at: Us) {
        while at >= (self.seconds.len() as Us + 1) * S {
            let end = self.instant((self.seconds.len() as Us + 1) * S);
            let view = self.qos.view(end);
            let receipts = self.qos.receipt_view(end);
            let mut second = self.current;
            second.pace = view.pace_fps;
            second.bpf = self.qos.bpf;
            second.expected_bps = view.expected_bps;
            second.encoder_bps = view.encoder_bps;
            second.rate_fps = view.rate_fps;
            second.rebase = view.rebase;
            second.cuts = receipts.cuts;
            if second.pace_min == u32::MAX {
                second.pace_min = view.pace_fps;
            }
            self.seconds.push(second);
            self.current = Second {
                pace_min: u32::MAX,
                ..Second::default()
            };
        }
    }

    /// Where a stall holds what would cross at `at`: its end.
    fn stalled(&self, at: Us) -> Us {
        self.stalls
            .iter()
            .find(|(from, to)| (*from..*to).contains(&at))
            .map_or(at, |(_, to)| *to)
    }

    /// `bytes` enter the bottleneck now; when they reach the viewer.
    fn forward_arrival(&mut self, bytes: u64) -> Us {
        let start = self.stalled(self.now.max(self.free));
        let transfer = bytes * 8 * S / self.rate_bps.max(1);
        self.free = start + transfer;
        let arrival = self
            .stalled(self.free + self.forward)
            .max(self.last_forward);
        self.last_forward = arrival;
        arrival
    }

    /// A message the viewer writes now; when it reaches the host.
    fn reverse_arrival(&mut self, sent: Us) -> Us {
        let arrival = self.stalled(sent + self.reverse).max(self.last_reverse);
        self.last_reverse = arrival;
        arrival
    }

    fn viewer_us(at: Us) -> u64 {
        at + VIEWER_OFFSET
    }

    fn handle(&mut self, event: Event) {
        let now = self.instant(self.now);
        match event {
            Event::Capture => self.capture(),
            Event::Encoded { idr, bytes } => self.handoff.push_back(Waiting {
                idr,
                bytes,
                ready: self.now,
            }),
            Event::UnitArrived { seq, bytes } => {
                let entered = self
                    .entered
                    .iter()
                    .position(|(unit, _)| *unit == seq)
                    .and_then(|index| self.entered.remove(index));
                if let Some((_, entered)) = entered {
                    let queue =
                        Duration::from_micros(self.now.saturating_sub(entered + self.forward));
                    self.current.queue_max = self.current.queue_max.max(queue);
                }
                self.current.received += 1;
                self.current.received_bits += bytes * 8;
                let at_us = Self::viewer_us(self.now);
                let back = self.reverse_arrival(self.now);
                self.at(back, Event::Receipt { seq, at_us });
            }
            Event::ProbeArrived { nonce } => {
                let at_us = Self::viewer_us(self.now);
                let back = self.reverse_arrival(self.now);
                self.at(back, Event::Echo { nonce, at_us });
            }
            Event::Receipt { seq, at_us } => {
                // As `Session::handle` does for `Received`.
                let at_us = self.one_way.then_some(at_us);
                let Ok(receipt) = self.credit.on_receipt(seq, now, at_us) else {
                    panic!("a receipt ahead of what was sent");
                };
                let blocked = std::mem::take(&mut self.blocked);
                self.qos.on_receipt(receipt, blocked, now);
            }
            Event::Echo { nonce, at_us } => {
                // As `Session::handle` does for `ProbeEcho`.
                let (base, sent, floor_gap) = (
                    self.qos.base_rtt(),
                    self.qos.probe_sent_at(nonce),
                    self.qos.floor_gap(),
                );
                self.credit.set_floor_gap(floor_gap);
                let forward = sent
                    .filter(|_| self.one_way)
                    .and_then(|sent| self.credit.probe_queue(at_us, sent, now));
                let ack_queue = base.map(|base| self.credit.ack_queue(base, now));
                self.qos.on_probe_echo(nonce, now, forward, ack_queue);
            }
            Event::Tick => {
                let view = self.credit.view();
                self.qos.tick(now, &view, self.produced);
                self.at(self.now + QOS_TICK.as_micros() as Us, Event::Tick);
            }
            Event::Wake => {}
        }
        // As `Session::update_qos` does after every change.
        let (generation, rebase) = self.qos.rebase_state();
        self.credit.set_rebase(generation, rebase);
    }

    /// Thread C: one picture a period, unless units wait for credit.
    fn capture(&mut self) {
        let now = self.instant(self.now);
        let targets = self.qos.targets(now);
        let pace = targets.pace_fps.max(1);
        self.current.pace_min = self.current.pace_min.min(pace);
        self.at(self.now + S / Us::from(pace), Event::Capture);
        if self.handoff.len() >= HANDOFF_WAITING {
            return;
        }
        if self.idr_every.is_some_and(|_| self.now >= self.next_idr) {
            self.idr_due = true;
            self.next_idr += self.idr_every.unwrap_or(S);
        }
        let idr = std::mem::take(&mut self.idr_due);
        let encoder_bps = f64::from(targets.encoder_bps);
        let gap = self
            .last_capture
            .replace(self.now)
            .map_or(S / Us::from(self.nominal), |last| self.now - last)
            .min(S);
        let per_frame = |bps: f64| bps / f64::from(self.nominal) / 8.0;
        let (bytes, idr_factor) = match self.encoder {
            Encoder::PerFrame { idr } => (per_frame(encoder_bps), idr),
            Encoder::Lagging { tau, idr } => {
                let weight = 1.0 - (-(gap as f64) / tau as f64).exp();
                self.lagged_bps += (encoder_bps - self.lagged_bps) * weight;
                (per_frame(self.lagged_bps), idr)
            }
            Encoder::PerSecond { idr } => (encoder_bps * gap as f64 / S as f64 / 8.0, idr),
            Encoder::Still => (300.0, 1.0),
            Encoder::Floor { bytes } => (per_frame(encoder_bps).max(bytes as f64), 4.0),
        };
        let bytes = if idr { bytes * idr_factor } else { bytes }.max(256.0) as u64;
        let still = matches!(self.encoder, Encoder::Still);
        let produced = &mut self.produced;
        produced.frames += 1;
        produced.unique += u64::from(!still || idr);
        if idr {
            produced.keyframes += 1;
        } else {
            produced.non_idr_frames += 1;
            produced.non_idr_bytes += bytes;
        }
        self.at(self.now + ENCODE, Event::Encoded { idr, bytes });
    }

    fn probe(&mut self) {
        let now = self.instant(self.now);
        let Some(nonce) = self.qos.probe_due(now) else {
            return;
        };
        let tainted = self.credit.idr_inflight().is_some();
        self.qos.probe_sent(nonce, now, tainted);
        let arrival = self.forward_arrival(PROBE_BYTES);
        self.at(arrival, Event::ProbeArrived { nonce });
    }

    /// B-send: a probe if due, then every unit credit admits, as
    /// `Session::admissible` judges them.
    fn send(&mut self) {
        let now = self.instant(self.now);
        self.probe();
        if let Some(due) = self.qos.probe_due_at(now) {
            let due = due.saturating_duration_since(self.t0).as_micros() as Us;
            if due > self.now {
                self.wake_at(due);
            }
        }
        while let Some(head) = self.handoff.front().copied() {
            if !head.idr
                && self.credit.idr_inflight().is_none()
                && self.now.saturating_sub(head.ready)
                    > crate::outgoing::OUTGOING_MAX_AGE.as_micros() as Us
            {
                self.handoff.clear();
                self.idr_due = true;
                break;
            }
            let (pace, expected, base, clamp) = (
                self.qos.pace_fps(now),
                self.qos.expected_bps(now),
                self.qos.base_rtt(),
                self.qos.clamp_rate(now),
            );
            let window = self
                .credit
                .window(VideoCodec::H264, pace, expected, base, clamp, now);
            self.current.clamped |= window.clamped;
            let len = head.bytes as usize;
            if !self.credit.fits(len, head.idr, &window) {
                if window.clamped && self.credit.idr_inflight().is_none() {
                    self.blocked = true;
                }
                self.wake_at(self.now + 20 * MS);
                return;
            }
            self.handoff.pop_front();
            let seq = self.credit.book(len, head.idr, now);
            self.entered.push_back((seq, self.now));
            let arrival = self.forward_arrival(head.bytes);
            self.at(
                arrival,
                Event::UnitArrived {
                    seq,
                    bytes: head.bytes,
                },
            );
        }
    }

    /// Every second of the run, one a line, for `--nocapture`.
    pub fn dump(&self, case: &str) {
        for (index, second) in self.seconds.iter().enumerate() {
            println!(
                "{case} s={} pace={} pace_min={} rate_fps={:?} bpf={:.0} expected={} encoder={} cuts={} rebase={:?} received={} received_kbit={} queue_max_ms={} clamped={}",
                index + 1,
                second.pace,
                second.pace_min,
                second.rate_fps,
                second.bpf,
                second.expected_bps,
                second.encoder_bps,
                second.cuts,
                second.rebase,
                second.received,
                second.received_bits / 1000,
                second.queue_max.as_millis(),
                second.clamped,
            );
        }
    }

    /// The seconds `from..to` of the run.
    pub fn span(&self, from: u64, to: u64) -> &[Second] {
        let to = (to as usize).min(self.seconds.len());
        self.seconds.get(from as usize..to).unwrap_or(&[])
    }

    /// The first second at or after `from` that `found` accepts.
    pub fn first(&self, from: u64, found: impl Fn(&Second) -> bool) -> Option<u64> {
        self.seconds
            .iter()
            .enumerate()
            .skip(from as usize)
            .find(|(_, second)| found(second))
            .map(|(index, _)| index as u64)
    }
}

/// Balanced's start, maximum and floor at 1080p, in bits per frame.
const START: f64 = 66_667.0;
const MAX: f64 = 133_333.0;
const FLOOR: f64 = 16_667.0;
/// Balanced's start at 60 FPS.
const START_BPS: u64 = 4_000_000;

fn typical() -> Encoder {
    Encoder::PerFrame { idr: 4.0 }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, count) = values.fold((0.0, 0), |(sum, count), value| (sum + value, count + 1));
    sum / f64::from(count.max(1))
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn a_clear_path_reaches_the_ceiling_and_the_presets_maximum() {
    let mut sim = Sim::new(typical(), 20, true);
    sim.run(20);
    sim.dump("clear");
    // The pace is at the ceiling once the startup cap's second is over ...
    assert!(sim.span(0, 20).iter().all(|second| second.pace == 60));
    // ... and bits per frame raise to the preset's maximum within seconds.
    let max = sim.first(0, |second| second.bpf >= MAX - 1.0);
    assert!(max.is_some_and(|max| max <= 8), "{max:?}");
    let last = sim.seconds.last().copied().unwrap_or_default();
    assert_eq!((last.cuts, last.rate_fps), (0, None));
    assert!(sim.seconds.iter().all(|second| second.queue_max < ms(10)));
}

#[test]
fn a_drop_to_3_mbit_keeps_60_fps() {
    for one_way in [true, false] {
        let mut sim = Sim::new(typical(), 20, one_way);
        sim.change(6 * S, Change::Rate(3_000_000));
        sim.run(20);
        sim.dump("3mbit");
        let limited = sim.span(7, 20);
        // Bits per frame take the drop from 8 Mbit/s: the pace stays at 60
        // and the viewer receives 60 FPS from 1 s in ...
        assert!(
            limited
                .iter()
                .all(|second| second.pace_min >= 55 && second.received >= 55),
            "{limited:?}"
        );
        // ... at about what the link carries, probing it now and then ...
        let expected = mean(limited.iter().map(|second| second.expected_bps as f64));
        assert!(
            (2_400_000.0..=3_000_000.0).contains(&expected),
            "{expected}"
        );
        // ... and the queue the drop left is gone within 2 s, as is each a
        // probe leaves.
        assert!(
            sim.span(8, 20)
                .iter()
                .all(|second| second.queue_max < ms(100)),
            "{:?}",
            sim.span(6, 20)
        );
    }
}

#[test]
fn a_drop_to_1_mbit_caps_the_pace_from_the_delivery_rate_then_recovers() {
    let mut sim = Sim::new(typical(), 20, true);
    sim.change(6 * S, Change::Rate(1_000_000));
    sim.change(14 * S, Change::Rate(0));
    sim.run(30);
    sim.dump("1mbit");
    // A drop from 8 Mbit/s leaves a second's queue; then bits per frame sit
    // at Balanced's floor, and the pace follows what the link delivers at
    // it, far above the preset's minimum of 10 ...
    let limited = sim.span(8, 14);
    assert!(
        limited.iter().all(|second| second.bpf < 1.1 * FLOOR
            && second.pace_min >= 45
            && second.received >= 45),
        "{limited:?}"
    );
    assert!(
        limited.iter().any(|second| second.rate_fps.is_some()),
        "{limited:?}"
    );
    // ... with a queue bounded near the cut's ...
    assert!(
        sim.span(9, 14)
            .iter()
            .all(|second| second.queue_max < ms(150)),
        "{limited:?}"
    );
    // ... and the pace, then the bitrate, back once the link is: 80% of the
    // start within 5 s.
    let back = sim.first(14, |second| second.expected_bps * 5 >= START_BPS * 4);
    assert!(
        back.is_some_and(|back| back < 14 + 5),
        "{:?}",
        sim.span(14, 30)
    );
    assert!(
        sim.span(16, 30)
            .iter()
            .all(|second| second.rate_fps.is_none())
    );
}

#[test]
fn a_lagging_encoder_is_not_cut_again_while_it_catches_up() {
    // An encoder whose output trails its bitrate by 0.7 s, on a link that
    // falls to 3 Mbit/s: a cut holds until what the link delivers falls, so
    // cuts never stack while the encoder lags.
    let mut sim = Sim::new(
        Encoder::Lagging {
            tau: 700 * MS,
            idr: 4.0,
        },
        20,
        true,
    );
    sim.change(6 * S, Change::Rate(3_000_000));
    sim.run(20);
    sim.dump("lagging");
    let first = sim.span(5, 6)[0].cuts;
    let cuts = sim.span(6, 8).last().map_or(0, |second| second.cuts) - first;
    assert!(cuts == 1, "{:?}", sim.span(5, 12));
    // A lagging encoder is no longer route: nothing rebases.
    assert!(sim.span(6, 20).iter().all(|second| second.rebase.is_none()));
    // What it settles at is near the link, not far below it, at 60 FPS.
    let settled = sim.span(10, 20);
    assert!(
        settled.iter().all(|second| second.pace_min >= 55),
        "{settled:?}"
    );
    let expected = mean(settled.iter().map(|second| second.expected_bps as f64));
    assert!(expected >= 2_000_000.0, "{settled:?}");
}

#[test]
fn a_per_second_encoder_sheds_bandwidth_through_bits_per_frame() {
    // An encoder that budgets per second, at the nominal model: a cut of the
    // pace sheds nothing, a cut of bits per frame sheds it all.
    let mut sim = Sim::new(Encoder::PerSecond { idr: 4.0 }, 20, true);
    sim.change(6 * S, Change::Rate(3_000_000));
    sim.run(20);
    sim.dump("per_second");
    let limited = sim.span(10, 20);
    let encoder = mean(limited.iter().map(|second| f64::from(second.encoder_bps)));
    assert!(encoder <= 3_000_000.0, "{limited:?}");
    let received = mean(limited.iter().map(|second| f64::from(second.received)));
    assert!(received >= 50.0, "{limited:?}");
    assert!(
        limited.iter().all(|second| second.queue_max < ms(150)),
        "{limited:?}"
    );
}

#[test]
fn an_isolated_stall_cuts_nothing() {
    for one_way in [true, false] {
        let mut sim = Sim::new(typical(), 20, one_way);
        for at in [8, 14, 20] {
            sim.change(at * S, Change::Stall(600 * MS));
        }
        sim.run(26);
        sim.dump("stall");
        let last = sim.seconds.last().copied().unwrap_or_default();
        assert_eq!(last.cuts, 0, "{:?}", sim.span(7, 26));
        // Bits per frame never fall for one, nor does the pace by half.
        let stalls = sim.span(7, 26);
        assert!(
            stalls
                .iter()
                .zip(stalls.iter().skip(1))
                .all(|(before, after)| after.bpf >= before.bpf && after.pace_min >= 30),
            "{stalls:?}"
        );
    }
}

#[test]
fn recurring_stalls_cut_twice_then_are_excused() {
    let mut sim = Sim::new(typical(), 20, true);
    for n in 0..10 {
        sim.change((8 + n) * S, Change::Stall(300 * MS));
    }
    sim.run(26);
    sim.dump("recurring");
    let before = sim.span(7, 8)[0];
    let stalls = sim.span(8, 18);
    let cuts = stalls.last().map_or(0, |second| second.cuts) - before.cuts;
    // Stalls that recur are congestion: the encoder bitrate falls ...
    assert!((1..=2).contains(&cuts), "{stalls:?}");
    assert!(
        stalls
            .iter()
            .any(|second| second.encoder_bps < before.encoder_bps),
        "{stalls:?}"
    );
    // ... but cuts that leave them as they were stop, and the rate comes back
    // once they end.
    let last = sim.seconds.last().copied().unwrap_or_default();
    assert!(
        last.expected_bps >= before.expected_bps,
        "{:?}",
        sim.span(7, 26)
    );
}

#[test]
fn a_longer_route_rebases_after_at_most_two_cuts() {
    for one_way in [true, false] {
        let mut sim = Sim::new(typical(), 20, one_way);
        // 60 ms more each way, with the bits per frame at their maximum.
        sim.change(
            12 * S,
            Change::Delay {
                forward: 70,
                reverse: 70,
            },
        );
        sim.run(40);
        sim.dump("route");
        let before = sim.span(11, 12)[0];
        let after = sim.span(12, 40);
        let cuts = after.last().map_or(0, |second| second.cuts) - before.cuts;
        assert!(cuts <= 2, "{after:?}");
        // The probes read the longer route against the rebase, so the pace
        // never falls, and the credit window covers it, so the viewer
        // receives the pace from 2 s on ...
        assert!(
            after.iter().all(|second| second.pace_min >= 55),
            "{after:?}"
        );
        assert!(
            sim.span(14, 40).iter().all(|second| second.received >= 55),
            "{after:?}"
        );
        // ... and the bitrate is back within 15 s.
        let back = sim.first(13, |second| second.expected_bps >= before.expected_bps);
        assert!(back.is_some_and(|back| back <= 12 + 15), "{after:?}");
        // The cuts end in a rebase, on the queue toward the viewer that
        // arrival times read as on the round trip.
        assert!(
            after.iter().any(|second| second.rebase.is_some()),
            "{after:?}"
        );
    }
}

#[test]
fn a_queue_on_the_way_back_alone_cuts_nothing_with_arrival_times() {
    let mut sim = Sim::new(typical(), 20, true);
    sim.change(
        8 * S,
        Change::Delay {
            forward: 10,
            reverse: 310,
        },
    );
    sim.run(24);
    sim.dump("reverse");
    let before = sim.span(7, 8)[0];
    let after = sim.span(9, 24);
    assert!(
        after
            .iter()
            .all(|second| second.cuts == before.cuts && second.pace_min >= 55),
        "{after:?}"
    );
}

#[test]
fn idrs_four_times_a_frame_near_capacity_never_run_the_rate_down() {
    // IDRs every 2 s on a link 10% above Balanced's start: each IDR's units,
    // and the units written behind it, decide nothing.
    let mut sim = Sim::new(typical(), 20, true);
    sim.idr_every(2 * S);
    sim.change(0, Change::Rate(4_400_000));
    sim.run(30);
    sim.dump("idr");
    let settled = sim.span(10, 30);
    assert!(
        settled.iter().all(|second| second.pace_min >= 55),
        "{settled:?}"
    );
    let expected = mean(settled.iter().map(|second| second.expected_bps as f64));
    assert!(expected >= 2_500_000.0, "{settled:?}");
    assert!(
        settled.iter().all(|second| second.queue_max < ms(250)),
        "{settled:?}"
    );
}

#[test]
fn a_still_picture_never_raises() {
    let mut sim = Sim::new(Encoder::Still, 20, true);
    sim.run(20);
    sim.dump("still");
    // The first probe may only lower bits per frame; nothing raises them.
    assert!(
        sim.seconds.iter().all(|second| second.bpf <= START),
        "{:?}",
        sim.seconds
    );
}

#[test]
fn quality_cuts_to_half_its_start_then_the_pace_then_its_floor() {
    let mut sim = Sim::with(
        typical(),
        20,
        true,
        QualityPreset::Quality,
        RateModel::Nominal,
    );
    // Quality paces at 30 at most: its cut floor of 100 000 bits carries
    // 3 Mbit/s there, and 0.8 at its minimum of 8 FPS.
    sim.change(6 * S, Change::Rate(2_000_000));
    sim.change(16 * S, Change::Rate(500_000));
    sim.run(26);
    sim.dump("quality");
    let fps_cut = sim.span(9, 16);
    assert!(
        fps_cut
            .iter()
            .all(|second| (second.bpf - 100_000.0).abs() < 1.0
                && second.rate_fps.is_some_and(|fps| (8..30).contains(&fps))),
        "{fps_cut:?}"
    );
    let floor = sim.span(19, 26);
    assert!(
        floor.iter().all(|second| second.rate_fps == Some(8)
            && second.bpf < 100_000.0
            && second.bpf >= 50_000.0 - 1.0),
        "{floor:?}"
    );
    assert!(
        mean(floor.iter().map(|second| second.bpf)) < 70_000.0,
        "{floor:?}"
    );
}

#[test]
fn an_encoder_at_its_content_floor_sheds_through_the_pace_and_recovers() {
    // Frames never below 3750 bytes, 1.8 × Balanced's floor: lower bits per
    // frame shed nothing on a 1.2 Mbit/s link, so the pace carries what the
    // frames the host measured cost.
    let mut sim = Sim::new(Encoder::Floor { bytes: 3750 }, 20, true);
    sim.change(6 * S, Change::Rate(1_200_000));
    sim.change(20 * S, Change::Rate(0));
    sim.run(40);
    sim.dump("floor");
    let limited = sim.span(10, 20);
    assert!(
        limited
            .iter()
            .all(|second| second.queue_max < ms(100) && second.rate_fps.is_some()),
        "{limited:?}"
    );
    let received = mean(limited.iter().map(|second| f64::from(second.received)));
    assert!(received >= 33.0, "{limited:?}");
    // Once the link is clear, raises lift the cap and then bits per frame,
    // though the encoder sends more than it is asked for.
    let back = sim.first(20, |second| {
        second.rate_fps.is_none() && second.expected_bps >= START_BPS
    });
    assert!(
        back.is_some_and(|back| back <= 20 + 6),
        "{:?}",
        sim.span(20, 40)
    );
    let last = sim.seconds.last().copied().unwrap_or_default();
    assert!(last.bpf >= MAX - 1.0, "{last:?}");
}

#[test]
fn a_light_picture_lifts_the_pace_cap_once_the_link_clears() {
    // The picture turns still during a 500 kbit/s limit, which capped the
    // pace: a light picture never raises bits per frame, but lifts the cap.
    let mut sim = Sim::new(typical(), 20, true);
    sim.change(6 * S, Change::Rate(500_000));
    sim.change(10 * S, Change::Picture(Encoder::Still));
    sim.change(14 * S, Change::Rate(0));
    sim.run(30);
    sim.dump("light_limit");
    assert!(
        sim.span(7, 10)
            .iter()
            .any(|second| second.rate_fps.is_some()),
        "{:?}",
        sim.span(6, 14)
    );
    let clear = sim.span(15, 30);
    assert!(
        clear
            .iter()
            .all(|second| second.pace_min >= 55 && second.rate_fps.is_none()),
        "{clear:?}"
    );
    assert!(clear.iter().all(|second| second.bpf <= FLOOR + 1.0));
}

#[test]
fn a_light_picture_behind_a_queue_it_did_not_cause_keeps_its_pace() {
    for one_way in [true, false] {
        // A still picture, 144 kbit/s, behind 60 ms more toward the viewer:
        // the busy rate is its own. Cuts sized by what its frames cost lower
        // the pace a step each, the queue stays, and the baseline moves.
        let mut sim = Sim::new(Encoder::Still, 20, one_way);
        sim.change(
            8 * S,
            Change::Delay {
                forward: 70,
                reverse: 10,
            },
        );
        sim.change(
            20 * S,
            Change::Delay {
                forward: 10,
                reverse: 10,
            },
        );
        sim.run(40);
        sim.dump(if one_way {
            "light_queue"
        } else {
            "light_route"
        });
        let queued = sim.span(8, 20);
        assert!(
            queued.iter().all(|second| second.pace_min >= 40),
            "{queued:?}"
        );
        assert!(queued.iter().any(|second| second.rebase.is_some()));
        assert!(
            sim.span(12, 40)
                .iter()
                .all(|second| second.pace_min >= 55 && second.cuts <= 2),
            "{:?}",
            sim.span(8, 40)
        );
    }
}

#[test]
fn a_start_a_closed_session_lowered_climbs_back_on_a_clear_link() {
    for (case, encoder) in [
        ("remembered", typical()),
        ("remembered_still", Encoder::Still),
    ] {
        let mut sim = Sim::after_closing(encoder, 20, true, FLOOR);
        sim.run(20);
        sim.dump(case);
        let first = sim.span(0, 1)[0];
        assert!(first.bpf < 1.5 * FLOOR, "{first:?}");
        // Back at the preset's start within 5 s, a still picture's too, which
        // climbs no further: from there a light picture raises as any does.
        let back = sim.first(0, |second| second.bpf >= START - 1.0);
        assert!(back.is_some_and(|back| back < 5), "{:?}", sim.span(0, 8));
        if matches!(encoder, Encoder::Still) {
            assert!(sim.seconds.iter().all(|second| second.bpf <= START + 1.0));
        }
        assert!(sim.seconds.iter().all(|second| second.cuts == 0));
    }
}

#[test]
fn a_start_a_closed_session_lowered_on_its_slow_link_climbs_until_its_first_cut() {
    let run = |remembered: bool| {
        let mut sim = if remembered {
            Sim::after_closing(typical(), 40, true, FLOOR)
        } else {
            Sim::new(typical(), 40, true)
        };
        sim.change(0, Change::Rate(1_000_000));
        sim.run(20);
        sim.dump(if remembered {
            "slow_remembered"
        } else {
            "slow_cold"
        });
        sim
    };
    let (cold, again) = (run(false), run(true));
    // Started at the floor its last session ended at, a reconnect over the
    // same 1 Mbit/s link queues little more than its IDR's own transfer in
    // its first two seconds, where a cold start's IDR and startup second
    // queued several times as long ...
    let queued = |sim: &Sim| {
        sim.span(0, 2)
            .iter()
            .map(|second| second.queue_max)
            .max()
            .unwrap_or_default()
    };
    assert!(queued(&again) < ms(100), "{:?}", again.span(0, 4));
    assert!(queued(&cold) > ms(250), "{:?}", cold.span(0, 4));
    // ... and its first cut ends the climb back to the preset's start: from
    // there it runs as the cold start does, at the floor.
    assert!(!again.qos.restoring);
    assert!(
        again
            .span(4, 20)
            .iter()
            .all(|second| second.bpf < 1.2 * FLOOR),
        "{:?}",
        again.span(4, 20)
    );
}
