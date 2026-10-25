//! What the app's self-update needs from Rust: the one cryptographic check,
//! that a release manifest carries a valid Ed25519 signature by the release
//! key, through `ring`, the cryptography the rest of the workspace already
//! trusts; and on Windows, starting the process that installs the update
//! once the app has quit.

use std::ffi::{CStr, c_char};
use std::slice;

use ring::signature::{ED25519, UnparsedPublicKey};

/// A manifest larger than this is refused as malformed.
const MAX_MESSAGE: u32 = 1 << 20;
const SIGNATURE_LEN: u32 = 64;
const PUBLIC_KEY_LEN: u32 = 32;

/// Checks that `signature` is an Ed25519 signature of exactly `message` by
/// `public_key` (its 32 raw bytes).
///
/// Returns 1 when it is, 0 when it is not, and -1 for a null pointer or a
/// length that cannot be right: a key that is not 32 bytes, a signature that
/// is not 64, a message over 1 MiB.
///
/// A leaf call, like `pb_mapper_galaxy_frame_v1`: it allocates nothing the
/// caller must free, takes no lock and makes no callback.
///
/// # Safety
///
/// `message`, `signature` and `public_key` must each be valid for reads of
/// `message_len`, `signature_len` and `public_key_len` bytes for the
/// duration of the call. The caller keeps ownership of all three.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_ed25519_verify_v1(
    message: *const u8,
    message_len: u32,
    signature: *const u8,
    signature_len: u32,
    public_key: *const u8,
    public_key_len: u32,
) -> i32 {
    if message.is_null() || signature.is_null() || public_key.is_null() {
        return -1;
    }
    if message_len > MAX_MESSAGE
        || signature_len != SIGNATURE_LEN
        || public_key_len != PUBLIC_KEY_LEN
    {
        return -1;
    }
    // SAFETY: every pointer is non-null, every length is bounded, and the
    // caller guarantees each pointer is valid for its length for the call.
    let (message, signature, public_key) = unsafe {
        (
            slice::from_raw_parts(message, message_len as usize),
            slice::from_raw_parts(signature, signature_len as usize),
            slice::from_raw_parts(public_key, public_key_len as usize),
        )
    };
    match UnparsedPublicKey::new(&ED25519, public_key).verify(message, signature) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

/// Starts `powershell.exe -Command <script>` so that it outlives this
/// process, which quits right after: the Windows update's hand-off, which
/// waits for the app to exit, runs the installer and opens the app again.
///
/// Dart's `ProcessStartMode.detached` cannot do this. It starts the child
/// with no console at all, and PowerShell exits at once without running a
/// line, so no update was ever installed from inside the app. Here it gets a
/// console of its own that has no window (`CREATE_NO_WINDOW`), its own
/// process group, null standard handles, and, where allowed, leaves any job
/// the app runs in, so closing that job cannot take it down with the app.
///
/// Returns 1 when it started, 0 when it could not (or on another platform),
/// and -1 for a null or non-UTF-8 script. A leaf call: it allocates nothing
/// the caller must free.
///
/// # Safety
///
/// `script` must be null or point to a NUL-terminated string that stays
/// valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_spawn_hidden_powershell_v1(script: *const c_char) -> i32 {
    if script.is_null() {
        return -1;
    }
    // SAFETY: non-null, and the caller guarantees a NUL-terminated string.
    let Ok(script) = unsafe { CStr::from_ptr(script) }.to_str() else {
        return -1;
    };
    match spawn_hidden_powershell(script) {
        Ok(()) => 1,
        Err(error) => {
            tracing::error!("could not start the update hand-off: {error}");
            0
        }
    }
}

#[cfg(windows)]
fn spawn_hidden_powershell(script: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let spawn = |flags: u32| {
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    // A job that forbids breaking away refuses the first with access denied;
    // the second then starts inside it, which is the best that job allows.
    // Any other failure is the real one, and is reported as it is.
    const ERROR_ACCESS_DENIED: i32 = 5;
    match spawn(flags | CREATE_BREAKAWAY_FROM_JOB) {
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => spawn(flags),
        started => started,
    }
    .map(drop)
}

#[cfg(not(windows))]
fn spawn_hidden_powershell(_script: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the update hand-off is a Windows installer's",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::ptr;

    fn verify(message: &[u8], signature: &[u8], key: &[u8]) -> i32 {
        // SAFETY: every pointer comes from a live slice of that length.
        unsafe {
            pb_mapper_ed25519_verify_v1(
                message.as_ptr(),
                message.len() as u32,
                signature.as_ptr(),
                signature.len() as u32,
                key.as_ptr(),
                key.len() as u32,
            )
        }
    }

    #[test]
    fn a_signature_holds_only_for_its_exact_bytes_and_key() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("generate a key");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse the key");
        let other = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&rng)
                .expect("generate a key")
                .as_ref(),
        )
        .expect("parse the key");
        let manifest = br#"{"version":"0.9.1","assets":[]}"#;
        let signature = pair.sign(manifest);
        let key = pair.public_key().as_ref();

        assert_eq!(verify(manifest, signature.as_ref(), key), 1);

        let mut tampered = manifest.to_vec();
        tampered[13] = b'2';
        assert_eq!(verify(&tampered, signature.as_ref(), key), 0);
        assert_eq!(
            verify(manifest, signature.as_ref(), other.public_key().as_ref()),
            0
        );

        assert_eq!(verify(manifest, &signature.as_ref()[..63], key), -1);
        assert_eq!(verify(manifest, signature.as_ref(), &key[..31]), -1);
        // SAFETY: the null message is refused before anything is read.
        let null = unsafe {
            pb_mapper_ed25519_verify_v1(
                ptr::null(),
                0,
                signature.as_ref().as_ptr(),
                64,
                key.as_ptr(),
                32,
            )
        };
        assert_eq!(null, -1);
    }

    /// The PowerShell it starts runs its script, which a detached Dart
    /// child never did.
    #[cfg(windows)]
    #[test]
    fn the_hidden_powershell_runs_its_script() {
        let marker =
            std::env::temp_dir().join(format!("pb-mapper-hand-off-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = std::ffi::CString::new(format!(
            "Set-Content -LiteralPath '{}' -Value 'ran'",
            marker.display()
        ))
        .expect("no NUL in the script");
        // SAFETY: a live NUL-terminated string, and null.
        let (started, null) = unsafe {
            (
                pb_mapper_spawn_hidden_powershell_v1(script.as_ptr()),
                pb_mapper_spawn_hidden_powershell_v1(ptr::null()),
            )
        };
        assert_eq!((started, null), (1, -1));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !marker.exists() {
            assert!(std::time::Instant::now() < deadline, "the script never ran");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = std::fs::remove_file(&marker);
    }
}
