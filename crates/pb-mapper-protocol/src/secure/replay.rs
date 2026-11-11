//! Durable, process-local duplicate admission for protocol-v2 first flights.
//!
//! ```text
//! key id + salt -> SHA-256 fingerprint -> current Bloom window --+
//!                                      -> previous Bloom window -+-> seen: Replayed
//!                                      -> pending (being logged) +
//! key id ---------> per-credential first-flight count, reservations included
//!
//! connection: check and reserve, under one lock -> queue -> writer thread:
//!   every reservation queued meanwhile, one append and one sync
//!   -> Bloom insert, or the reservation given back -> the answers
//! ```
//!
//! A first flight is checked and reserved under one lock, so of two with one
//! fingerprint the second is `Replayed` even while the first is being logged,
//! and it is answered `Fresh` only once its record is synced. One writer
//! appends whatever was reserved while it synced the batch before, so a burst
//! of admissions costs a few syncs, not one each. This Bloom filter protects
//! all connection types from immediate duplicates; administrator mutations
//! additionally use the exact durable replay set in `auth`.
//!
//! Each generation lasts `2 *` the accepted clock-skew so a salt inserted at the
//! end of a window with a max-future timestamp cannot be replayed after rotation.
//! Per-credential counts stop one tenant from filling the shared filter with
//! unique salts before the request payload is decoded.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use parking_lot::{Mutex, MutexGuard};
use pb_mapper_auth::KeyId;
use pb_mapper_core::config;
use pb_mapper_core::durable_file::{self, NewFileMode};
use ring::digest::{SHA256, SHA256_OUTPUT_LEN, digest};
use tokio::sync::oneshot;

use super::{CONNECTION_SALT_LEN, pack, unix_seconds};

/// A first-flight fingerprint: SHA-256 of the key id and connection salt.
const FINGERPRINT_LEN: usize = SHA256_OUTPUT_LEN;
/// One admission in the log: its fingerprint, then the admission time.
pub(super) const REPLAY_RECORD_LEN: usize = FINGERPRINT_LEN + size_of::<u64>();
pub(super) const REPLAY_COMPACT_INTERVAL_SECONDS: u64 = 60;
/// The most admissions one append carries; a longer queue takes more.
const MAX_BATCH: usize = 4096;

fn first_flight_budget(window_seconds: u64) -> u32 {
    // A rate past `u32` takes the default, as it did when this read the
    // variable as a `u32` itself.
    let streams_per_sec = u32::try_from(config::new_streams_per_second().get())
        .unwrap_or(config::DEFAULT_NEW_STREAMS_PER_SECOND)
        .min(1_000_000);
    let window = u32::try_from(window_seconds).unwrap_or(u32::MAX);
    streams_per_sec
        .saturating_mul(2)
        .saturating_mul(window)
        .saturating_mul(2)
        .max(8_192)
}

fn bloom_insert_capacity(bytes: usize) -> u32 {
    let bits = (bytes as u64).saturating_mul(8);
    u32::try_from(bits / 16).unwrap_or(u32::MAX).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FirstFlightAdmit {
    Fresh,
    Replayed,
    Limited,
    Unavailable,
}

#[expect(
    clippy::expect_used,
    reason = "a SHA-256 digest is always SHA256_OUTPUT_LEN bytes"
)]
pub(super) fn replay_fingerprint(
    key_id: KeyId,
    salt: &[u8; CONNECTION_SALT_LEN],
) -> [u8; FINGERPRINT_LEN] {
    let input: [u8; size_of::<u64>() + CONNECTION_SALT_LEN] = pack(&[&key_id.to_be_bytes(), salt]);
    digest(&SHA256, &input)
        .as_ref()
        .try_into()
        .expect("a SHA-256 digest is SHA256_OUTPUT_LEN bytes")
}

/// One admission in the log.
struct ReplayRecord {
    fingerprint: [u8; FINGERPRINT_LEN],
    timestamp: u64,
}

impl ReplayRecord {
    /// The record on disk: the fingerprint, then the admission time.
    fn to_bytes(&self) -> [u8; REPLAY_RECORD_LEN] {
        pack(&[&self.fingerprint, &self.timestamp.to_be_bytes()])
    }

