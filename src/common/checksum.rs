use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, RwLock};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngExt;
use ring::digest::{digest, SHA256};

use super::message::DataLenType;

pub type ChecksumType = u32;

/// Environment variable used by server/client processes to carry the 32-byte header key.
pub const ENV_MSG_HEADER_KEY: &str = "MSG_HEADER_KEY";
/// Fixed file path used to persist a machine-derived key for operators to reuse.
pub const MACHINE_MSG_HEADER_KEY_PATH: &str = "/var/lib/pb-mapper-server/msg_header_key";
pub const ADMIN_KEY_PATH: &str = "/var/lib/pb-mapper/auth/admin.key";
pub const TEMP_CREDENTIAL_PREFIX: &str = "pbmt1_";
pub const ADMIN_KEY_LEN: usize = 32;

/// Administrator keys are also stored in `MSG_HEADER_KEY`. `std::env::set_var`
/// panics on interior NUL, so the key must be printable ASCII with no whitespace.
pub fn is_env_safe_admin_key(bytes: &[u8]) -> bool {
    bytes.len() == ADMIN_KEY_LEN && bytes.iter().all(|byte| byte.is_ascii_graphic())
}

pub fn env_safe_admin_key_error() -> String {
    format!(
        "`{ENV_MSG_HEADER_KEY}` administrator key must be 32 printable ASCII bytes without whitespace or NUL"
    )
}

const DERIVE_MSG_HEADER_KEY_TAG: &str = "pb-mapper-msg-header-key-v1";
const DERIVE_MSG_HEADER_KEY_CHARSET: &[u8] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

struct MsgHeaderKeyState {
    credential: RwLock<Option<Credential>>,
    load_error: RwLock<Option<String>>,
    hash: AtomicU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credential {
    Admin(AesKeyType),
    Temporary { key_id: u64, key: AesKeyType },
}

impl Credential {
    pub fn key_id(&self) -> u64 {
        match self {
            Self::Admin(_) => 0,
            Self::Temporary { key_id, .. } => *key_id,
        }
    }

    pub fn key(&self) -> &AesKeyType {
        match self {
            Self::Admin(key) | Self::Temporary { key, .. } => key,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Admin(_))
    }
}

fn key_len_error(input: &str) -> String {
    format!(
        "`{ENV_MSG_HEADER_KEY}` administrator key must be exactly 32 bytes; received {} bytes",
        input.len()
    )
}

fn load_credential_from_env() -> Result<Option<Credential>, String> {
    let Some(raw) = std::env::var_os(ENV_MSG_HEADER_KEY) else {
        return Ok(None);
    };
    let raw = raw
        .into_string()
        .map_err(|_| format!("`{ENV_MSG_HEADER_KEY}` must contain valid UTF-8 credential text"))?;
    parse_credential(raw.trim()).map(Some)
}

fn update_runtime_credential(credential: Option<Credential>) {
    let hash = credential
        .as_ref()
        .map(|credential| gen_checksum_by_key(credential.key()))
        .unwrap_or_default();
    let mut guard = MSG_HEADER_KEY_STATE
        .credential
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = credential;
    *MSG_HEADER_KEY_STATE
        .load_error
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    MSG_HEADER_KEY_STATE.hash.store(hash, Ordering::Release);
}

/// Runtime key material and checksum hash.
///
/// This state is mutable so FFI/UI can update `MSG_HEADER_KEY` at runtime
/// without restarting the process.
static MSG_HEADER_KEY_STATE: LazyLock<MsgHeaderKeyState> = LazyLock::new(|| {
    let (credential, load_error) = match load_credential_from_env() {
        Ok(credential) => (credential, None),
        Err(error) => {
            tracing::error!(reason = "credential_invalid", %error, "invalid MSG_HEADER_KEY");
            (None, Some(error))
        }
    };
    let hash = credential
        .as_ref()
        .map(|credential| gen_checksum_by_key(credential.key()))
        .unwrap_or_default();
    MsgHeaderKeyState {
        credential: RwLock::new(credential),
        load_error: RwLock::new(load_error),
        hash: AtomicU32::new(hash),
    }
});

/// Return the configured process credential, failing closed when none exists.
pub fn get_process_credential() -> Result<Credential, String> {
    if let Some(error) = MSG_HEADER_KEY_STATE
        .load_error
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Err(error);
    }
    MSG_HEADER_KEY_STATE
        .credential
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .ok_or_else(|| {
            format!(
                "`{ENV_MSG_HEADER_KEY}` is required; no insecure default credential is available"
            )
        })
}

