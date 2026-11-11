//! Fast process-local duplicate admission guard for protocol-v2 first flights.
//!
//! ```text
//! key id + salt -> SHA-256 fingerprint -> current Bloom window
//!                                      -> previous Bloom window
//! key id ---------> per-credential first-flight count (before Bloom insert)
//! ```
//!
//! `admit` is called while one mutex is held, making concurrent admission
//! atomic. This Bloom filter protects all connection types from immediate duplicates;
//! administrator mutations additionally use the exact durable replay set in `auth`.
//!
//! Each generation lasts `2 *` the accepted clock-skew so a salt inserted at the
//! end of a window with a max-future timestamp cannot be replayed after rotation.
//! Per-credential counts stop one tenant from filling the shared filter with
//! unique salts before the request payload is decoded.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use pb_mapper_auth::KeyId;
use pb_mapper_core::config;
use pb_mapper_core::durable_file::{self, NewFileMode};
use ring::digest::{SHA256, SHA256_OUTPUT_LEN, digest};

use super::{CONNECTION_SALT_LEN, pack, unix_seconds};

/// A first-flight fingerprint: SHA-256 of the key id and connection salt.
const FINGERPRINT_LEN: usize = SHA256_OUTPUT_LEN;
/// One admission in the log: its fingerprint, then the admission time.
pub(super) const REPLAY_RECORD_LEN: usize = FINGERPRINT_LEN + size_of::<u64>();
pub(super) const REPLAY_COMPACT_INTERVAL_SECONDS: u64 = 60;

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

pub(super) struct ReplayGuard {
    bloom: RotatingBloom,
    counts: HashMap<KeyId, u32>,
    counts_started_at: u64,
    window_seconds: u64,
    max_per_key: u32,
    total: u32,
    total_started_at: u64,
    max_total: u32,
    log_path: Option<PathBuf>,
    last_compact_at: u64,
    log_failed: bool,
    /// An append has failed, and been logged, since the last one that
    /// succeeded.
    append_failing: bool,
}

impl ReplayGuard {
    pub(super) fn open(log_path: Option<PathBuf>, bytes: usize, window_seconds: u64) -> Self {
        let now = unix_seconds();
        let configured = first_flight_budget(window_seconds);
        let mut guard = Self {
            bloom: RotatingBloom::new(bytes, window_seconds),
            counts: HashMap::new(),
            counts_started_at: now,
            window_seconds,
            max_per_key: configured,
            total: 0,
            total_started_at: now,
            max_total: configured.min(bloom_insert_capacity(bytes)),
            log_path,
            last_compact_at: now,
            log_failed: false,
            append_failing: false,
        };
        guard.load_persisted();
        guard
    }

    #[cfg(test)]
    pub(super) fn with_max_per_key(mut self, max_per_key: u32) -> Self {
        self.max_per_key = max_per_key;
        self
    }

    #[cfg(test)]
    pub(super) fn with_max_total(mut self, max_total: u32) -> Self {
        self.max_total = max_total;
        self
    }

    pub(super) fn admit(
        &mut self,
        key_id: KeyId,
        fingerprint: &[u8; FINGERPRINT_LEN],
        now: u64,
    ) -> FirstFlightAdmit {
        self.rotate_counts(now);
        if self.bloom.contains(fingerprint, now) {
            return FirstFlightAdmit::Replayed;
        }
        self.reset_total_if_rotated();
        if self.counts.get(&key_id).copied().unwrap_or(0) >= self.max_per_key
            || self.total >= self.max_total
        {
            return FirstFlightAdmit::Limited;
        }
        let result = self.reserve(fingerprint, now);
        if result == FirstFlightAdmit::Fresh {
            *self.counts.entry(key_id).or_insert(0) += 1;
            if now.saturating_sub(self.last_compact_at) >= REPLAY_COMPACT_INTERVAL_SECONDS {
                self.compact(now);
            }
        }
        result
    }

    pub(super) fn claim(
        &mut self,
        fingerprint: &[u8; FINGERPRINT_LEN],
        now: u64,
    ) -> FirstFlightAdmit {
        if self.bloom.contains(fingerprint, now) {
            return FirstFlightAdmit::Replayed;
        }
        self.reset_total_if_rotated();
        if self.total >= self.max_total {
            return FirstFlightAdmit::Unavailable;
        }
        self.reserve(fingerprint, now)
    }

    fn reset_total_if_rotated(&mut self) {
        if self.bloom.current_started_at != self.total_started_at {
            self.total = 0;
            self.total_started_at = self.bloom.current_started_at;
        }
    }

    fn reserve(&mut self, fingerprint: &[u8; FINGERPRINT_LEN], now: u64) -> FirstFlightAdmit {
        if self.persist(fingerprint, now).is_err() {
            return FirstFlightAdmit::Unavailable;
        }
        self.bloom.insert(fingerprint, now);
        self.total = self.total.saturating_add(1);
        FirstFlightAdmit::Fresh
    }

    fn rotate_counts(&mut self, now: u64) {
        if now.saturating_sub(self.counts_started_at) < self.window_seconds {
            return;
        }
        self.counts.clear();
        self.counts_started_at = now;
    }