    fn from_bytes(bytes: [u8; REPLAY_RECORD_LEN]) -> Self {
        let [fingerprint @ .., t0, t1, t2, t3, t4, t5, t6, t7] = bytes;
        Self {
            fingerprint,
            timestamp: u64::from_be_bytes([t0, t1, t2, t3, t4, t5, t6, t7]),
        }
    }
}

pub(super) struct RotatingBloom {
    current: Vec<u8>,
    previous: Vec<u8>,
    pub(super) current_started_at: u64,
    window_seconds: u64,
}

impl RotatingBloom {
    pub(super) fn new(bytes: usize, window_seconds: u64) -> Self {
        Self {
            current: vec![0; bytes],
            previous: vec![0; bytes],
            current_started_at: unix_seconds(),
            window_seconds,
        }
    }

    pub(super) fn contains(&mut self, fingerprint: &[u8; FINGERPRINT_LEN], now: u64) -> bool {
        self.rotate(now);
        bloom_contains(&self.current, fingerprint) || bloom_contains(&self.previous, fingerprint)
    }

    pub(super) fn insert(&mut self, fingerprint: &[u8; FINGERPRINT_LEN], now: u64) {
        self.rotate(now);
        bloom_insert(&mut self.current, fingerprint);
    }

    fn rotate(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.current_started_at);
        if elapsed < self.window_seconds {
            return;
        }
        if elapsed >= self.window_seconds.saturating_mul(2) {
            self.current.fill(0);
            self.previous.fill(0);
        } else {
            std::mem::swap(&mut self.current, &mut self.previous);
            self.current.fill(0);
        }
        self.current_started_at = now;
    }
}

/// An admission's answer: given at once, or by the writer once the admission
/// is logged.
pub(super) enum Admission {
    Answered(FirstFlightAdmit),
    Logging(oneshot::Receiver<FirstFlightAdmit>),
}

impl Admission {
    pub(super) async fn answer(self) -> FirstFlightAdmit {
        match self {
            Self::Answered(answer) => answer,
            // A writer that stopped without answering vouched for nothing.
            Self::Logging(answer) => answer.await.unwrap_or(FirstFlightAdmit::Unavailable),
        }
    }

    /// [`Self::answer`], blocking this thread: for tests outside a runtime.
    #[cfg(test)]
    pub(super) fn wait(self) -> FirstFlightAdmit {
        match self {
            Self::Answered(answer) => answer,
            Self::Logging(answer) => answer
                .blocking_recv()
                .unwrap_or(FirstFlightAdmit::Unavailable),
        }
    }
}

pub(super) struct ReplayGuard {
    admissions: Arc<Mutex<Admissions>>,
    /// The log's writer; `None` when nothing is logged.
    writer: Option<Writer>,
}

/// The way to the log's writer, and the thread it runs on.
struct Writer {
    queue: mpsc::Sender<Queued>,
    /// Taken by whoever closes the guard, to wait for the thread's end;
    /// `None` too when the thread could not be started.
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// What the writer is sent.
enum Queued {
    Reservation(Reservation),
    /// The guard is closed: nothing follows.
    Close,
}

/// What admitting checks and reserves, all under one lock.
struct Admissions {
    bloom: RotatingBloom,
    /// The fingerprints reserved and not yet logged. They are checked with
    /// the filter, so a copy is `Replayed` while the first is being logged,
    /// and kept apart from it, because a Bloom insert cannot be undone when
    /// the append fails.
    pending: HashSet<[u8; FINGERPRINT_LEN]>,
    /// First flights per credential since `counts_started_at`, reservations
    /// included.
    counts: HashMap<KeyId, u32>,
    counts_started_at: u64,
    window_seconds: u64,
    max_per_key: u32,
    /// First flights since `total_started_at`, reservations included.
    total: u32,
    total_started_at: u64,
    max_total: u32,
    /// The log may hold what was never admitted, or lack what was: every
    /// admission is refused from now on.
    log_failed: bool,
    /// The guard is closed: every admission is refused from now on.
    closed: bool,
}

/// An admission on its way to the log, and what it took from the budgets.
struct Reservation {
    record: ReplayRecord,
    /// The credential whose count it took and when that count began; `None`
    /// for a claim, which counts against no credential.
    count: Option<(KeyId, u64)>,
    /// When the total it took began.
    total_started_at: u64,
    answer: oneshot::Sender<FirstFlightAdmit>,
}

impl Admissions {
    /// Whether `fingerprint` was admitted or claimed, or is being.
    fn seen(&mut self, fingerprint: &[u8; FINGERPRINT_LEN], now: u64) -> bool {
        self.bloom.contains(fingerprint, now) || self.pending.contains(fingerprint)
    }

