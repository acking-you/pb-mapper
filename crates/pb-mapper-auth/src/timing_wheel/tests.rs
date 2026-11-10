use super::*;

/// A wheel plus the tick each scheduled timer actually fired on.
struct Harness {
    wheel: TimingWheel,
    fired: Arc<Mutex<Vec<(u32, u64)>>>,
    ticks: Arc<AtomicU64>,
}

impl Harness {
    fn new(max_delay: u64, radix: u64) -> Self {
        Self {
            wheel: TimingWheel::new(max_delay, radix),
            fired: Arc::new(Mutex::new(Vec::new())),
            ticks: Arc::new(AtomicU64::new(0)),
        }
    }

    fn timer(&self, id: u32) -> Arc<Timer> {
        let fired = self.fired.clone();
        let ticks = self.ticks.clone();
        Timer::new(move || {
            fired.lock().push((id, ticks.load(Ordering::Acquire)));
        })
    }

    fn schedule(&mut self, id: u32, delay: u64) {
        let timer = self.timer(id);
        self.wheel.schedule(delay, timer);
    }

    fn tick(&mut self) {
        self.ticks.fetch_add(1, Ordering::AcqRel);
        self.wheel.tick();
    }

    fn fired_at(&self, id: u32) -> Option<u64> {
        self.fired
            .lock()
            .iter()
            .find(|(fired, _)| *fired == id)
            .map(|(_, at)| *at)
    }
}

/// Every delay, from every starting offset, must fire on exactly the tick it
/// asked for. This is the wheel's whole contract, and a radix decomposition is
/// easy to get wrong by one bucket, so it is checked exhaustively rather than
/// sampled.
#[test]
fn every_delay_fires_on_its_exact_tick() {
    let radix = 4;
    let max_delay = radix * radix * radix;
    for offset in 0..2 * radix * radix {
        let mut harness = Harness::new(max_delay, radix);
        for _ in 0..offset {
            harness.tick();
        }
        for delay in 1..=max_delay {
            harness.schedule(delay as u32, delay);
        }
        for _ in 0..max_delay {
            harness.tick();
        }
        for delay in 1..=max_delay {
            assert_eq!(
                harness.fired_at(delay as u32),
                Some(offset + delay),
                "radix {radix}, offset {offset}, delay {delay}"
            );
        }
    }
}

/// The same contract at the shape the wheel actually runs with.
#[test]
fn every_short_delay_fires_on_its_exact_tick_at_radix_64() {
    let radix = 64;
    let mut harness = Harness::new(radix * radix * radix * radix, radix);
    for _ in 0..100 {
        harness.tick();
    }
    let delays = (1..=200).chain([radix - 1, radix, radix + 1, radix * radix, 4095, 4096]);
    for delay in delays.clone() {
        harness.schedule(delay as u32, delay);
    }
    for _ in 0..5000 {
        harness.tick();
    }
    for delay in delays {
        assert_eq!(
            harness.fired_at(delay as u32),
            Some(100 + delay),
            "delay {delay}"
        );
    }
}

#[test]
fn level_count_covers_the_requested_delay() {
    assert_eq!(TimingWheel::new(64, 64).max_delay(), 64);
    assert_eq!(TimingWheel::new(65, 64).max_delay(), 4096);
    assert_eq!(TimingWheel::new(4096, 64).max_delay(), 4096);
    assert_eq!(TimingWheel::new(4097, 64).max_delay(), 262_144);
}

#[test]
fn a_delay_the_wheel_cannot_place_fires_at_once() {
    let mut harness = Harness::new(64, 64);
    harness.schedule(1, 65);
    assert_eq!(harness.fired_at(1), Some(0));
    harness.schedule(2, 0);
    assert_eq!(harness.fired_at(2), Some(0));
}

#[test]
fn rescheduling_the_same_timer_defers_it_to_the_later_route() {
    let mut harness = Harness::new(4096, 64);
    let timer = harness.timer(1);
    harness.wheel.schedule(5, timer.clone());
    harness.wheel.schedule(20, timer);

    for _ in 0..5 {
        harness.tick();
    }
    assert_eq!(
        harness.fired_at(1),
        None,
        "the earlier route must not fire the timer"
    );
    for _ in 5..20 {
        harness.tick();
    }
    assert_eq!(harness.fired_at(1), Some(20));
}

#[test]
fn firing_early_makes_the_scheduled_route_inert() {
    let mut harness = Harness::new(4096, 64);
    let timer = harness.timer(1);
    harness.wheel.schedule(10, timer.clone());

    timer.fire();
    assert_eq!(harness.fired_at(1), Some(0));
    for _ in 0..10 {
        harness.tick();
    }
    assert_eq!(harness.fired.lock().len(), 1);
}

#[test]
fn dropping_the_wheel_fires_everything_it_holds() {
    let mut harness = Harness::new(262_144, 64);
    harness.schedule(1, 5);
    harness.schedule(2, 200_000);

    let fired = harness.fired.clone();
    drop(harness);
    let ids = fired
        .lock()
        .iter()
        .map(|(id, _)| *id)
        .collect::<HashSet<_>>();
    assert_eq!(ids, HashSet::from([1, 2]));
}
