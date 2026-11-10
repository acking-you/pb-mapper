//! AEAD wrap/unwrap for snapshot and WAL payloads.
use ring::aead::NONCE_LEN;

use super::super::*;
use super::random_bytes;

/// `STATE_BLOB_MAGIC`, then a fresh nonce, then `plain` sealed under
/// `admin_key` with `STATE_AAD`.
pub(crate) fn seal_blob(admin_key: &AesKeyType, plain: &[u8]) -> Result<Vec<u8>, AuthFailure> {
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, admin_key)
            .map_err(|_| AuthFailure::internal("failed to initialize state encryption key"))?,
    );
    let nonce_bytes: [u8; NONCE_LEN] = random_bytes();
    let mut output = plain.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::from(STATE_AAD),
        &mut output,
    )
    .map_err(|_| AuthFailure::internal("failed to encrypt authentication state"))?;
    let mut sealed = Vec::with_capacity(STATE_BLOB_MAGIC.len() + nonce_bytes.len() + output.len());
    sealed.extend_from_slice(STATE_BLOB_MAGIC);
    sealed.extend_from_slice(&nonce_bytes);
    sealed.extend_from_slice(&output);
    Ok(sealed)
}

pub(crate) fn open_blob(admin_key: &AesKeyType, sealed: &[u8]) -> Result<Vec<u8>, AuthFailure> {
    let invalid_header = || {
        AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
            "authentication state blob has an invalid header",
            false,
        )
    };
    if sealed.len() < STATE_BLOB_MAGIC.len() + NONCE_LEN + AES_256_GCM.tag_len() {
        return Err(invalid_header());
    }
    let Some(rest) = sealed.strip_prefix(STATE_BLOB_MAGIC.as_slice()) else {
        return Err(invalid_header());
    };
    // Cannot fail after the length check above, but this parses a file that may
    // have been truncated or corrupted, so it reports rather than panics.
    let Some((nonce_bytes, ciphertext)) = rest.split_first_chunk::<NONCE_LEN>() else {
        return Err(AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
            "authentication state blob has an invalid nonce",
            false,
        ));
    };
    let mut plain = ciphertext.to_vec();
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, admin_key).map_err(|_| {
        AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
            "failed to initialize state decryption key",
            false,
        )
    })?);
    let opened = key
        .open_in_place(
            Nonce::assume_unique_for_key(*nonce_bytes),
            Aad::from(STATE_AAD),
            &mut plain,
        )
        .map_err(|_| {
            AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                "authentication state integrity check failed",
                false,
            )
        })?;
    let len = opened.len();
    plain.truncate(len);
    Ok(plain)
}