    fn rotate_counts(&mut self, now: u64) {
        if now.saturating_sub(self.counts_started_at) < self.window_seconds {
            return;
        }
        self.counts.clear();
        self.counts_started_at = now;
    }

    fn reset_total_if_rotated(&mut self) {
        if self.bloom.current_started_at != self.total_started_at {
            self.total = 0;
            self.total_started_at = self.bloom.current_started_at;
        }
    }

    /// Gives back what an admission that was not logged took: from the
    /// windows it took it from, if they are still the current ones.
    fn release(&mut self, count: Option<(KeyId, u64)>, total_started_at: u64) {
        if let Some((key_id, started_at)) = count
            && started_at == self.counts_started_at
            && let Some(count) = self.counts.get_mut(&key_id)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.counts.remove(&key_id);
            }
        }
        if total_started_at == self.total_started_at {
            self.total = self.total.saturating_sub(1);
        }
    }
}

impl ReplayGuard {
    pub(super) fn open(log_path: Option<PathBuf>, bytes: usize, window_seconds: u64) -> Self {
        let now = unix_seconds();
        let configured = first_flight_budget(window_seconds);
        let mut admissions = Admissions {
            bloom: RotatingBloom::new(bytes, window_seconds),
            pending: HashSet::new(),
            counts: HashMap::new(),
            counts_started_at: now,
            window_seconds,
            max_per_key: configured,
            total: 0,
            total_started_at: now,
            max_total: configured.min(bloom_insert_capacity(bytes)),
            log_failed: false,
            closed: false,
        };
        let Some(path) = log_path else {
            return Self {
                admissions: Arc::new(Mutex::new(admissions)),
                writer: None,
            };
        };
        load_persisted(&path, &mut admissions);
        let admissions = Arc::new(Mutex::new(admissions));
        let (sender, queue) = mpsc::channel();
        let log = LogWriter {
            path: path.clone(),
            admissions: admissions.clone(),
            last_compact_at: unix_seconds(),
            append_failing: false,
            batch: Vec::new(),
            bytes: Vec::new(),
        };
        // The writer logs where the code that opened the log does.
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let started = std::thread::Builder::new()
            .name("pb-mapper-replay-log".into())
            .spawn(move || tracing::dispatcher::with_default(&dispatch, || log.run(queue)));
        let thread = match started {
            Ok(thread) => Some(thread),
            Err(error) => {
                admissions.lock().log_failed = true;
                log_unavailable(&path, "start its writer", &error, None);
                None
            }
        };
        Self {
            admissions,
            writer: Some(Writer {
                queue: sender,
                thread: Mutex::new(thread),
            }),
        }
    }

    /// Refuses every admission from now on, and returns once the log's writer
    /// has logged what was queued and ended. Nothing of this guard touches
    /// the log after, so whoever opens it next has it alone.
    pub(super) fn close(&self) {
        self.admissions.lock().closed = true;
        let Some(writer) = &self.writer else {
            return;
        };
        // Behind every reservation: each is sent under the lock just held.
        let _ = writer.queue.send(Queued::Close);
        if let Some(thread) = writer.thread.lock().take() {
            // A writer that panicked has ended too.
            let _ = thread.join();
        }
    }

    #[cfg(test)]
    pub(super) fn with_max_per_key(self, max_per_key: u32) -> Self {
        self.admissions.lock().max_per_key = max_per_key;
        self
    }

