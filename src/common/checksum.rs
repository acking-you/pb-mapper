use once_cell::sync::Lazy;

use super::message::DataLenType;

pub type ChecksumType = u32;

const DEFAULT_KEY: &str = "abcdefghijklmnopqlsn123456789j01";

/// 256-bit key,must be 256/8 = 32 byte key and hashcode
pub static MSG_HEADER_KEY: Lazy<(Vec<u8>, u32)> = Lazy::new(|| {
    const ENV_MSG_HEADER_KEY: &str = "MSG_HEADER_KEY";
    let key = match std::env::var(ENV_MSG_HEADER_KEY) {
        Ok(k) => {
            let key = k.as_bytes();
            if key.len() != 32 {
                tracing::warn!(
                    "`{ENV_MSG_HEADER_KEY}` must have 256 bit(32 byte)!. current input key:{k}"
                );
                std::process::exit(1);
            }
            key.to_vec()
        }
        Err(_) => {
            tracing::warn!("No ENV:`SECRET_KEY` provided,we use default key:{DEFAULT_KEY}");
            DEFAULT_KEY.as_bytes().to_vec()
        }
    };
    let hash = gen_checksum_by_key(&key);
    (key, hash)
});

fn gen_checksum_by_key(key: &[u8]) -> ChecksumType {
    key.iter().fold(0u32, |hash, &byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    })
}

#[inline]
pub fn get_checksum(datalen: DataLenType) -> ChecksumType {
    datalen ^ MSG_HEADER_KEY.1
}

#[inline]
pub fn valid_checksum(datalen: DataLenType, checksum: ChecksumType) -> bool {
    datalen == (checksum ^ MSG_HEADER_KEY.1)
}

mod tests {
    #[test]
    fn test_random_checksum() {
        use super::*;
        println!("{}", gen_checksum_by_key(DEFAULT_KEY.as_bytes()));
    }
}
