use ring::aead::{
    AES_256_GCM, Aad, BoundKey, NONCE_LEN, Nonce, NonceSequence, OpeningKey, SealingKey, Tag,
    UnboundKey,
};

type RingResult<T> = Result<T, ring::error::Unspecified>;

/// A nonce is a direction prefix, then a big-endian message counter.
const PREFIX_LEN: usize = 4;
const COUNTER_LEN: usize = size_of::<u64>();
const _: () = assert!(
    PREFIX_LEN + COUNTER_LEN == NONCE_LEN,
    "a nonce is a 4-byte direction prefix and a big-endian 64-bit counter"
);

/// How many messages a legacy codec seals or opens under one key: it predates
/// the 64-bit counter of data v2, whose limit is `u64::MAX`.
const LEGACY_LIMIT: u64 = u32::MAX as u64;

/// The next message's counter, and the count at which the key is spent.
struct Counter {
    next: u64,
    limit: u64,
}

impl Counter {
    fn advance(&mut self) -> RingResult<()> {
        if self.next >= self.limit {
            return Err(ring::error::Unspecified);
        }
        self.next += 1;
        Ok(())
    }

    fn to_bytes(&self) -> [u8; COUNTER_LEN] {
        self.next.to_be_bytes()
    }
}

/// The nonces of one key in one direction.
///
/// Neither `Clone` nor `Copy`: a copy would hand out the same nonces again
/// under the same key.
struct CounterNonceSequence {
    counter: Counter,
    nonce: [u8; NONCE_LEN],
}

impl Default for CounterNonceSequence {
    /// The legacy sequence: no prefix, and [`LEGACY_LIMIT`] messages.
    fn default() -> Self {
        Self {
            counter: Counter {
                next: 0,
                limit: LEGACY_LIMIT,
            },
            nonce: [0; NONCE_LEN],
        }
    }
}

impl CounterNonceSequence {
    /// A data-v2 sequence: a direction prefix and a full 64-bit counter.
    fn data_v2(prefix: [u8; PREFIX_LEN]) -> Self {
        let mut nonce = [0; NONCE_LEN];
        nonce[..PREFIX_LEN].copy_from_slice(&prefix);
        Self {
            counter: Counter {
                next: 0,
                limit: u64::MAX,
            },
            nonce,
        }
    }
}

impl NonceSequence for CounterNonceSequence {
    // called once for each seal operation
    fn advance(&mut self) -> RingResult<Nonce> {
        self.nonce[PREFIX_LEN..].copy_from_slice(&self.counter.to_bytes());
        self.counter.advance()?;
        Ok(Nonce::assume_unique_for_key(self.nonce))
    }
}

#[derive(Debug)]
pub struct Aes256GcmEnCodec {
    seal: SealingKey<CounterNonceSequence>,
}

impl Aes256GcmEnCodec {
    /// Construct a negotiated data-v2 codec with a directional nonce prefix and a
    /// checked 64-bit counter. Legacy constructors retain their 32-bit limit.
    /// Peers sharing a key must use disjoint prefixes for opposite directions.
    pub fn try_new_data_v2(key: &[u8], prefix: [u8; 4]) -> RingResult<Self> {
        Ok(Self {
            seal: SealingKey::new(
                UnboundKey::new(&AES_256_GCM, key)?,
                CounterNonceSequence::data_v2(prefix),
            ),
        })
    }

    pub fn try_new(key: &[u8]) -> RingResult<Self> {
        Ok(Self {
            seal: SealingKey::new(
                UnboundKey::new(&AES_256_GCM, key)?,
                CounterNonceSequence::default(),
            ),
        })
    }
}

impl Encryptor for Aes256GcmEnCodec {
    fn encrypt(&mut self, data: &mut [u8]) -> RingResult<Tag> {
        self.seal.seal_in_place_separate_tag(Aad::empty(), data)
    }
}

#[derive(Debug)]
pub struct Aes256GcmDeCodec {
    open: OpeningKey<CounterNonceSequence>,
}

impl Aes256GcmDeCodec {
    /// Construct the receiving half of a negotiated data-v2 codec.
    pub fn try_new_data_v2(key: &[u8], prefix: [u8; 4]) -> RingResult<Self> {
        Ok(Self {
            open: OpeningKey::new(
                UnboundKey::new(&AES_256_GCM, key)?,
                CounterNonceSequence::data_v2(prefix),
            ),
        })
    }

    pub fn try_new(key: &[u8]) -> RingResult<Self> {
        Ok(Self {
            open: OpeningKey::new(
                UnboundKey::new(&AES_256_GCM, key)?,
                CounterNonceSequence::default(),
            ),
        })
    }
}

impl Decryptor for Aes256GcmDeCodec {
    fn decrypt<'a>(&mut self, data: &'a mut [u8]) -> RingResult<&'a mut [u8]> {
        self.open.open_in_place(Aad::empty(), data)
    }
}

/// Seals each message in place under the next nonce, returning its tag.
pub trait Encryptor: 'static {
    fn encrypt(&mut self, data: &mut [u8]) -> RingResult<Tag>;
}

/// Opens each message (ciphertext followed by its tag) in place under the
/// next nonce, returning the plaintext.
pub trait Decryptor {
    fn decrypt<'a>(&mut self, data: &'a mut [u8]) -> RingResult<&'a mut [u8]>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_nonces_keep_their_bytes_and_both_counter_widths_fail_closed() {
        for limit in [u64::from(u32::MAX), u64::MAX] {
            let mut seq = CounterNonceSequence {
                counter: Counter {
                    next: limit - 1,
                    limit,
                },
                nonce: [0; 12],
            };
            let nonce = seq.advance().unwrap();
            assert_eq!(&nonce.as_ref()[..4], &[0; 4]);
            assert_eq!(&nonce.as_ref()[4..], &(limit - 1).to_be_bytes());
            assert!(seq.advance().is_err());
            assert!(seq.advance().is_err());
        }
        let mut seq = CounterNonceSequence::default();
        assert_eq!(seq.advance().unwrap().as_ref(), &[0; 12]);
        assert_eq!(
            seq.advance().unwrap().as_ref(),
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
    }

    /// Which limit each constructor picks, and where the data-v2 prefix goes.
    #[test]
    fn legacy_and_data_v2_sequences_pick_their_prefix_and_limit() {
        let legacy = CounterNonceSequence::default();
        assert_eq!(legacy.counter.limit, u64::from(u32::MAX));
        let mut v2 = CounterNonceSequence::data_v2([0, 0, 0, 1]);
        assert_eq!(v2.counter.limit, u64::MAX);
        assert_eq!(
            v2.advance().unwrap().as_ref(),
            &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn a_sealed_message_opens_with_the_same_key() {
        const KEY: [u8; 32] = [0x42; 32];
        let plaintext = b"pb-mapper frame";
        let mut seal = Aes256GcmEnCodec::try_new(&KEY).unwrap();
        let mut open = Aes256GcmDeCodec::try_new(&KEY).unwrap();
        for _ in 0..2 {
            let mut message = plaintext.to_vec();
            let tag = seal.encrypt(&mut message).unwrap();
            assert_ne!(message, plaintext);
            message.extend_from_slice(tag.as_ref());
            assert_eq!(open.decrypt(&mut message).unwrap(), plaintext);
        }
    }
}