    #[cfg(test)]
    pub(super) fn with_max_total(self, max_total: u32) -> Self {
        self.admissions.lock().max_total = max_total;
        self
    }

    pub(super) fn admit(
        &self,
        key_id: KeyId,
        fingerprint: &[u8; FINGERPRINT_LEN],
        now: u64,
    ) -> Admission {
        let mut admissions = self.admissions.lock();
        admissions.rotate_counts(now);
        if admissions.seen(fingerprint, now) {
            return Admission::Answered(FirstFlightAdmit::Replayed);
        }
        admissions.reset_total_if_rotated();
        if admissions.counts.get(&key_id).copied().unwrap_or(0) >= admissions.max_per_key
            || admissions.total >= admissions.max_total
        {
            return Admission::Answered(FirstFlightAdmit::Limited);
        }
        self.reserve(admissions, Some(key_id), fingerprint, now)
    }

    pub(super) fn claim(&self, fingerprint: &[u8; FINGERPRINT_LEN], now: u64) -> Admission {
        let mut admissions = self.admissions.lock();
        if admissions.seen(fingerprint, now) {
            return Admission::Answered(FirstFlightAdmit::Replayed);
        }
        admissions.reset_total_if_rotated();
        if admissions.total >= admissions.max_total {
            return Admission::Answered(FirstFlightAdmit::Unavailable);
        }
        self.reserve(admissions, None, fingerprint, now)
    }

    /// Takes `fingerprint`'s place in the budgets, and in the log through the
    /// writer, which answers once it is there.
    fn reserve(
        &self,
        mut admissions: MutexGuard<'_, Admissions>,
        key_id: Option<KeyId>,
        fingerprint: &[u8; FINGERPRINT_LEN],
        now: u64,
    ) -> Admission {
        if admissions.log_failed || admissions.closed {
            return Admission::Answered(FirstFlightAdmit::Unavailable);
        }
        if let Some(key_id) = key_id {
            *admissions.counts.entry(key_id).or_insert(0) += 1;
        }
        admissions.total = admissions.total.saturating_add(1);
        let Some(writer) = &self.writer else {
            admissions.bloom.insert(fingerprint, now);
            return Admission::Answered(FirstFlightAdmit::Fresh);
        };
        admissions.pending.insert(*fingerprint);
        let (answer, answered) = oneshot::channel();
        let reservation = Reservation {
            record: ReplayRecord {
                fingerprint: *fingerprint,
                timestamp: now,
            },
            count: key_id.map(|key_id| (key_id, admissions.counts_started_at)),
            total_started_at: admissions.total_started_at,
            answer,
        };
        if let Err(mpsc::SendError(Queued::Reservation(reservation))) =
            writer.queue.send(Queued::Reservation(reservation))
        {
            // Only a panic stops the writer of a guard that is not closed.
            admissions.pending.remove(fingerprint);
            admissions.release(reservation.count, reservation.total_started_at);
            admissions.log_failed = true;
            tracing::error!(
                event = "replay_log_unavailable",
                step = "hand a record to its writer",
                "first-flight replay log failed; protocol-v2 connections are refused while it is unavailable"
            );
            return Admission::Answered(FirstFlightAdmit::Unavailable);
        }
        Admission::Logging(answered)
    }
}

/// A guard let go without being closed still ends its writer before
/// whoever dropped it can open the log again.
impl Drop for ReplayGuard {
    fn drop(&mut self) {
        self.close();
    }
}

/// Loads the log at `path` into `admissions`, keeping the records still
/// inside the window, and rewrites it to hold just those. A log that cannot
/// be read or rewritten fails closed.
fn load_persisted(path: &Path, admissions: &mut Admissions) {
    if !path.exists() {
        return;
    }
    let now = unix_seconds();
    let window_seconds = admissions.window_seconds;
    let records =
        match read_replay_log(path).and_then(|log| after_a_crash(path, log, now, window_seconds)) {
            Ok(records) => records,
            Err(error) => {
                admissions.log_failed = true;
                log_unavailable(path, "read", &error, None);
                return;
            }
        };
    let mut live = Vec::new();
    for record in records {
        if now.saturating_sub(record.timestamp) > window_seconds {
            continue;
        }
        admissions
            .bloom
            .insert(&record.fingerprint, record.timestamp);
        live.push(record);
    }
    if let Some(oldest) = live.iter().map(|record| record.timestamp).min() {
        admissions.bloom.current_started_at = oldest;
    }
    admissions.total = u32::try_from(live.len())
        .unwrap_or(u32::MAX)
        .min(admissions.max_total);
    admissions.total_started_at = admissions.bloom.current_started_at;
    if let Err(error) = rewrite_live(path, &live) {
        admissions.log_failed = true;
        log_unavailable(path, "rewrite", &error, None);
    }
}

