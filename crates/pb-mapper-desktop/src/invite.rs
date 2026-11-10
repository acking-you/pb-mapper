//! The desktop invitation: `pbd1.` + base64url (no padding) of
//!
//! | offset | size | field |
//! | --- | --- | --- |
//! | 0 | 1 | version `0x02` |
//! | 1 | 1 | flags `0x00` |
//! | 2 | 1 | `n`, the service suffix length, 1..=120 |
//! | 3 | n | the bytes after `desktop:` |
//! | 3+n | 32 | SHA-256 of the host certificate DER |
//! | 35+n | 32 | the bearer token |
//! | 67+n | 4 | `SHA-256(bytes[0..67+n])[0..4]`, a typo check |
//!
//! Decoding also accepts the v1 JSON invitation and pins the same certificate,
//! and recognises a pb-mapper setup string pasted in the wrong place.
use base64::{
    Engine, alphabet,
    engine::{
        DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD,
    },
};
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use subtle::ConstantTimeEq;

/// What every desktop service name starts with.
pub(crate) const SERVICE_PREFIX: &str = "desktop:";
/// The longest service name, its prefix included.
pub(crate) const MAX_SERVICE_LEN: usize = 128;
pub(crate) fn valid_service(service: &str) -> bool {
    service.starts_with(SERVICE_PREFIX)
        && service.len() > SERVICE_PREFIX.len()
        && service.len() <= MAX_SERVICE_LEN
        && service
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

const PREFIX: &str = "pbd1.";
const VERSION: u8 = 0x02;
const MAX_TEXT: usize = 8192;
const MAX_PBD1_TEXT: usize = 512;
/// The longest service name without its prefix: 120.
const MAX_SUFFIX: usize = MAX_SERVICE_LEN - SERVICE_PREFIX.len();
const _: () = assert!(
    MAX_SUFFIX <= u8::MAX as usize,
    "the suffix length is one byte"
);
const FIXED: usize = 3 + 32 + 32 + 4;

/// Deliver privately to a specific trusted controller. Possession grants view
/// access until the host stops or the trust is revoked. Never log, publish, or
/// put it in a service name. Relay credentials are deliberately absent;
/// configure each endpoint normally.
#[derive(Clone)]
pub struct Invitation {
    pub service: String,
    /// SHA-256 of the host certificate DER, pinned by the viewer.
    pub fingerprint: [u8; 32],
    token: Token,
    format: InvitationFormat,
}

/// The bearer capability. Its `Debug` is redacted and it is zeroed on drop.
#[derive(Clone)]
struct Token([u8; 32]);
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token([redacted])")
    }
}
impl Drop for Token {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // SAFETY: `byte` is a valid, aligned, exclusive reference. A
            // volatile store keeps the compiler from eliding the wipe.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Where a decoded invitation came from. Encoding always emits `Pbd1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvitationFormat {
    Pbd1,
    JsonV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvitationError {
    Empty,
    TooLarge,
    /// A pb-mapper setup string, which configures a relay, not a desktop.
    SetupString,
    Malformed,
    /// The typo check failed: a character was changed or lost.
    Checksum,
    UnsupportedVersion,
    InvalidService,
}
impl std::fmt::Display for InvitationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "the desktop invitation is empty",
            Self::TooLarge => "the desktop invitation is too large",
            Self::SetupString => {
                "this is a pb-mapper setup string; paste it in Settings, not as a desktop invitation"
            }
            Self::Malformed => "invalid desktop invitation",
            Self::Checksum => "the desktop invitation is damaged; copy it again",
            Self::UnsupportedVersion => {
                "the desktop invitation needs a newer pb-mapper; update pb-mapper"
            }
            Self::InvalidService => "the desktop invitation names an invalid service",
        })
    }
}
impl std::error::Error for InvitationError {}

impl std::fmt::Debug for Invitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invitation")
            .field("service", &self.service)
            .field("fingerprint", &self.short_fingerprint())
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// The v1 JSON invitation, accepted on input only.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1 {
    version: u32,
    service: String,
    certificate: String,
    token: String,
}

impl Invitation {
    /// A host's own invitation for `service`. `certificate` is its DER.
    pub(crate) fn new(
        service: String,
        certificate: &[u8],
        token: [u8; 32],
    ) -> Result<Self, InvitationError> {
        let suffix = service
            .strip_prefix(SERVICE_PREFIX)
            .ok_or(InvitationError::InvalidService)?;
        if !valid_service(&service) || suffix.len() > MAX_SUFFIX {
            return Err(InvitationError::InvalidService);
        }
        Ok(Self {
            service,
            fingerprint: fingerprint(certificate),
            token: Token(token),
            format: InvitationFormat::Pbd1,
        })
    }

