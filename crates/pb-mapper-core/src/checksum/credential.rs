//! Credentials: an administrator key or a temporary credential, parsed from and
//! encoded to their text forms.

use std::ops::Range;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use ring::digest::{SHA256, digest};

use super::ENV_MSG_HEADER_KEY;

pub const TEMP_CREDENTIAL_PREFIX: &str = "pbmt1_";
pub const ADMIN_KEY_LEN: usize = 32;
const _: () = assert!(
    ADMIN_KEY_LEN == size_of::<AesKeyType>(),
    "an administrator key's bytes are its AES-256 key"
);

// A temporary credential, after its prefix, is the base64url of: the version,
// the big-endian key id, the key, and the first bytes of the SHA-256 of all
// that precedes them.
const TEMP_VERSION: u8 = 1;
const TEMP_KEY_ID: Range<usize> = 1..1 + size_of::<u64>();
const TEMP_KEY: Range<usize> = TEMP_KEY_ID.end..TEMP_KEY_ID.end + size_of::<AesKeyType>();
const TEMP_DIGEST_LEN: usize = 4;
const TEMP_PAYLOAD_LEN: usize = TEMP_KEY.end + TEMP_DIGEST_LEN;
const _: () = assert!(
    TEMP_PAYLOAD_LEN == 45,
    "temporary credentials already issued are 45-byte payloads"
);

/// Administrator keys are also stored in `MSG_HEADER_KEY`. `std::env::set_var`
/// panics on interior NUL, so the key must be printable ASCII with no whitespace.
pub fn is_env_safe_admin_key(bytes: &[u8]) -> bool {
    bytes.len() == ADMIN_KEY_LEN && bytes.iter().all(|byte| byte.is_ascii_graphic())
}

pub fn env_safe_admin_key_error() -> String {
    format!(
        "`{ENV_MSG_HEADER_KEY}` administrator key must be {ADMIN_KEY_LEN} printable ASCII bytes without whitespace or NUL"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    Admin(AesKeyType),
    Temporary { key_id: u64, key: AesKeyType },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admin(_) => f.debug_tuple("Admin").field(&"[redacted]").finish(),
            Self::Temporary { key_id, .. } => f
                .debug_struct("Temporary")
                .field("key_id", key_id)
                .field("key", &"[redacted]")
                .finish(),
        }
    }
}

impl Credential {
    pub fn key_id(&self) -> u64 {
        match self {
            Self::Admin(_) => 0,
            Self::Temporary { key_id, .. } => *key_id,
        }
    }

    pub fn key(&self) -> &AesKeyType {
        match self {
            Self::Admin(key) | Self::Temporary { key, .. } => key,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Admin(_))
    }
}

fn key_len_error(input: &str) -> String {
    format!(
        "`{ENV_MSG_HEADER_KEY}` administrator key must be exactly {ADMIN_KEY_LEN} bytes; received {} bytes",
        input.len()
    )
}

pub fn parse_credential(raw: &str) -> Result<Credential, String> {
    if let Some(encoded) = raw.strip_prefix(TEMP_CREDENTIAL_PREFIX) {
        let payload = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "temporary credential is not valid base64url".to_string())?;
        if payload.len() != TEMP_PAYLOAD_LEN {
            return Err(format!(
                "temporary credential payload must be {TEMP_PAYLOAD_LEN} bytes, got {}",
                payload.len()
            ));
        }
        if payload[0] != TEMP_VERSION {
            return Err(format!(
                "unsupported temporary credential version {}",
                payload[0]
            ));
        }
        let expected = digest(&SHA256, &payload[..TEMP_KEY.end]);
        if expected.as_ref()[..TEMP_DIGEST_LEN] != payload[TEMP_KEY.end..TEMP_PAYLOAD_LEN] {
            return Err("temporary credential checksum mismatch".to_string());
        }
        // The length check above already guarantees both widths, so neither arm
        // is reachable — but this returns `Result` anyway, so saying so costs a
        // line and removes a panic from a path that parses network input.
        let key_id = u64::from_be_bytes(
            payload[TEMP_KEY_ID]
                .try_into()
                .map_err(|_| "temporary credential key id is malformed".to_string())?,
        );
        if key_id == 0 {
            return Err("temporary credential key id must not be zero".to_string());
        }
        let key = payload[TEMP_KEY]
            .try_into()
            .map_err(|_| "temporary credential key is malformed".to_string())?;
        return Ok(Credential::Temporary { key_id, key });
    }

    let bytes = raw.as_bytes();
    if bytes.len() != ADMIN_KEY_LEN {
        return Err(key_len_error(raw));
    }
    if !is_env_safe_admin_key(bytes) {
        return Err(env_safe_admin_key_error());
    }
    // Unreachable after the `ADMIN_KEY_LEN` check above, for the same reason.
    Ok(Credential::Admin(
        bytes.try_into().map_err(|_| key_len_error(raw))?,
    ))
}