/// The records of a log read at start, less the record a crash cut off,
/// if one did.
///
/// A crash in the middle of an append leaves that append's last record cut
/// off as the last thing in the log, and nothing else: each append adds whole
/// records at the end, and one that fails is rolled back or stops every
/// append after it. Its admission was never acknowledged, which waits for the
/// append to be synced, so dropping it loses nothing replay protection needs;
/// the whole records before it in the same append were never acknowledged
/// either, and are kept, which only refuses what no client was told it may
/// use. A log cut off anywhere else reads out of step from there on, and a
/// file that is no replay log reads as records no relay wrote: either way
/// some record, or the cut-off one at the earliest it can be, names a time
/// past the replay window ahead of this relay's clock, and the log is
/// refused, as one is that a relay wrote before its clock was set back by
/// more than the window.
fn after_a_crash(
    path: &Path,
    ReplayLog { records, cut_off }: ReplayLog,
    now: u64,
    window_seconds: u64,
) -> std::io::Result<Vec<ReplayRecord>> {
    if cut_off.is_empty() {
        return Ok(records);
    }
    // The earliest time the cut-off record can have had.
    let mut completed = [0_u8; REPLAY_RECORD_LEN];
    for (byte, kept) in completed.iter_mut().zip(&cut_off) {
        *byte = *kept;
    }
    let latest = now.saturating_add(window_seconds);
    if records
        .iter()
        .chain([&ReplayRecord::from_bytes(completed)])
        .any(|record| record.timestamp > latest)
    {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "the log ends in a cut-off record and names times past the replay window \
             ahead of this relay's clock: it is damaged, or no first-flight replay log",
        ));
    }
    tracing::warn!(
        event = "replay_log_cut_off_record_dropped",
        path = %path.display(),
        bytes = cut_off.len(),
        "dropped the first-flight replay log's last record, cut off by a crash in the middle of its append"
    );
    Ok(records)
}

fn rewrite_live(path: &Path, live: &[ReplayRecord]) -> std::io::Result<()> {
    let records = live.iter().map(ReplayRecord::to_bytes).collect::<Vec<_>>();
    durable_file::replace_contents(path, records.as_flattened(), NewFileMode::Default)
        .map_err(std::io::Error::from)
}

/// The log's one writer, on a thread of its own: it appends every
/// reservation queued while it wrote the last batch in one write and one
/// sync, and compacts the log between batches.
struct LogWriter {
    path: PathBuf,
    admissions: Arc<Mutex<Admissions>>,
    last_compact_at: u64,
    /// An append has failed, and been logged, since the last one that
    /// succeeded.
    append_failing: bool,
    /// The batch being written, and its bytes, kept for the next.
    batch: Vec<Reservation>,
    bytes: Vec<u8>,
}

impl LogWriter {
    /// Until the guard is closed: wait for a reservation, then log it with
    /// every other one queued by then.
    fn run(mut self, queue: mpsc::Receiver<Queued>) {
        while let Ok(Queued::Reservation(first)) = queue.recv() {
            self.batch.push(first);
            let mut closed = false;
            for queued in queue.try_iter().take(MAX_BATCH - 1) {
                match queued {
                    Queued::Reservation(reservation) => self.batch.push(reservation),
                    Queued::Close => {
                        closed = true;
                        break;
                    }
                }
            }
            self.write_batch();
            if closed {
                return;
            }
        }
    }