    /// Always the compact `pbd1` form, about 115 characters for
    /// `desktop:my-computer`. A `service` changed to an invalid one encodes to
    /// text that `decode` rejects.
    pub fn encode(&self) -> String {
        let suffix = self
            .service
            .strip_prefix(SERVICE_PREFIX)
            .unwrap_or(&self.service)
            .as_bytes();
        let n = u8::try_from(suffix.len()).unwrap_or(0);
        let mut payload = Vec::with_capacity(FIXED + suffix.len());
        payload.extend_from_slice(&[VERSION, 0, n]);
        payload.extend_from_slice(suffix);
        payload.extend_from_slice(&self.fingerprint);
        payload.extend_from_slice(&self.token.0);
        let check = digest(&SHA256, &payload);
        payload.extend_from_slice(&check.as_ref()[..4]);
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(payload))
    }

    pub fn decode(text: &str) -> Result<Self, InvitationError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(InvitationError::Empty);
        }
        if text.len() > MAX_TEXT {
            return Err(InvitationError::TooLarge);
        }
        if let Some(encoded) = text.strip_prefix(PREFIX) {
            if text.len() > MAX_PBD1_TEXT {
                return Err(InvitationError::TooLarge);
            }
            let payload = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| InvitationError::Malformed)?;
            return Self::from_pbd1(&payload);
        }
        if text.starts_with('{') {
            return Self::from_v1(text);
        }
        if is_setup_string(text) {
            return Err(InvitationError::SetupString);
        }
        Err(InvitationError::Malformed)
    }

    fn from_pbd1(payload: &[u8]) -> Result<Self, InvitationError> {
        let [version, flags, n, ..] = *payload else {
            return Err(InvitationError::Malformed);
        };
        // The layout after these bytes belongs to the version, so check it first.
        if version != VERSION || flags != 0 {
            return Err(InvitationError::UnsupportedVersion);
        }
        let n = usize::from(n);
        if !(1..=MAX_SUFFIX).contains(&n) || payload.len() != FIXED + n {
            return Err(InvitationError::Malformed);
        }
        let (body, check) = payload.split_at(FIXED + n - 4);
        if !bool::from(digest(&SHA256, body).as_ref()[..4].ct_eq(check)) {
            return Err(InvitationError::Checksum);
        }
        let suffix =
            std::str::from_utf8(&body[3..3 + n]).map_err(|_| InvitationError::InvalidService)?;
        let service = format!("{SERVICE_PREFIX}{suffix}");
        if !valid_service(&service) {
            return Err(InvitationError::InvalidService);
        }
        let mut fingerprint = [0; 32];
        fingerprint.copy_from_slice(&body[3 + n..35 + n]);
        let mut token = Token([0; 32]);
        token.0.copy_from_slice(&body[35 + n..67 + n]);
        Ok(Self {
            service,
            fingerprint,
            token,
            format: InvitationFormat::Pbd1,
        })
    }

    fn from_v1(text: &str) -> Result<Self, InvitationError> {
        let value: V1 = serde_json::from_str(text).map_err(|_| InvitationError::Malformed)?;
        if value.version != 1 {
            return Err(InvitationError::UnsupportedVersion);
        }
        if !valid_service(&value.service) {
            return Err(InvitationError::InvalidService);
        }
        if value.certificate.len() > 4096 || value.token.len() != 43 {
            return Err(InvitationError::Malformed);
        }
        // Pin the decoded DER, never the base64 text, so this pins the same
        // certificate as the host's pbd1 re-encoding.
        let certificate = URL_SAFE_NO_PAD
            .decode(&value.certificate)
            .ok()
            .filter(|certificate| !certificate.is_empty())
            .ok_or(InvitationError::Malformed)?;
        let token = URL_SAFE_NO_PAD
            .decode(&value.token)
            .ok()
            .and_then(|token| <[u8; 32]>::try_from(token).ok())
            .ok_or(InvitationError::Malformed)?;
        Ok(Self {
            service: value.service,
            fingerprint: fingerprint(&certificate),
            token: Token(token),
            format: InvitationFormat::JsonV1,
        })
    }

    /// The first 8 bytes of the fingerprint, as `AB12-CD34-EF56-7890`, for a
    /// person to compare.
    pub fn short_fingerprint(&self) -> String {
        let (pairs, _) = self.fingerprint[..8].as_chunks::<2>();
        pairs
            .iter()
            .map(|[high, low]| format!("{high:02X}{low:02X}"))
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn format(&self) -> InvitationFormat {
        self.format
    }

    /// The 43-character base64url token the viewer authenticates with.
    pub(crate) fn token_text(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.token.0)
    }
}

