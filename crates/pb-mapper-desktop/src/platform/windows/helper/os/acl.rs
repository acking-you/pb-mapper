//! The protected installation: its root, the owner and DACL every file in
//! it must have, and the descriptors new files and pipes are created with.
use super::wide;
use crate::{Result, error, platform::windows::win32::LocalAllocation};
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        Security::Authorization::*, Security::*, Storage::FileSystem::*,
        System::Com::CoTaskMemFree, UI::Shell::*,
    },
    core::PCWSTR,
};
#[cfg(feature = "windows-helper")]
use {
    std::{fs::File, io::Write, os::windows::io::FromRawHandle},
    windows::Win32::Foundation::GENERIC_WRITE,
};

#[cfg(feature = "windows-helper")]
pub struct Descriptor {
    attributes: SECURITY_ATTRIBUTES,
    /// The descriptor `attributes` points to.
    _descriptor: LocalAllocation,
}
#[cfg(feature = "windows-helper")]
impl Descriptor {
    pub fn new(sddl: &str) -> Result<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide(sddl).as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(error)?;
        Ok(Self {
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
            _descriptor: unsafe { LocalAllocation::own(descriptor.0) },
        })
    }
    pub fn attributes(&self) -> *mut SECURITY_ATTRIBUTES {
        (&self.attributes as *const SECURITY_ATTRIBUTES).cast_mut()
    }
}

/// `path`, created or opened as `disposition` says, for writing. A file it
/// creates has `descriptor`'s owner and DACL from its first instant: set
/// afterwards, they would leave an owner a WRITE_DAC window.
#[cfg(feature = "windows-helper")]
pub fn create_protected(
    path: &Path,
    descriptor: &Descriptor,
    disposition: FILE_CREATION_DISPOSITION,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
) -> Result<File> {
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide(path).as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(descriptor.attributes()),
            disposition,
            flags,
            None,
        )
    }
    .map_err(error)?;
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

/// Writes `bytes` to the new file `temporary`, created as
/// [`create_protected`] creates it, syncs it, and moves it over `target`
/// write-through. On failure the caller removes `temporary`.
#[cfg(feature = "windows-helper")]
pub fn replace_protected(
    temporary: &Path,
    target: &Path,
    descriptor: &Descriptor,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
    bytes: &[u8],
) -> Result<()> {
    let mut file = create_protected(temporary, descriptor, CREATE_NEW, flags)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    unsafe {
        MoveFileExW(
            PCWSTR(wide(temporary).as_ptr()),
            PCWSTR(wide(target).as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(error)
}

pub fn install_root() -> Result<PathBuf> {
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }
        .map_err(error)?;
    let text = unsafe { path.to_string() }.map_err(error);
    unsafe {
        CoTaskMemFree(Some(path.0.cast()));
    }
    Ok(PathBuf::from(text?).join("pb-mapper-desktop"))
}
pub fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy()),
        _ => false,
    }
}