    /// Logs the batch, settles each admission in it, compacts the log if
    /// that is due, then answers them.
    fn write_batch(&mut self) {
        self.bytes.clear();
        for reservation in &self.batch {
            self.bytes.extend_from_slice(&reservation.record.to_bytes());
        }
        let latched = self.admissions.lock().log_failed;
        let logged = !latched && self.append();
        {
            let mut admissions = self.admissions.lock();
            for Reservation {
                record,
                count,
                total_started_at,
                answer: _,
            } in &self.batch
            {
                // Out of `pending` and into the filter in one step, so no
                // copy finds it in neither.
                admissions.pending.remove(&record.fingerprint);
                if logged {
                    admissions
                        .bloom
                        .insert(&record.fingerprint, record.timestamp);
                } else {
                    admissions.release(*count, *total_started_at);
                }
            }
        }
        let newest = self
            .batch
            .iter()
            .map(|reservation| reservation.record.timestamp)
            .max()
            .unwrap_or(self.last_compact_at);
        if logged && newest.saturating_sub(self.last_compact_at) >= REPLAY_COMPACT_INTERVAL_SECONDS
        {
            self.compact(newest);
        }
        let answer = if logged {
            FirstFlightAdmit::Fresh
        } else {
            FirstFlightAdmit::Unavailable
        };
        for reservation in self.batch.drain(..) {
            // A connection that stopped waiting needs no answer.
            let _ = reservation.answer.send(answer);
        }
    }

    /// Appends the batch's bytes durably; `false`, said in the relay's log,
    /// when it could not.
    fn append(&mut self) -> bool {
        match append_replay_records(&self.path, &self.bytes) {
            Ok(()) => {
                self.append_failing = false;
                true
            }
            Err(AppendFailure::Refused { step, error }) => {
                // Said once for a run of failures, such as a full disk's, not
                // once for every connection it refuses.
                if !std::mem::replace(&mut self.append_failing, true) {
                    log_append_failed(&self.path, step, &error);
                }
                false
            }
            Err(AppendFailure::Latched { step, error, cause }) => {
                self.admissions.lock().log_failed = true;
                log_unavailable(&self.path, step, &error, cause.as_ref());
                false
            }
        }
    }

    fn compact(&mut self, now: u64) {
        // While the relay runs, an append either completes or is rolled back,
        // so a cut-off record here was not left by this relay.
        let window_seconds = self.admissions.lock().window_seconds;
        let compacted = read_replay_log(&self.path)
            .and_then(ReplayLog::whole)
            .map_err(|error| ("read", error))
            .and_then(|records| {
                let live = records
                    .into_iter()
                    .filter(|record| now.saturating_sub(record.timestamp) <= window_seconds)
                    .collect::<Vec<_>>();
                rewrite_live(&self.path, &live).map_err(|error| ("rewrite", error))
            });
        if let Err((step, error)) = compacted {
            self.admissions.lock().log_failed = true;
            log_unavailable(&self.path, step, &error, None);
            return;
        }
        self.last_compact_at = now;
    }
}

/// Why a batch of admissions could not be appended to the log.
enum AppendFailure {
    /// The batch's admissions are refused, and the next batch tries again: the
    /// log is as it was, as after a full disk's failed write that was rolled
    /// back.
    Refused {
        step: &'static str,
        error: std::io::Error,
    },
    /// The log may hold what was never admitted, or lose what was, so every
    /// admission is refused from now on. `cause` is the append error that a
    /// failed rollback was undoing.
    Latched {
        step: &'static str,
        error: std::io::Error,
        cause: Option<std::io::Error>,
    },
}

/// Append `records`, whole records back to back, to the log at `path` and
/// make them durable, or say which step failed.
fn append_replay_records(path: &Path, records: &[u8]) -> Result<(), AppendFailure> {
    let refused = |step| move |error| AppendFailure::Refused { step, error };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(refused("create its directory"))?;
    }
    let created = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(refused("open"))?;
    let start_len = file.metadata().map_err(refused("read its length"))?.len();
    if let Err(error) = file.write_all(records).and_then(|()| file.sync_data()) {
        drop(file);
        return Err(match truncate_replay_log(path, start_len) {
            Ok(()) => AppendFailure::Refused {
                step: "append a record",
                error,
            },
            Err(rollback) => AppendFailure::Latched {
                step: "roll back a partial record",
                error: rollback,
                cause: Some(error),
            },
        });
    }
    if created && let Err(error) = durable_file::sync_parent_directory(path) {
        return Err(AppendFailure::Latched {
            step: "sync its directory",
            error,
            cause: None,
        });
    }
    Ok(())
}