/// Get current message header key bytes.
pub fn get_msg_header_key() -> Result<Vec<u8>, String> {
    get_process_credential().map(|credential| credential.key().to_vec())
}

/// Set process `MSG_HEADER_KEY` and update runtime checksum/key state.
///
/// - `Some(non-empty)` => validate an admin or temporary credential and apply it immediately.
/// - `None` or empty => remove the credential. Subsequent network operations fail closed.
pub fn set_process_msg_header_key(msg_header_key: Option<&str>) -> Result<(), String> {
    let normalized = msg_header_key.map(str::trim).unwrap_or("");
    if normalized.is_empty() {
        std::env::remove_var(ENV_MSG_HEADER_KEY);
        update_runtime_credential(None);
        return Ok(());
    }

    let credential = parse_credential(normalized)?;

    std::env::set_var(ENV_MSG_HEADER_KEY, normalized);
    update_runtime_credential(Some(credential));
    Ok(())
}

pub fn parse_credential(raw: &str) -> Result<Credential, String> {
    if let Some(encoded) = raw.strip_prefix(TEMP_CREDENTIAL_PREFIX) {
        let payload = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "temporary credential is not valid base64url".to_string())?;
        if payload.len() != 45 {
            return Err(format!(
                "temporary credential payload must be 45 bytes, got {}",
                payload.len()
            ));
        }
        if payload[0] != 1 {
            return Err(format!(
                "unsupported temporary credential version {}",
                payload[0]
            ));
        }
        let expected = digest(&SHA256, &payload[..41]);
        if expected.as_ref()[..4] != payload[41..45] {
            return Err("temporary credential checksum mismatch".to_string());
        }
        let key_id = u64::from_be_bytes(payload[1..9].try_into().expect("fixed key id width"));
        if key_id == 0 {
            return Err("temporary credential key id must not be zero".to_string());
        }
        let key = payload[9..41]
            .try_into()
            .expect("fixed temporary key width");
        return Ok(Credential::Temporary { key_id, key });
    }

    let bytes = raw.as_bytes();
    if bytes.len() != ADMIN_KEY_LEN {
        return Err(key_len_error(raw));
    }
    if !is_env_safe_admin_key(bytes) {
        return Err(env_safe_admin_key_error());
    }
    Ok(Credential::Admin(
        bytes.try_into().expect("validated admin key width"),
    ))
}

pub fn encode_temporary_credential(key_id: u64, key: &AesKeyType) -> String {
    let mut payload = Vec::with_capacity(45);
    payload.push(1);
    payload.extend_from_slice(&key_id.to_be_bytes());
    payload.extend_from_slice(key);
    let checksum = digest(&SHA256, &payload);
    payload.extend_from_slice(&checksum.as_ref()[..4]);
    format!(
        "{TEMP_CREDENTIAL_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(payload)
    )
}

/// Derive a stable machine-specific `MSG_HEADER_KEY` and persist it.
///
/// The derivation seed is built from normalized hostname + normalized MAC list,
/// then hashed with SHA-256.
///
/// Why SHA-256:
/// - it is deterministic for the same input;
/// - it returns exactly 32 bytes, which naturally matches the required key length.
///
/// The final key is represented with alphanumeric ASCII characters and written to
/// [`MACHINE_MSG_HEADER_KEY_PATH`], and also injected into `MSG_HEADER_KEY` env
/// for the current process.
pub fn setup_machine_msg_header_key() -> io::Result<String> {
    let hostname = get_machine_hostname()?;
    let mac_addresses = get_machine_mac_addresses()?;
    let key = derive_msg_header_key(&hostname, &mac_addresses);
    set_process_msg_header_key(Some(&key))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    write_machine_msg_header_key(&key)?;
    Ok(key)
}

fn get_machine_hostname() -> io::Result<String> {
    if let Some(hostname) = normalize_non_empty(std::env::var("HOSTNAME").ok().as_deref()) {
        return Ok(hostname);
    }

    if let Ok(content) = std::fs::read_to_string("/etc/hostname") {
        if let Some(hostname) = normalize_non_empty(Some(content.as_str())) {
            return Ok(hostname);
        }
    }

    if let Ok(output) = Command::new("hostname").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(hostname) = normalize_non_empty(Some(stdout.as_ref())) {
                return Ok(hostname);
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "failed to get hostname from HOSTNAME env, /etc/hostname or hostname command",
    ))
}

