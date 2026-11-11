//! Core Foundation glue for the macOS parts: property dictionaries, the
//! values in them, OS errors, and symbols of systems newer than the app's
//! oldest.

use crate::{Result, error};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use std::ffi::{CStr, c_char, c_void};

/// A dictionary of `pairs`, as the APIs that take one want it.
pub(crate) fn dictionary(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary> {
    let (keys, values): (Vec<&CFString>, Vec<&CFType>) = pairs.iter().copied().unzip();
    let typed = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // SAFETY: a dictionary of CF keys and values is an opaque dictionary.
    unsafe { CFRetained::cast_unchecked(typed) }
}

pub(crate) fn boolean(value: bool) -> &'static CFType {
    CFBoolean::new(value)
}

pub(crate) fn number(value: i64) -> CFRetained<CFNumber> {
    CFNumber::new_i64(value)
}

pub(crate) fn float(value: f64) -> CFRetained<CFNumber> {
    CFNumber::new_f64(value)
}

/// `dictionary`'s value for `key`, a CF string, as `T`.
pub(crate) fn value<T: objc2_core_foundation::ConcreteType>(
    dictionary: &CFDictionary,
    key: &CFString,
) -> Option<CFRetained<T>> {
    // SAFETY: only read through, with a CF string key; every value of a CF
    // dictionary is a CF type.
    let typed: &CFDictionary<CFString, CFType> =
        unsafe { &*(dictionary as *const CFDictionary).cast() };
    typed.get(key)?.downcast::<T>().ok()
}

/// A failure of the OS call `what`, with its status.
pub(crate) fn status(what: &str, status: i32) -> std::io::Error {
    error(format!("{what} failed (OSStatus {status})"))
}

/// `Ok` for a zero status.
pub(crate) fn check(what: &str, code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(status(what, code))
    }
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
/// `RTLD_DEFAULT`: every image loaded.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// The CF string constant `name` of a system newer than the app's oldest:
/// looked up when used, so the library loads where the symbol is missing.
pub(crate) fn symbol(name: &CStr) -> Option<&'static CFString> {
    // SAFETY: a constant's symbol is the address of a CFStringRef that
    // lives as long as the process.
    unsafe {
        let address = dlsym(RTLD_DEFAULT, name.as_ptr()).cast::<*const CFString>();
        address.as_ref()?.as_ref()
    }
}
