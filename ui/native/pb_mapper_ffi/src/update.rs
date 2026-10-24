//! The one cryptographic check the app's self-update needs from Rust: that a
//! release manifest carries a valid Ed25519 signature by the release key. It
//! goes through `ring`, the cryptography the rest of the workspace already
//! trusts, rather than a second implementation on the Dart side.

use std::slice;

use ring::signature::{ED25519, UnparsedPublicKey};

/// A manifest larger than this is refused as malformed.
const MAX_MESSAGE: u32 = 1 << 20;
const SIGNATURE_LEN: u32 = 64;
const PUBLIC_KEY_LEN: u32 = 32;

/// Checks that `signature` is an Ed25519 signature of exactly `message` by
/// `public_key` (its 32 raw bytes).
///
/// Returns 1 when it is, 0 when it is not, and -1 for a null pointer or a
/// length that cannot be right: a key that is not 32 bytes, a signature that
/// is not 64, a message over 1 MiB.
///
/// A leaf call, like `pb_mapper_galaxy_frame_v1`: it allocates nothing the
/// caller must free, takes no lock and makes no callback.
///
/// # Safety
///
/// `message`, `signature` and `public_key` must each be valid for reads of
/// `message_len`, `signature_len` and `public_key_len` bytes for the
/// duration of the call. The caller keeps ownership of all three.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_ed25519_verify_v1(
    message: *const u8,
    message_len: u32,
    signature: *const u8,
    signature_len: u32,
    public_key: *const u8,
    public_key_len: u32,
) -> i32 {
    if message.is_null() || signature.is_null() || public_key.is_null() {
        return -1;
    }
    if message_len > MAX_MESSAGE
        || signature_len != SIGNATURE_LEN
        || public_key_len != PUBLIC_KEY_LEN
    {
        return -1;
    }
    // SAFETY: every pointer is non-null, every length is bounded, and the
    // caller guarantees each pointer is valid for its length for the call.
    let (message, signature, public_key) = unsafe {
        (
            slice::from_raw_parts(message, message_len as usize),
            slice::from_raw_parts(signature, signature_len as usize),
            slice::from_raw_parts(public_key, public_key_len as usize),
        )
    };
    match UnparsedPublicKey::new(&ED25519, public_key).verify(message, signature) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::ptr;

    fn verify(message: &[u8], signature: &[u8], key: &[u8]) -> i32 {
        // SAFETY: every pointer comes from a live slice of that length.
        unsafe {
            pb_mapper_ed25519_verify_v1(
                message.as_ptr(),
                message.len() as u32,
                signature.as_ptr(),
                signature.len() as u32,
                key.as_ptr(),
                key.len() as u32,
            )
        }
    }

    #[test]
    fn a_signature_holds_only_for_its_exact_bytes_and_key() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("generate a key");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse the key");
        let other = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&rng)
                .expect("generate a key")
                .as_ref(),
        )
        .expect("parse the key");
        let manifest = br#"{"version":"0.9.1","assets":[]}"#;
        let signature = pair.sign(manifest);
        let key = pair.public_key().as_ref();

        assert_eq!(verify(manifest, signature.as_ref(), key), 1);

        let mut tampered = manifest.to_vec();
        tampered[13] = b'2';
        assert_eq!(verify(&tampered, signature.as_ref(), key), 0);
        assert_eq!(
            verify(manifest, signature.as_ref(), other.public_key().as_ref()),
            0
        );

        assert_eq!(verify(manifest, &signature.as_ref()[..63], key), -1);
        assert_eq!(verify(manifest, signature.as_ref(), &key[..31]), -1);
        // SAFETY: the null message is refused before anything is read.
        let null = unsafe {
            pb_mapper_ed25519_verify_v1(
                ptr::null(),
                0,
                signature.as_ref().as_ptr(),
                64,
                key.as_ptr(),
                32,
            )
        };
        assert_eq!(null, -1);
    }
}