fn normalize_non_empty(input: Option<&str>) -> Option<String> {
    input.map(str::trim).and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(value.to_ascii_lowercase())
        }
    })
}

fn get_machine_mac_addresses() -> io::Result<Vec<String>> {
    if let Ok(mac_addresses) = get_machine_mac_addresses_from_sysfs() {
        if !mac_addresses.is_empty() {
            return Ok(mac_addresses);
        }
    }

    if let Ok(mac_addresses) = get_machine_mac_addresses_from_ip_link() {
        if !mac_addresses.is_empty() {
            return Ok(mac_addresses);
        }
    }

    if let Ok(mac_addresses) = get_machine_mac_addresses_from_ifconfig() {
        if !mac_addresses.is_empty() {
            return Ok(mac_addresses);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no valid MAC address found from /sys/class/net, `ip link` or `ifconfig`",
    ))
}

fn get_machine_mac_addresses_from_sysfs() -> io::Result<Vec<String>> {
    let mut mac_addresses = Vec::new();
    for entry in std::fs::read_dir("/sys/class/net")? {
        let entry = entry?;
        let interface = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        if interface == "lo" {
            continue;
        }
        let address_path = entry.path().join("address");
        let mac = match std::fs::read_to_string(address_path) {
            Ok(mac) => match normalize_mac_address(&mac) {
                Some(mac) => mac,
                None => continue,
            },
            Err(_) => continue,
        };
        mac_addresses.push(format!("{interface}:{mac}"));
    }
    normalize_and_validate_mac_entries(&mut mac_addresses);
    Ok(mac_addresses)
}

fn get_machine_mac_addresses_from_ip_link() -> io::Result<Vec<String>> {
    let output = Command::new("ip").arg("link").output()?;
    if !output.status.success() {
        return Err(io::Error::other("`ip link` returned non-zero status"));
    }
    let mut mac_addresses = Vec::new();
    let mut current_interface: Option<String> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.starts_with(' ') {
            current_interface = parse_interface_name_from_ip_link(line);
            continue;
        }
        let line = line.trim_start();
        if !line.starts_with("link/ether ") {
            continue;
        }
        let Some(interface) = current_interface.as_ref() else {
            continue;
        };
        let Some(raw_mac) = line.split_whitespace().nth(1) else {
            continue;
        };
        let Some(mac) = normalize_mac_address(raw_mac) else {
            continue;
        };
        mac_addresses.push(format!("{interface}:{mac}"));
    }
    normalize_and_validate_mac_entries(&mut mac_addresses);
    Ok(mac_addresses)
}

fn get_machine_mac_addresses_from_ifconfig() -> io::Result<Vec<String>> {
    let output = Command::new("ifconfig").output()?;
    if !output.status.success() {
        return Err(io::Error::other("`ifconfig` returned non-zero status"));
    }
    let mut mac_addresses = Vec::new();
    let mut current_interface: Option<String> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.starts_with('\t') && !line.starts_with(' ') {
            current_interface = parse_interface_name_from_ifconfig(line);
            continue;
        }
        let line = line.trim_start();
        if !line.starts_with("ether ") {
            continue;
        }
        let Some(interface) = current_interface.as_ref() else {
            continue;
        };
        let Some(raw_mac) = line.split_whitespace().nth(1) else {
            continue;
        };
        let Some(mac) = normalize_mac_address(raw_mac) else {
            continue;
        };
        mac_addresses.push(format!("{interface}:{mac}"));
    }
    normalize_and_validate_mac_entries(&mut mac_addresses);
    Ok(mac_addresses)
}

fn normalize_and_validate_mac_entries(mac_addresses: &mut Vec<String>) {
    mac_addresses.sort();
    mac_addresses.dedup();
}

fn parse_interface_name_from_ip_link(line: &str) -> Option<String> {
    let mut parts = line.splitn(3, ':');
    let _ = parts.next()?;
    let name = parts.next()?.trim();
    let name = name.split('@').next()?.trim();
    if name.is_empty() || name == "lo" {
        return None;
    }
    Some(name.to_string())
}

fn parse_interface_name_from_ifconfig(line: &str) -> Option<String> {
    let name = line.split(':').next()?.trim();
    if name.is_empty() || name == "lo" || name == "lo0" {
        return None;
    }
    Some(name.to_string())
}

fn normalize_mac_address(mac: &str) -> Option<String> {
    let mac = mac.trim().to_ascii_lowercase();
    if mac.len() != 17 || mac == "00:00:00:00:00:00" {
        return None;
    }
    for (index, ch) in mac.char_indices() {
        if [2usize, 5, 8, 11, 14].contains(&index) {
            if ch != ':' {
                return None;
            }
        } else if !ch.is_ascii_hexdigit() {
            return None;
        }
    }
    Some(mac)
}

