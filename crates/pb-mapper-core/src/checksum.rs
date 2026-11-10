//! Credentials and the frame length checksum they key.
//!
//! The submodules are private; every item is re-exported here, so callers name
//! it as `pb_mapper_core::checksum::…` whichever file it lives in.

mod credential;
mod machine_key;
mod process;

pub use credential::{
    ADMIN_KEY_LEN, AesKeyType, Credential, TEMP_CREDENTIAL_PREFIX, encode_temporary_credential,
    env_safe_admin_key_error, gen_random_key, is_env_safe_admin_key, parse_credential,
};
pub use machine_key::{MACHINE_MSG_HEADER_KEY_PATH, setup_machine_msg_header_key};
pub use process::{
    ENV_MSG_HEADER_KEY, get_checksum, get_msg_header_key, get_process_credential,
    process_checksum_is_ready, process_credential_configured, set_process_msg_header_key,
    valid_checksum,
};

use crate::DataLenType;

pub type ChecksumType = u32;

fn gen_checksum_by_key(key: &[u8]) -> ChecksumType {
    key.iter().fold(0u32, |hash, &byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    })
}

#[inline]
/// Compute frame checksum from payload length and an explicit header key.
pub fn get_checksum_for_key(datalen: DataLenType, key: &[u8]) -> ChecksumType {
    datalen ^ gen_checksum_by_key(key)
}

#[inline]
/// Validate a frame checksum against an explicit header key.
pub fn valid_checksum_for_key(datalen: DataLenType, checksum: ChecksumType, key: &[u8]) -> bool {
    checksum == get_checksum_for_key(datalen, key)
}

// This was missing its `#[cfg(test)]`, so it compiled into release builds.
#[cfg(test)]
mod tests {
    #[test]
    fn test_random_checksum() {
        use super::*;
        println!(
            "{}",
            gen_checksum_by_key(b"0123456789abcdefghijklmnopqrstuv")
        );
    }

    #[test]
    fn checksum_for_an_explicit_key_is_independent_of_process_state() {
        use super::*;
        let key = b"0123456789abcdefghijklmnopqrstuv";
        let datalen = 32;
        let checksum = get_checksum_for_key(datalen, key);
        assert!(valid_checksum_for_key(datalen, checksum, key));
        assert!(!valid_checksum_for_key(
            datalen,
            checksum,
            b"abcdefghijklmnopqrstuvwxyz012345"
        ));
    }
}
