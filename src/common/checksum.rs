use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use rand::RngExt;
use ring::digest::{digest, SHA256};

use super::message::DataLenType;

pub type ChecksumType = u32;

const DEFAULT_KEY: &str = "abcdefghijklmnopqlsn123456789j01";
/// Environment variable used by server/client processes to carry the 32-byte header key.
pub const ENV_MSG_HEADER_KEY: &str = "MSG_HEADER_KEY";
/// Fixed file path used to persist a machine-derived key for operators to reuse.
pub const MACHINE_MSG_HEADER_KEY_PATH: &str = "/var/lib/pb-mapper-server/msg_header_key";

const DERIVE_MSG_HEADER_KEY_TAG: &str = "pb-mapper-msg-header-key-v1";
const DERIVE_MSG_HEADER_KEY_CHARSET: &[u8] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Global key material and its checksum hash.
///
/// The key must be exactly 32 bytes (256 bits) to match AES-256-GCM requirements.
pub static MSG_HEADER_KEY: LazyLock<(Vec<u8>, u32)> = LazyLock::new(|| {
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
    std::env::set_var(ENV_MSG_HEADER_KEY, &key);
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
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    Ok(())
}

fn gen_checksum_by_key(key: &[u8]) -> ChecksumType {
    key.iter().fold(0u32, |hash, &byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    })
}

#[inline]
/// Compute frame checksum from payload length and the current header key hash.
pub fn get_checksum(datalen: DataLenType) -> ChecksumType {
    datalen ^ MSG_HEADER_KEY.1
}

#[inline]
/// Validate frame checksum generated by [`get_checksum`].
pub fn valid_checksum(datalen: DataLenType, checksum: ChecksumType) -> bool {
    datalen == (checksum ^ MSG_HEADER_KEY.1)
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
        println!("{}", gen_checksum_by_key(DEFAULT_KEY.as_bytes()));
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
}
