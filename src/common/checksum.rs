use std::sync::LazyLock;

use rand::Rng;

use super::message::DataLenType;

pub type ChecksumType = u32;

const DEFAULT_KEY: &str = "abcdefghijklmnopqlsn123456789j01";

/// 256-bit key,must be 256/8 = 32 byte key and hashcode
pub static MSG_HEADER_KEY: LazyLock<(Vec<u8>, u32)> = LazyLock::new(|| {
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
            tracing::warn!(
                "No ENV:`{ENV_MSG_HEADER_KEY}` provided,we use default key:{DEFAULT_KEY}"
            );
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

pub type AesKeyType = [u8; 32];

pub fn gen_random_key() -> [u8; 32] {
    const CHARSET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

    let mut rng = rand::rng();
    let mut random_key: AesKeyType = [0; 32];
    (0..32).for_each(|i| {
        let idx = rng.random_range(0..CHARSET.len());
        random_key[i] = CHARSET[idx];
    });

    random_key
}

mod tests {
    #[test]
    fn test_random_checksum() {
        use super::*;
        println!("{}", gen_checksum_by_key(DEFAULT_KEY.as_bytes()));
    }
}
