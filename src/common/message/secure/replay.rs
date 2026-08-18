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
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

use super::*;

const DEFAULT_NEW_STREAMS_PER_SECOND: u32 = 100;
const REPLAY_RECORD_LEN: usize = 40;
const REPLAY_COMPACT_INTERVAL_SECONDS: u64 = 60;

fn first_flight_budget(window_seconds: u64) -> u32 {
    let streams_per_sec = std::env::var("PB_MAPPER_NEW_STREAMS_PER_SECOND")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_NEW_STREAMS_PER_SECOND)
        .min(1_000_000);
    let window = u32::try_from(window_seconds).unwrap_or(u32::MAX);
    streams_per_sec
        .saturating_mul(2)
        .saturating_mul(window)
        .saturating_mul(2)
        .max(8_192)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FirstFlightAdmit {
    Fresh,
    Replayed,
    Limited,
    Unavailable,
}

pub(super) fn replay_fingerprint(key_id: u64, salt: &[u8; CONNECTION_SALT_LEN]) -> [u8; 32] {
    let mut input = [0_u8; 8 + CONNECTION_SALT_LEN];
    input[..8].copy_from_slice(&key_id.to_be_bytes());
    input[8..].copy_from_slice(salt);
    digest(&SHA256, &input)
        .as_ref()
        .try_into()
        .expect("SHA-256 width")
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

    pub(super) fn contains(&mut self, fingerprint: &[u8; 32], now: u64) -> bool {
        self.rotate(now);
        bloom_contains(&self.current, fingerprint) || bloom_contains(&self.previous, fingerprint)
    }

    pub(super) fn insert(&mut self, fingerprint: &[u8; 32], now: u64) {
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
    counts: HashMap<u64, u32>,
    counts_started_at: u64,
    window_seconds: u64,
    max_per_key: u32,
    log_path: Option<PathBuf>,
    last_compact_at: u64,
    log_failed: bool,
}

impl ReplayGuard {
    pub(super) fn open(log_path: Option<PathBuf>, bytes: usize, window_seconds: u64) -> Self {
        let now = unix_seconds();
        let mut guard = Self {
            bloom: RotatingBloom::new(bytes, window_seconds),
            counts: HashMap::new(),
            counts_started_at: now,
            window_seconds,
            max_per_key: first_flight_budget(window_seconds),
            log_path,
            last_compact_at: now,
            log_failed: false,
        };
        guard.load_persisted();
        guard
    }

    #[cfg(test)]
    pub(super) fn with_max_per_key(mut self, max_per_key: u32) -> Self {
        self.max_per_key = max_per_key;
        self
    }

    pub(super) fn admit(
        &mut self,
        key_id: u64,
        fingerprint: &[u8; 32],
        now: u64,
    ) -> FirstFlightAdmit {
        self.rotate_counts(now);
        if self.bloom.contains(fingerprint, now) {
            return FirstFlightAdmit::Replayed;
        }
        if self.counts.get(&key_id).copied().unwrap_or(0) >= self.max_per_key {
            return FirstFlightAdmit::Limited;
        }
        if self.persist(fingerprint, now).is_err() {
            return FirstFlightAdmit::Unavailable;
        }
        self.bloom.insert(fingerprint, now);
        *self.counts.entry(key_id).or_insert(0) += 1;
        if now.saturating_sub(self.last_compact_at) >= REPLAY_COMPACT_INTERVAL_SECONDS {
            self.compact(now);
        }
        FirstFlightAdmit::Fresh
    }

    fn rotate_counts(&mut self, now: u64) {
        if now.saturating_sub(self.counts_started_at) < self.window_seconds {
            return;
        }
        self.counts.clear();
        self.counts_started_at = now;
    }

    fn persist(&mut self, fingerprint: &[u8; 32], now: u64) -> std::io::Result<()> {
        if self.log_failed {
            return Err(std::io::Error::other(
                "durable first-flight replay log is unavailable after a failed rollback",
            ));
        }
        let Some(path) = &self.log_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let created = !path.exists();
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let start_len = file.metadata()?.len();
        let mut record = [0_u8; REPLAY_RECORD_LEN];
        record[..32].copy_from_slice(fingerprint);
        record[32..].copy_from_slice(&now.to_be_bytes());
        if let Err(error) = file.write_all(&record).and_then(|()| file.sync_data()) {
            if file
                .set_len(start_len)
                .and_then(|()| file.sync_data())
                .is_err()
            {
                self.log_failed = true;
            }
            return Err(error);
        }
        if created {
            if let Err(error) = crate::common::auth::sync_parent_directory(path) {
                self.log_failed = true;
                return Err(std::io::Error::other(error.to_string()));
            }
        }
        Ok(())
    }

    fn load_persisted(&mut self) {
        let Some(path) = self.log_path.clone() else {
            return;
        };
        if !path.exists() {
            return;
        }
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                self.log_failed = true;
                return;
            }
        };
        let now = unix_seconds();
        let mut live = Vec::new();
        let mut record = [0_u8; REPLAY_RECORD_LEN];
        loop {
            match file.read(&mut record) {
                Ok(0) => break,
                Ok(n) if n == REPLAY_RECORD_LEN => {}
                _ => break,
            }
            let timestamp = u64::from_be_bytes(record[32..].try_into().expect("timestamp width"));
            if now.saturating_sub(timestamp) > self.window_seconds {
                continue;
            }
            let fingerprint: [u8; 32] = record[..32].try_into().expect("fingerprint width");
            self.bloom.insert(&fingerprint, timestamp);
            live.push(record);
        }
        if self.rewrite_live(&live).is_ok() {
            self.last_compact_at = now;
        }
    }

    fn compact(&mut self, now: u64) {
        let Some(path) = &self.log_path else {
            self.last_compact_at = now;
            return;
        };
        let Ok(mut file) = File::open(path) else {
            self.last_compact_at = now;
            return;
        };
        let mut live = Vec::new();
        let mut record = [0_u8; REPLAY_RECORD_LEN];
        loop {
            match file.read(&mut record) {
                Ok(0) => break,
                Ok(n) if n == REPLAY_RECORD_LEN => {}
                _ => break,
            }
            let timestamp = u64::from_be_bytes(record[32..].try_into().expect("timestamp width"));
            if now.saturating_sub(timestamp) <= self.window_seconds {
                live.push(record);
            }
        }
        if self.rewrite_live(&live).is_ok() {
            self.last_compact_at = now;
        }
    }

    fn rewrite_live(&self, live: &[[u8; REPLAY_RECORD_LEN]]) -> std::io::Result<()> {
        let Some(path) = &self.log_path else {
            return Ok(());
        };
        let temporary = path.with_file_name(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("connection.replay"),
            std::process::id()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&live.concat())?;
            file.sync_all()?;
            drop(file);
            crate::common::auth::replace_file(&temporary, path)?;
            crate::common::auth::sync_parent_directory(path)
                .map_err(|error| std::io::Error::other(error.to_string()))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

fn bloom_positions(filter_len: usize, fingerprint: &[u8; 32]) -> [usize; 4] {
    let bits = filter_len * 8;
    std::array::from_fn(|index| {
        let offset = index * 8;
        let hash = u64::from_be_bytes(
            fingerprint[offset..offset + 8]
                .try_into()
                .expect("fingerprint chunk"),
        );
        hash as usize % bits
    })
}

fn bloom_contains(filter: &[u8], fingerprint: &[u8; 32]) -> bool {
    bloom_positions(filter.len(), fingerprint)
        .into_iter()
        .all(|position| filter[position / 8] & (1 << (position % 8)) != 0)
}

fn bloom_insert(filter: &mut [u8], fingerprint: &[u8; 32]) {
    for position in bloom_positions(filter.len(), fingerprint) {
        filter[position / 8] |= 1 << (position % 8);
    }
}
