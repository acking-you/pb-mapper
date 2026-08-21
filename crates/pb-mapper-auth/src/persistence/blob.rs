//! AEAD wrap/unwrap for snapshot and WAL payloads.
use super::super::*;

pub(crate) fn seal_blob(admin_key: &AesKeyType, plain: &[u8]) -> Result<Vec<u8>, AuthFailure> {
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, admin_key)
            .map_err(|_| AuthFailure::internal("failed to initialize state encryption key"))?,
    );
    let mut nonce_bytes = [0_u8; 12];
    let mut rng = rand::rng();
    for byte in &mut nonce_bytes {
        *byte = rng.random();
    }
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
    if sealed.len() < STATE_BLOB_MAGIC.len() + 12 + AES_256_GCM.tag_len()
        || &sealed[..STATE_BLOB_MAGIC.len()] != STATE_BLOB_MAGIC
    {
        return Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "authentication state blob has an invalid header",
            false,
        ));
    }
    let nonce_start = STATE_BLOB_MAGIC.len();
    let nonce_end = nonce_start + 12;
    let nonce_bytes: [u8; 12] = sealed[nonce_start..nonce_end]
        .try_into()
        .expect("validated nonce width");
    let mut plain = sealed[nonce_end..].to_vec();
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, admin_key).map_err(|_| {
        AuthFailure::new(
            "temporary_key_store_unavailable",
            "failed to initialize state decryption key",
            false,
        )
    })?);
    let opened = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(STATE_AAD),
            &mut plain,
        )
        .map_err(|_| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                "authentication state integrity check failed",
                false,
            )
        })?;
    let len = opened.len();
    plain.truncate(len);
    Ok(plain)
}