pub fn encode_temporary_credential(key_id: u64, key: &AesKeyType) -> String {
    let mut payload = Vec::with_capacity(TEMP_PAYLOAD_LEN);
    payload.push(TEMP_VERSION);
    payload.extend_from_slice(&key_id.to_be_bytes());
    payload.extend_from_slice(key);
    let checksum = digest(&SHA256, &payload);
    payload.extend_from_slice(&checksum.as_ref()[..TEMP_DIGEST_LEN]);
    format!(
        "{TEMP_CREDENTIAL_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(payload)
    )
}

pub type AesKeyType = [u8; 32];

/// Generate a random printable 32-byte key.
///
/// This helper is used when a transient key is preferred over deterministic
/// machine-derived key material.
pub fn gen_random_key() -> AesKeyType {
    const CHARSET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

    let mut rng = rand::rng();
    let mut random_key = AesKeyType::default();
    for byte in &mut random_key {
        *byte = CHARSET[rng.random_range(0..CHARSET.len())];
    }
    random_key
}

#[cfg(test)]
mod tests {
    #[test]
    fn credential_debug_never_prints_key_bytes() {
        let key = *b"0123456789abcdefghijklmnopqrstuv";
        assert_eq!(
            format!("{:?}", super::Credential::Admin(key)),
            "Admin(\"[redacted]\")"
        );
        assert_eq!(
            format!("{:?}", super::Credential::Temporary { key_id: 7, key }),
            "Temporary { key_id: 7, key: \"[redacted]\" }"
        );
    }

    /// An administrator key is 32 printable ASCII bytes: no NUL, no
    /// whitespace, whether checked directly or parsed as a credential.
    #[test]
    fn administrator_keys_are_printable_ascii_without_nul_or_whitespace() {
        use super::*;
        assert!(is_env_safe_admin_key(b"0123456789abcdefghijklmnopqrstuv"));
        let mut with_nul = *b"0123456789abcdefghijklmnopqrstuv";
        with_nul[8] = 0;
        assert!(!is_env_safe_admin_key(&with_nul));
        assert!(!is_env_safe_admin_key(b"short"));
        assert!(parse_credential(std::str::from_utf8(&with_nul).unwrap()).is_err());
        assert!(parse_credential("0123456789abcdefghijklmnopq rstuv").is_err());
    }

    #[test]
    fn temporary_credential_round_trip_and_checksum() {
        use super::*;

        let key = [7_u8; 32];
        let encoded = encode_temporary_credential(0x0000_0007_0000_002a, &key);
        assert_eq!(
            parse_credential(&encoded).unwrap(),
            Credential::Temporary {
                key_id: 0x0000_0007_0000_002a,
                key
            }
        );

        // The wire form: version, big-endian key id, key, four digest bytes.
        assert_eq!(
            encoded,
            "pbmt1_AQAAAAcAAAAqBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBweejWIj"
        );

        let mut corrupted = encoded.into_bytes();
        let last = corrupted.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert!(parse_credential(std::str::from_utf8(&corrupted).unwrap()).is_err());
    }
}