pub fn protected_file(path: &Path) -> Result<()> {
    protected_path(path)?;
    if !path.is_file() {
        return Err(error("expected a protected installation file"));
    }
    Ok(())
}
pub fn protected_path(path: &Path) -> Result<()> {
    let root = install_root()?;
    if !contained_path(&root, path) {
        return Err(error("helper path is outside its protected installation"));
    }
    let mut current = path;
    loop {
        let attributes = unsafe { GetFileAttributesW(PCWSTR(wide(current).as_ptr())) };
        if attributes == INVALID_FILE_ATTRIBUTES || attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        {
            return Err(error(
                "reparse points are forbidden in the helper installation",
            ));
        }
        check_acl(current, WRITE_ACCESS)?;
        if current == root {
            break;
        }
        current = current
            .parent()
            .ok_or_else(|| error("invalid installation root"))?;
    }
    Ok(())
}
fn contained_path(root: &Path, path: &Path) -> bool {
    path.is_absolute()
        && path.starts_with(root)
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}
fn trusted_sid(sid: PSID) -> bool {
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid).as_bool()
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool()
    }
}
/// WRITE_DATA, APPEND_DATA, WRITE_EA, DELETE_CHILD, WRITE_ATTRIBUTES, DELETE,
/// WRITE_DAC, WRITE_OWNER, GENERIC_WRITE and GENERIC_ALL.
const WRITE_ACCESS: u32 = 0x500d0156;
/// `path` must be owned by SYSTEM or administrators, and grant nobody else
/// any of the `forbidden` access bits.
pub fn check_acl(path: &Path, forbidden: u32) -> Result<()> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut owner = PSID::default();
    let mut acl = std::ptr::null_mut();
    unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide(path).as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
    }
    .ok()
    .map_err(error)?;
    // `owner` and `acl` point into the descriptor, freed after the check.
    let _descriptor = unsafe { LocalAllocation::own(descriptor.0) };
    check_descriptor(owner, acl, forbidden)
}
/// The ACE types a protected DACL may hold (`Win32_System_SystemServices`,
/// a feature this crate does not otherwise need).
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
fn check_descriptor(owner: PSID, acl: *const ACL, forbidden: u32) -> Result<()> {
    // Descriptors obtained from Windows are validated before reaching this
    // routine. Deny entries cannot grant access, so they are harmless.
    (|| {
        if !trusted_sid(owner) || acl.is_null() {
            return Err(error("helper installation has an unsafe owner or DACL"));
        }
        for index in 0..unsafe { (*acl).AceCount } {
            let mut ace = std::ptr::null_mut();
            unsafe { GetAce(acl, u32::from(index), &mut ace) }.map_err(error)?;
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            if header.AceType == ACCESS_ALLOWED_ACE_TYPE {
                let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
                if allowed.Mask & forbidden != 0 && !trusted_sid(sid) {
                    return Err(error(
                        "helper files grant an unprivileged identity forbidden access",
                    ));
                }
            } else if header.AceType != ACCESS_DENIED_ACE_TYPE {
                return Err(error("unsupported installation ACL"));
            }
        }
        Ok(())
    })()
}

#[cfg(all(test, feature = "windows-helper"))]
mod tests {
    use super::*;
    fn validate(sddl: &str, forbidden: u32) -> Result<()> {
        let descriptor = Descriptor::new(sddl)?;
        let raw = PSECURITY_DESCRIPTOR(descriptor.attributes.lpSecurityDescriptor);
        let mut owner = PSID::default();
        let mut acl = std::ptr::null_mut();
        let mut defaulted = windows::core::BOOL(0);
        let mut present = windows::core::BOOL(0);
        unsafe {
            GetSecurityDescriptorOwner(raw, &mut owner, &mut defaulted).map_err(error)?;
            GetSecurityDescriptorDacl(raw, &mut present, &mut acl, &mut defaulted)
                .map_err(error)?;
        }
        check_descriptor(owner, acl, forbidden)
    }
    #[test]
    fn protected_acl_rejects_forbidden_access_and_untrusted_owner() {
        let readable = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;BU)";
        assert!(validate(readable, WRITE_ACCESS).is_ok());
        assert!(
            validate(
                "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x40;;;BU)",
                WRITE_ACCESS
            )
            .is_err()
        );
        assert!(validate("O:BUG:BUD:P(A;;GRGX;;;BU)", WRITE_ACCESS).is_err());
        // A background grant may not even be read by anyone else.
        assert!(validate("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)", u32::MAX).is_ok());
        assert!(validate(readable, u32::MAX).is_err());
        let root = Path::new(r"C:\Program Files\pb-mapper-desktop");
        assert!(contained_path(root, &root.join(r"release-safe\helper.exe")));
        assert!(!contained_path(root, &root.join(r"..\outside.exe")));
    }
}