fn derive_msg_header_key(hostname: &str, mac_addresses: &[String]) -> String {
    let mut normalized_mac_addresses = mac_addresses
        .iter()
        .map(|address| address.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized_mac_addresses.sort();
    normalized_mac_addresses.dedup();

    let seed = format!(
        "{DERIVE_MSG_HEADER_KEY_TAG}|{}|{}",
        hostname.trim().to_ascii_lowercase(),
        normalized_mac_addresses.join("|")
    );

    // SHA-256 digest is always 32 bytes, so the downstream key length is fixed at 32.
    let digest = digest(&SHA256, seed.as_bytes());

    // Map each digest byte into an alphanumeric character to keep the key
    // readable and shell-friendly when users copy it between server/client tools.
    digest
        .as_ref()
        .iter()
        .map(|byte| {
            DERIVE_MSG_HEADER_KEY_CHARSET[(*byte as usize) % DERIVE_MSG_HEADER_KEY_CHARSET.len()]
                as char
        })
        .collect()
}

fn write_machine_msg_header_key(key: &str) -> io::Result<()> {
    let path = Path::new(MACHINE_MSG_HEADER_KEY_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to create directory `{}`: {error}", parent.display()),
            )
        })?;
    }
    std::fs::write(path, format!("{key}\n")).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to write key file `{}`: {error}", path.display()),
        )
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

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
/// Compute frame checksum from payload length and the current header key hash.
pub fn get_checksum(datalen: DataLenType) -> ChecksumType {
    datalen ^ MSG_HEADER_KEY_STATE.hash.load(Ordering::Acquire)
}

#[inline]
/// Validate a frame checksum against an explicit header key.
pub fn valid_checksum_for_key(datalen: DataLenType, checksum: ChecksumType, key: &[u8]) -> bool {
    checksum == get_checksum_for_key(datalen, key)
}

#[inline]
/// Validate frame checksum generated by [`get_checksum`].
pub fn valid_checksum(datalen: DataLenType, checksum: ChecksumType) -> bool {
    datalen == (checksum ^ MSG_HEADER_KEY_STATE.hash.load(Ordering::Acquire))
}

pub type AesKeyType = [u8; 32];

/// Generate a random printable 32-byte key.
///
/// This helper is used when a transient key is preferred over deterministic
/// machine-derived key material.
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

    #[test]
    fn env_safe_admin_key_rejects_nul_and_accepts_printable_ascii() {
        use super::*;
        assert!(is_env_safe_admin_key(b"0123456789abcdefghijklmnopqrstuv"));
        let mut with_nul = *b"0123456789abcdefghijklmnopqrstuv";
        with_nul[8] = 0;
        assert!(!is_env_safe_admin_key(&with_nul));
        assert!(!is_env_safe_admin_key(b"short"));
    }

    #[test]
    fn administrator_credentials_reject_nul_and_whitespace() {
        use super::*;
        let mut with_nul = *b"0123456789abcdefghijklmnopqrstuv";
        with_nul[8] = 0;
        assert!(parse_credential(std::str::from_utf8(&with_nul).unwrap()).is_err());
        assert!(parse_credential("0123456789abcdefghijklmnopq rstuv").is_err());
    }

    #[test]
    fn test_derive_msg_header_key_is_stable() {
        use super::*;
        let mac_addresses = vec![
            "eth0:52:54:00:12:34:56".to_string(),
            "ens3:02:42:ac:11:00:02".to_string(),
        ];
        let key1 = derive_msg_header_key("DemoHost", &mac_addresses);
        let key2 = derive_msg_header_key("demohost", &mac_addresses);
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 32);
        assert!(key1.chars().all(|ch| ch.is_ascii_alphanumeric()));
    }

    #[test]
    fn temporary_credential_round_trip_and_checksum() {
        use super::*;

        let key = [7_u8; 32];
        let encoded = encode_temporary_credential(0x0000_0007_0000_002a, &key);
        assert_eq!(
            parse_credential(&encoded).unwrap(),
            Credential::Temporary {
                key_id: 0x0000_0007_0000_002a,
                key
            }
        );

        let mut corrupted = encoded.into_bytes();
        let last = corrupted.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert!(parse_credential(std::str::from_utf8(&corrupted).unwrap()).is_err());
    }
}
