use once_cell::sync::Lazy;

use super::message::DataLenType;

pub type ChecksumType = u32;

const DEFAULT_KEY: &str = "abcdefghijklmnopqlsn";

pub static CHECKSUM_KEY: Lazy<ChecksumType> = Lazy::new(|| match std::env::var("CHECKSUM_KEY") {
    Ok(checksum) => gen_checksum_by_key(checksum.as_bytes()),
    Err(_) => {
        tracing::info!(
            "env:CHECKSUM_KEY not set,we will generate `checksum` by default key ({})",
            DEFAULT_KEY
        );
        gen_checksum_by_key(DEFAULT_KEY.as_bytes())
    }
});

pub fn gen_checksum_by_key(key: &[u8]) -> ChecksumType {
    key.iter().fold(0u32, |hash, &byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    })
}

#[inline]
pub fn get_checksum(datalen: DataLenType) -> ChecksumType {
    datalen ^ *CHECKSUM_KEY
}

#[inline]
pub fn valid_checksum(datalen: DataLenType, checksum: ChecksumType) -> bool {
    datalen == (checksum ^ *CHECKSUM_KEY)
}

mod tests {
    #[test]
    fn test_random_checksum() {
        use super::*;
        println!("{}", gen_checksum_by_key(DEFAULT_KEY.as_bytes()));
    }
}
