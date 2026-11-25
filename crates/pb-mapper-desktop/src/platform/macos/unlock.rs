//! Verifying a Mac login password with OpenDirectory before typing it into
//! `loginwindow`, so an incorrect password fails cleanly without triggering
//! lock-screen shake or leaking wrong keystrokes.

use objc2::{
    msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{NSError, NSString};
use std::ptr::NonNull;

/// `kODErrorCredentialsInvalid` in `<OpenDirectory/CFOpenDirectoryConstants.h>`.
const CREDENTIALS_INVALID: isize = 5000;

#[link(name = "OpenDirectory", kind = "framework")]
unsafe extern "C" {
    static kODRecordTypeUsers: &'static NSString;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    fn NSUserName() -> NonNull<NSString>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PasswordCheck {
    /// Local OpenDirectory confirmed the password matches the console owner.
    Valid,
    /// Local OpenDirectory explicitly rejected the password.
    Invalid,
    /// Not a local OpenDirectory record (for example a network account), so
    /// `loginwindow` itself must judge the keystrokes.
    Indeterminate,
}

/// Check `password` against the logged-in user's `/Local/Default` OpenDirectory
/// record without logging or retaining `password`.
pub(super) fn verify_local_password(password: &str) -> PasswordCheck {
    autoreleasepool(|_| {
        let (Some(session_cls), Some(node_cls)) =
            (AnyClass::get(c"ODSession"), AnyClass::get(c"ODNode"))
        else {
            return PasswordCheck::Indeterminate;
        };
        // SAFETY: `+[ODSession defaultSession]` takes no arguments and returns
        // the shared `ODSession` or nil.
        let session: Option<Retained<AnyObject>> =
            unsafe { msg_send![session_cls, defaultSession] };
        let Some(session) = session else {
            return PasswordCheck::Indeterminate;
        };
        let node_name = NSString::from_str("/Local/Default");
        let mut error: *mut NSError = std::ptr::null_mut();
        // SAFETY: `+[ODNode nodeWithSession:name:error:]` takes an `ODSession`,
        // an `NSString` node path, and an optional out `NSError *`.
        let node: Option<Retained<AnyObject>> = unsafe {
            msg_send![
                node_cls,
                nodeWithSession: &*session,
                name: &*node_name,
                error: &mut error,
            ]
        };
        let Some(node) = node else {
            return PasswordCheck::Indeterminate;
        };
        // SAFETY: `NSUserName()` returns an autoreleased non-null `NSString`.
        let user = unsafe { NSUserName().as_ref() };
        if user.is_empty() {
            return PasswordCheck::Indeterminate;
        }
        error = std::ptr::null_mut();
        // SAFETY: `-[ODNode recordWithRecordType:name:attributes:error:]`
        // looks up the local user record without mutating directory state.
        let record: Option<Retained<AnyObject>> = unsafe {
            msg_send![
                &*node,
                recordWithRecordType: kODRecordTypeUsers,
                name: user,
                attributes: Option::<&AnyObject>::None,
                error: &mut error,
            ]
        };
        let Some(record) = record else {
            return PasswordCheck::Indeterminate;
        };
        let secret = NSString::from_str(password);
        error = std::ptr::null_mut();
        // SAFETY: `-[ODRecord verifyPassword:error:]` checks `secret` against
        // OpenDirectory and populates `error` when returning `NO`.
        let matched: bool =
            unsafe { msg_send![&*record, verifyPassword: &*secret, error: &mut error] };
        if matched {
            return PasswordCheck::Valid;
        }
        // SAFETY: `error` is null or autoreleased for this pool.
        if unsafe { error.as_ref() }.is_some_and(|err| err.code() == CREDENTIALS_INVALID) {
            PasswordCheck::Invalid
        } else {
            PasswordCheck::Indeterminate
        }
    })
}
