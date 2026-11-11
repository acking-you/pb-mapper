//! The login Keychain: one generic password per purpose, holding a random
//! key that never leaves it except into this process. The item's access
//! list names this app's signature, so another app reading it makes macOS
//! ask the owner.

use super::cf;
use crate::{Result, error};
use objc2_core_foundation::{CFData, CFRetained, CFString, CFType};
use objc2_security::{
    SecItemAdd, SecItemCopyMatching, kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass,
    kSecClassGenericPassword, kSecReturnData, kSecValueData,
};

/// `errSecItemNotFound`, `errSecDuplicateItem`.
const NOT_FOUND: i32 = -25300;
const DUPLICATE: i32 = -25299;

/// The key `service` and `account` name, 32 bytes. Missing, it is made from
/// `make` and stored when `create`, else `None`.
pub(crate) fn key(
    service: &str,
    account: &str,
    create: bool,
    make: impl FnOnce() -> Result<[u8; 32]>,
) -> Result<Option<[u8; 32]>> {
    if let Some(key) = read(service, account)? {
        return Ok(Some(key));
    }
    if !create {
        return Ok(None);
    }
    let key = make()?;
    let (service_cf, account_cf) = (CFString::from_str(service), CFString::from_str(account));
    let (label, data) = (CFString::from_str(service), CFData::from_bytes(&key));
    // SAFETY: static keys, alive for the process.
    let attributes = unsafe {
        cf::dictionary(&[
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, &service_cf),
            (kSecAttrAccount, &account_cf),
            (kSecAttrLabel, &label),
            (kSecValueData, &data),
        ])
    };
    // SAFETY: a dictionary of keychain attributes; no result asked for.
    match unsafe { SecItemAdd(&attributes, std::ptr::null_mut()) } {
        0 => Ok(Some(key)),
        // Another process stored one meanwhile: that one is the key.
        DUPLICATE => read(service, account),
        status => Err(cf::status("storing a key in the Keychain", status)),
    }
}

fn read(service: &str, account: &str) -> Result<Option<[u8; 32]>> {
    let (service_cf, account_cf) = (CFString::from_str(service), CFString::from_str(account));
    // SAFETY: static keys, alive for the process.
    let query = unsafe {
        cf::dictionary(&[
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, &service_cf),
            (kSecAttrAccount, &account_cf),
            (kSecReturnData, cf::boolean(true)),
        ])
    };
    let mut found: *const CFType = std::ptr::null();
    // SAFETY: a keychain query; `found` receives a retained CF data.
    match unsafe { SecItemCopyMatching(&query, &mut found) } {
        0 => {}
        NOT_FOUND => return Ok(None),
        status => return Err(cf::status("reading a key from the Keychain", status)),
    }
    let found = std::ptr::NonNull::new(found.cast_mut())
        .ok_or_else(|| error("the Keychain returned no key"))?;
    // SAFETY: a Copy-rule result, released when this drops.
    let found = unsafe { CFRetained::from_raw(found) };
    let data = found
        .downcast::<CFData>()
        .map_err(|_| error("the Keychain item is not a key"))?;
    let key: [u8; 32] = data
        .to_vec()
        .try_into()
        .map_err(|_| error("the Keychain key has the wrong length"))?;
    Ok(Some(key))
}
