//! Windows DPAPI for every store that keeps a desktop secret at rest: the
//! app's remembered grant and computers, and the helper's background grant.
use super::win32::{LocalAllocation, wide};
use crate::{Result, error};
use windows::{
    Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN,
        CryptProtectData, CryptUnprotectData,
    },
    core::PCWSTR,
};

/// Who may unlock what `protect` encrypts. Decrypting reads it from the data.
#[derive(Clone, Copy)]
pub enum Scope {
    CurrentUser,
    /// Any account on this computer, so the file needs a private ACL.
    Machine,
}

/// Encrypts `bytes`, or decrypts them when `encrypt` is false. The scope,
/// `entropy` and `description` belong to the stored data and never change:
/// data protected with other values can no longer be read. Neither `bytes`
/// nor the result may exceed `limit`.
pub fn protect(
    bytes: &[u8],
    encrypt: bool,
    scope: Scope,
    entropy: &[u8],
    description: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let too_large = || error("data exceeds its Windows protection limit");
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| too_large())?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(entropy.len()).map_err(|_| too_large())?,
        pbData: entropy.as_ptr().cast_mut(),
    };
    let description = wide(description);
    let flags = match scope {
        Scope::CurrentUser => CRYPTPROTECT_UI_FORBIDDEN,
        Scope::Machine => CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                PCWSTR(description.as_ptr()),
                Some(&entropy),
                None,
                None,
                flags,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    }
    .map_err(|_| error("Windows could not protect or unlock this data"))?;
    // Windows allocated the output for this caller to free.
    let _output = unsafe { LocalAllocation::own(output.pbData.cast()) };
    if output.pbData.is_null() || output.cbData as u64 > limit {
        return Err(error("Windows returned invalid protected data"));
    }
    Ok(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec())
}