fn fingerprint(certificate: &[u8]) -> [u8; 32] {
    let mut fingerprint = [0; 32];
    fingerprint.copy_from_slice(digest(&SHA256, certificate).as_ref());
    fingerprint
}

/// A temporary credential, or the base64 JSON setup string the app exports.
fn is_setup_string(text: &str) -> bool {
    if text.starts_with(pb_mapper_core::checksum::TEMP_CREDENTIAL_PREFIX) {
        return true;
    }
    let lenient = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    let compact: String = text.split_whitespace().collect();
    [alphabet::STANDARD, alphabet::URL_SAFE]
        .iter()
        .filter_map(|alphabet| GeneralPurpose::new(alphabet, lenient).decode(&compact).ok())
        .filter_map(|json| {
            serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&json).ok()
        })
        .any(|object| object.contains_key("serverAddress") || object.contains_key("msgHeaderKey"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostIdentity;

    #[test]
    fn pbd1_round_trip_rejects_tamper_and_setup_strings() {
        let identity = HostIdentity::generate().unwrap();
        let invitation = identity.invitation("desktop:my-computer".into()).unwrap();
        let text = invitation.encode();
        assert!(text.starts_with(PREFIX));
        assert_eq!(text.len(), 115);
        let decoded = Invitation::decode(&format!("  {text}\n")).unwrap();
        assert_eq!(decoded.service, "desktop:my-computer");
        assert_eq!(decoded.fingerprint, invitation.fingerprint);
        assert_eq!(decoded.token_text(), invitation.token_text());
        assert_eq!(decoded.format(), InvitationFormat::Pbd1);
        assert_eq!(decoded.encode(), text);
        assert_eq!(decoded.short_fingerprint().len(), 19);
        assert!(!format!("{decoded:?}").contains(&decoded.token_text()));

        // Any changed character in the service, pin or token fails the check.
        let mut payload = URL_SAFE_NO_PAD.decode(&text[PREFIX.len()..]).unwrap();
        payload[40] ^= 1;
        let tampered = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&payload));
        assert_eq!(
            Invitation::decode(&tampered).unwrap_err(),
            InvitationError::Checksum
        );
        assert_eq!(
            Invitation::decode(&format!("{text}=")).unwrap_err(),
            InvitationError::Malformed
        );
        assert_eq!(
            Invitation::decode(&format!("{}+{}", &text[..20], &text[21..])).unwrap_err(),
            InvitationError::Malformed
        );
        payload[0] = 0x03;
        assert_eq!(
            Invitation::decode(&format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&payload)))
                .unwrap_err(),
            InvitationError::UnsupportedVersion
        );

        // A pasted setup string or temporary credential is named as such.
        let setup = base64::engine::general_purpose::STANDARD.encode(
            serde_json::json!({"version": 1, "serverAddress": "relay:7666", "msgHeaderKey": "k"})
                .to_string(),
        );
        assert_eq!(
            Invitation::decode(&setup).unwrap_err(),
            InvitationError::SetupString
        );
        assert_eq!(
            Invitation::decode("pbmt1_abcdef").unwrap_err(),
            InvitationError::SetupString
        );
        assert_eq!(
            Invitation::decode(" \n").unwrap_err(),
            InvitationError::Empty
        );
        assert_eq!(
            Invitation::decode("hello").unwrap_err(),
            InvitationError::Malformed
        );

        // A v1 invitation pins the SHA-256 of its decoded certificate DER: the
        // same identity as its pbd1 re-encoding.
        let secret: serde_json::Value =
            serde_json::from_slice(&identity.encode_secret().unwrap()).unwrap();
        let v1 = serde_json::json!({
            "version": 1,
            "service": "desktop:my-computer",
            "certificate": secret["certificate"],
            "token": secret["token"],
        })
        .to_string();
        let legacy = Invitation::decode(&v1).unwrap();
        assert_eq!(legacy.format(), InvitationFormat::JsonV1);
        assert_eq!(legacy.fingerprint, invitation.fingerprint);
        assert_eq!(legacy.encode(), text);
    }
}
