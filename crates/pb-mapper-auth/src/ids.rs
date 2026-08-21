//! Identity types for temporary credentials.
//!
//! ```text
//!  KeyId (u64) — what a client presents
//! ┌──────────────────────────┬──────────────────────────┐
//! │ Generation (high 32)     │ SlotIndex (low 32)       │
//! └──────────────────────────┴──────────────────────────┘
//!      which tenant of the row       which row of the table
//! ```
//!
//! These were all bare integers, which made `make_key_id(generation, slot)`
//! accept its arguments in either order and let a slot index be compared against
//! a generation without complaint. Separate types make both a compile error, and
//! keep a `KeyId` from being used as an array index by mistake — the only way to
//! get one is [`KeyId::slot`], which is also the only place the truncation to a
//! row number is expressed.
//!
//! All three are `#[serde(transparent)]`, so persisted snapshots and the admin
//! wire protocol keep the plain-integer encoding they already had.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The identity a client presents: a [`SlotIndex`] paired with the
/// [`Generation`] of the row it was issued from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyId(u64);

/// Which tenant of a slot a credential belongs to. Bumped every time the row is
/// reissued, and never reset, so a retired credential can never match the row
/// that replaced it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Generation(u32);

/// Which row of the slot table a credential lives in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SlotIndex(u32);

/// The administrator, which owns no slot and never expires.
pub const ADMIN_KEY_ID: KeyId = KeyId(0);

impl KeyId {
    pub const fn new(generation: Generation, slot: SlotIndex) -> Self {
        Self(((generation.0 as u64) << 32) | slot.0 as u64)
    }

    pub const fn generation(self) -> Generation {
        Generation((self.0 >> 32) as u32)
    }

    pub const fn slot(self) -> SlotIndex {
        SlotIndex(self.0 as u32)
    }

    pub const fn is_admin(self) -> bool {
        self.0 == ADMIN_KEY_ID.0
    }

    /// The bytes mixed into the credential's key derivation.
    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn from_u64(raw: u64) -> Self {
        Self(raw)
    }
}

impl Generation {
    pub const FIRST: Self = Self(0);

    /// The generation for a reissue of this row, or `None` once the row has been
    /// cycled `u32::MAX` times and can no longer produce a fresh identity.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }

    pub const fn from_u32(raw: u32) -> Self {
        Self(raw)
    }
}

impl SlotIndex {
    pub const fn as_index(self) -> usize {
        self.0 as usize
    }

    /// # Panics
    ///
    /// If `index` exceeds `u32::MAX`. `MAX_TEMP_KEY_CAPACITY` caps the table far
    /// below that, so a real index cannot reach it; panicking keeps a future
    /// capacity change from silently wrapping into another row's identity.
    pub fn from_index(index: usize) -> Self {
        match u32::try_from(index) {
            Ok(index) => Self(index),
            Err(_) => panic!("slot index exceeds the addressable slot table"),
        }
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Display for SlotIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