/// Say why the log stopped admitting. From here on every protocol-v2 first
/// flight is refused, and a peer learns only that the store is unavailable:
/// the operator who has to act needs the file and the cause. `cause` is what
/// led to the failed step, when it was another failure.
fn log_unavailable(
    path: &Path,
    step: &'static str,
    error: &std::io::Error,
    cause: Option<&std::io::Error>,
) {
    tracing::error!(
        event = "replay_log_unavailable",
        path = %path.display(),
        step,
        %error,
        cause = cause.map(tracing::field::display),
        "first-flight replay log failed; protocol-v2 connections are refused while it is unavailable"
    );
}

/// Say why an append failed without leaving the log unusable, as a full disk's
/// does: each protocol-v2 first flight is refused until one succeeds again,
/// and a peer learns only that the store is unavailable.
fn log_append_failed(path: &Path, step: &'static str, error: &std::io::Error) {
    tracing::error!(
        event = "replay_log_append_failed",
        path = %path.display(),
        step,
        %error,
        "first-flight replay log append failed; protocol-v2 connections are refused until an append succeeds"
    );
}

/// The log as read: its whole records, then the bytes of one cut off.
struct ReplayLog {
    records: Vec<ReplayRecord>,
    cut_off: Vec<u8>,
}

impl ReplayLog {
    /// The records of a log that has none cut off.
    fn whole(self) -> std::io::Result<Vec<ReplayRecord>> {
        if self.cut_off.is_empty() {
            Ok(self.records)
        } else {
            Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "truncated first-flight replay record",
            ))
        }
    }
}

/// Reads the log whole, and closes it before returning.
///
/// Both callers go on to replace this file. On Windows a handle still open on
/// it makes `MoveFileExW` refuse that replace, leaving it to std's POSIX-rename
/// retry, which older releases and some file systems lack; a failed rewrite
/// latches `log_failed` for the life of the process.
fn read_replay_log(path: &Path) -> std::io::Result<ReplayLog> {
    let bytes = std::fs::read(path)?;
    let (records, cut_off) = bytes.as_chunks::<REPLAY_RECORD_LEN>();
    Ok(ReplayLog {
        records: records
            .iter()
            .copied()
            .map(ReplayRecord::from_bytes)
            .collect(),
        cut_off: cut_off.to_vec(),
    })
}

/// Cuts the log back to `len`, removing a partly appended record.
///
/// This opens a handle of its own rather than reusing the append one: on
/// Windows an append handle lacks `FILE_WRITE_DATA`, so `set_len` through it
/// always fails, and the rollback would latch `log_failed` on any write error.
pub(super) fn truncate_replay_log(path: &Path, len: u64) -> std::io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(len)?;
    file.sync_data()
}

/// The filter bits a fingerprint sets: each of its four big-endian `u64`s,
/// modulo the filter's bit count.
fn bloom_positions(
    filter_len: usize,
    fingerprint: &[u8; FINGERPRINT_LEN],
) -> impl Iterator<Item = usize> {
    let bits = filter_len * 8;
    let (hashes, _) = fingerprint.as_chunks::<8>();
    hashes
        .iter()
        .map(move |hash| u64::from_be_bytes(*hash) as usize % bits)
}

fn bloom_contains(filter: &[u8], fingerprint: &[u8; FINGERPRINT_LEN]) -> bool {
    bloom_positions(filter.len(), fingerprint)
        .all(|position| filter[position / 8] & (1 << (position % 8)) != 0)
}

fn bloom_insert(filter: &mut [u8], fingerprint: &[u8; FINGERPRINT_LEN]) {
    for position in bloom_positions(filter.len(), fingerprint) {
        filter[position / 8] |= 1 << (position % 8);
    }
}