    fn persist(&mut self, fingerprint: &[u8; FINGERPRINT_LEN], now: u64) -> std::io::Result<()> {
        if self.log_failed {
            return Err(std::io::Error::other(
                "durable first-flight replay log is unavailable after a failed rollback",
            ));
        }
        let Some(path) = &self.log_path else {
            return Ok(());
        };
        let record = ReplayRecord {
            fingerprint: *fingerprint,
            timestamp: now,
        };
        match append_replay_record(path, &record) {
            Ok(()) => {
                self.append_failing = false;
                Ok(())
            }
            Err(AppendFailure::Refused { step, error }) => {
                // Said once for a run of failures, such as a full disk's, not
                // once for every connection it refuses.
                if !std::mem::replace(&mut self.append_failing, true) {
                    log_append_failed(path, step, &error);
                }
                Err(error)
            }
            Err(AppendFailure::Latched { step, error, cause }) => {
                self.log_failed = true;
                log_unavailable(path, step, &error, cause.as_ref());
                Err(error)
            }
        }
    }

    fn load_persisted(&mut self) {
        let Some(path) = self.log_path.clone() else {
            return;
        };
        if !path.exists() {
            return;
        }
        let now = unix_seconds();
        let records =
            match read_replay_log(&path).and_then(|log| self.after_a_crash(&path, log, now)) {
                Ok(records) => records,
                Err(error) => {
                    self.log_failed = true;
                    log_unavailable(&path, "read", &error, None);
                    return;
                }
            };
        let mut live = Vec::new();
        for record in records {
            if now.saturating_sub(record.timestamp) > self.window_seconds {
                continue;
            }
            self.bloom.insert(&record.fingerprint, record.timestamp);
            live.push(record);
        }
        if let Some(oldest) = live.iter().map(|record| record.timestamp).min() {
            self.bloom.current_started_at = oldest;
        }
        self.total = u32::try_from(live.len())
            .unwrap_or(u32::MAX)
            .min(self.max_total);
        self.total_started_at = self.bloom.current_started_at;
        if let Err(error) = self.rewrite_live(&live) {
            self.log_failed = true;
            log_unavailable(&path, "rewrite", &error, None);
            return;
        }
        self.last_compact_at = now;
    }

    /// The records of a log read at start, less the record a crash cut off,
    /// if one did.
    ///
    /// A crash in the middle of an append leaves that append's record cut off
    /// as the last thing in the log, and nothing else: each append adds one
    /// record at the end, and one that fails is rolled back or stops every
    /// append after it. Its admission was never acknowledged, which waits for
    /// the record to be synced, so dropping it loses nothing replay protection
    /// needs. A log cut off anywhere else reads out of step from there on, and
    /// a file that is no replay log reads as records no relay wrote: either
    /// way some record, or the cut-off one at the earliest it can be, names a
    /// time past the replay window ahead of this relay's clock, and the log is
    /// refused, as one is that a relay wrote before its clock was set back by
    /// more than the window.
    fn after_a_crash(
        &self,
        path: &Path,
        ReplayLog { records, cut_off }: ReplayLog,
        now: u64,
    ) -> std::io::Result<Vec<ReplayRecord>> {
        if cut_off.is_empty() {
            return Ok(records);
        }
        // The earliest time the cut-off record can have had.
        let mut completed = [0_u8; REPLAY_RECORD_LEN];
        for (byte, kept) in completed.iter_mut().zip(&cut_off) {
            *byte = *kept;
        }
        let latest = now.saturating_add(self.window_seconds);
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

    fn compact(&mut self, now: u64) {
        let Some(path) = &self.log_path else {
            self.last_compact_at = now;
            return;
        };
        // While the relay runs, an append either completes or is rolled back,
        // so a cut-off record here was not left by this relay.
        let records = match read_replay_log(path).and_then(ReplayLog::whole) {
            Ok(records) => records,
            Err(error) => {
                self.log_failed = true;
                log_unavailable(path, "read", &error, None);
                return;
            }
        };
        let live = records
            .into_iter()
            .filter(|record| now.saturating_sub(record.timestamp) <= self.window_seconds)
            .collect::<Vec<_>>();
        if let Err(error) = self.rewrite_live(&live) {
            self.log_failed = true;
            log_unavailable(path, "rewrite", &error, None);
            return;
        }
        self.last_compact_at = now;
    }

    fn rewrite_live(&self, live: &[ReplayRecord]) -> std::io::Result<()> {
        let Some(path) = &self.log_path else {
            return Ok(());
        };
        let records = live.iter().map(ReplayRecord::to_bytes).collect::<Vec<_>>();
        durable_file::replace_contents(path, records.as_flattened(), NewFileMode::Default)
            .map_err(std::io::Error::from)
    }
}

/// Why an admission could not be appended to the log.
enum AppendFailure {
    /// This admission is refused, and the next one tries again: the log is as
    /// it was, as after a full disk's failed write that was rolled back.
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

/// Append `record` to the log at `path` and make it durable, or say which
/// step failed.
fn append_replay_record(path: &Path, record: &ReplayRecord) -> Result<(), AppendFailure> {
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
    if let Err(error) = file
        .write_all(&record.to_bytes())
        .and_then(|()| file.sync_data())
    {
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
