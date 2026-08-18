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

use super::*;

pub(super) const MAX_FIRST_FLIGHTS_PER_KEY: u32 = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FirstFlightAdmit {
    Fresh,
    Replayed,
    Limited,
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
}

impl ReplayGuard {
    pub(super) fn new(bytes: usize, window_seconds: u64) -> Self {
        Self {
            bloom: RotatingBloom::new(bytes, window_seconds),
            counts: HashMap::new(),
            counts_started_at: unix_seconds(),
            window_seconds,
            max_per_key: MAX_FIRST_FLIGHTS_PER_KEY,
        }
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
        let count = self.counts.entry(key_id).or_insert(0);
        if *count >= self.max_per_key {
            return FirstFlightAdmit::Limited;
        }
        self.bloom.insert(fingerprint, now);
        *count = count.saturating_add(1);
        FirstFlightAdmit::Fresh
    }

    fn rotate_counts(&mut self, now: u64) {
        if now.saturating_sub(self.counts_started_at) < self.window_seconds {
            return;
        }
        self.counts.clear();
        self.counts_started_at = now;
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
