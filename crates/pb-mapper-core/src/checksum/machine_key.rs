//! The legacy machine-derived administrator key: derived from the hostname and
//! MAC addresses, and persisted for operators to reuse.

use std::io;
use std::path::Path;
use std::process::Command;

use ring::digest::{SHA256, digest};

use super::set_process_msg_header_key;
use crate::durable_file::{NewFileMode, ReplaceStep, replace_contents};

/// Fixed file path used to persist a machine-derived key for operators to reuse.
pub const MACHINE_MSG_HEADER_KEY_PATH: &str = "/var/lib/pb-mapper-server/msg_header_key";

const DERIVE_MSG_HEADER_KEY_TAG: &str = "pb-mapper-msg-header-key-v1";
const DERIVE_MSG_HEADER_KEY_CHARSET: &[u8] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Derive a stable machine-specific `MSG_HEADER_KEY` and persist it.
///
/// The derivation seed is built from normalized hostname + normalized MAC list,
/// then hashed with SHA-256.
///
/// Why SHA-256:
/// - it is deterministic for the same input;
/// - it returns exactly 32 bytes, which naturally matches the required key length.
///
/// The final key is represented with alphanumeric ASCII characters, installed as
/// this process's credential through [`set_process_msg_header_key`], and written
/// to [`MACHINE_MSG_HEADER_KEY_PATH`].
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

    if let Ok(content) = std::fs::read_to_string("/etc/hostname")
        && let Some(hostname) = normalize_non_empty(Some(content.as_str()))
    {
        return Ok(hostname);
    }

    if let Ok(output) = Command::new("hostname").output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(hostname) = normalize_non_empty(Some(stdout.as_ref())) {
            return Ok(hostname);
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
    if let Ok(mac_addresses) = get_machine_mac_addresses_from_sysfs()
        && !mac_addresses.is_empty()
    {
        return Ok(mac_addresses);
    }

    if let Ok(mac_addresses) = get_machine_mac_addresses_from_ip_link()
        && !mac_addresses.is_empty()
    {
        return Ok(mac_addresses);
    }

    if let Ok(mac_addresses) = get_machine_mac_addresses_from_ifconfig()
        && !mac_addresses.is_empty()
    {
        return Ok(mac_addresses);
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
    sort_and_dedup(&mut mac_addresses);
    Ok(mac_addresses)
}

fn get_machine_mac_addresses_from_ip_link() -> io::Result<Vec<String>> {
    let output = Command::new("ip").arg("link").output()?;
    if !output.status.success() {
        return Err(io::Error::other("`ip link` returned non-zero status"));
    }
    Ok(macs_from_ip_link(&String::from_utf8_lossy(&output.stdout)))
}

fn macs_from_ip_link(stdout: &str) -> Vec<String> {
    macs_from_listing(
        stdout,
        |line| !line.starts_with(' '),
        parse_interface_name_from_ip_link,
        "link/ether ",
    )
}

fn get_machine_mac_addresses_from_ifconfig() -> io::Result<Vec<String>> {
    let output = Command::new("ifconfig").output()?;
    if !output.status.success() {
        return Err(io::Error::other("`ifconfig` returned non-zero status"));
    }
    Ok(macs_from_ifconfig(&String::from_utf8_lossy(&output.stdout)))
}

fn macs_from_ifconfig(stdout: &str) -> Vec<String> {
    macs_from_listing(
        stdout,
        |line| !line.starts_with('\t') && !line.starts_with(' '),
        parse_interface_name_from_ifconfig,
        "ether ",
    )
}

/// The `interface:mac` entries of an interface listing, sorted and
/// deduplicated. Each header line names the interface the indented lines below
/// it describe (`None` skips it); an indented line that starts with `marker`
/// carries its MAC address as the next word.
fn macs_from_listing(
    stdout: &str,
    is_header: fn(&str) -> bool,
    interface_name: fn(&str) -> Option<String>,
    marker: &str,
) -> Vec<String> {
    let mut mac_addresses = Vec::new();
    let mut current_interface: Option<String> = None;
    for line in stdout.lines() {
        if is_header(line) {
            current_interface = interface_name(line);
            continue;
        }
        let line = line.trim_start();
        if !line.starts_with(marker) {
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
    sort_and_dedup(&mut mac_addresses);
    mac_addresses
}

fn sort_and_dedup(mac_addresses: &mut Vec<String>) {
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
    write_key_file(Path::new(MACHINE_MSG_HEADER_KEY_PATH), key)
}

/// Replaces `path` with `key` and a newline, readable by its owner alone on
/// unix from the moment it exists, creating its directory first.
fn write_key_file(path: &Path, key: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to create directory `{}`: {error}", parent.display()),
            )
        })?;
    }
    replace_contents(
        path,
        format!("{key}\n").as_bytes(),
        NewFileMode::Exactly(0o600),
    )
    .map_err(|error| match error.step {
        // Reported bare, as the `chmod` that used to follow the write was.
        ReplaceStep::SetPermissions => io::Error::from(error),
        ReplaceStep::Create
        | ReplaceStep::Write
        | ReplaceStep::Replace
        | ReplaceStep::SyncParent => io::Error::new(
            error.io().kind(),
            format!(
                "failed to write key file `{}`: {}",
                path.display(),
                error.io()
            ),
        ),
    })
}

#[cfg(test)]
mod tests {
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
        // Operators have distributed keys derived this way.
        assert_eq!(key1, "1sQAOc3e6Zhob33ZdgvQU0LWVLUfD3uW");
    }

    /// Discovery feeds the derivation, so what each listing yields is the key:
    /// loopback, link-less and zero addresses are skipped, a `@peer` suffix is
    /// dropped, and entries come back lower-case, sorted and deduplicated.
    #[test]
    fn ip_link_listing_yields_each_ethernet_interface_once() {
        use super::*;
        let listing = "\
1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000
    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000
    link/ether 52:54:00:12:34:56 brd ff:ff:ff:ff:ff:ff
    altname enp0s3
3: docker0: <NO-CARRIER,BROADCAST,MULTICAST,UP> mtu 1500 qdisc noqueue state DOWN mode DEFAULT group default
    link/ether 02:42:AC:11:00:02 brd ff:ff:ff:ff:ff:ff
4: veth1a2b@if5: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue master docker0 state UP mode DEFAULT group default
    link/ether 9a:3f:01:02:03:04 brd ff:ff:ff:ff:ff:ff link-netnsid 0
5: wg0: <POINTOPOINT,NOARP,UP,LOWER_UP> mtu 1420 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000
    link/none
6: dummy0: <BROADCAST,NOARP> mtu 1500 qdisc noop state DOWN mode DEFAULT group default qlen 1000
    link/ether 00:00:00:00:00:00 brd ff:ff:ff:ff:ff:ff
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000
    link/ether 52:54:00:12:34:56 brd ff:ff:ff:ff:ff:ff
";
        assert_eq!(
            macs_from_ip_link(listing),
            [
                "docker0:02:42:ac:11:00:02",
                "eth0:52:54:00:12:34:56",
                "veth1a2b:9a:3f:01:02:03:04",
            ]
        );
    }

    /// Both `ifconfig` dialects: BSD/macOS (tab-indented) and Linux net-tools
    /// (space-indented, blank line between interfaces).
    #[test]
    fn ifconfig_listing_yields_each_ethernet_interface_once() {
        use super::*;
        let listing = "\
lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
\toptions=1203<RXCSUM,TXCSUM,TXSTATUS,SW_TIMESTAMP>
\tinet 127.0.0.1 netmask 0xff000000
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\toptions=6463<RXCSUM,TXCSUM,TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINVERT_CSUM>
\tether a4:83:e7:12:34:56
\tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255
bridge0: flags=8822<BROADCAST,SMART,SIMPLEX,MULTICAST> mtu 1500
\tether 36:1C:2B:00:00:01
utun0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1380
\tinet6 fe80::1%utun0 prefixlen 64 scopeid 0xc
eth0: flags=4163<UP,BROADCAST,RUNNING,MULTICAST>  mtu 1500
        inet 10.0.0.5  netmask 255.255.255.0  broadcast 10.0.0.255
        ether 52:54:00:ab:cd:ef  txqueuelen 1000  (Ethernet)

lo: flags=73<UP,LOOPBACK,RUNNING>  mtu 65536
        inet 127.0.0.1  netmask 255.0.0.0
        ether 52:54:00:ff:ff:ff  txqueuelen 1000  (Local Loopback)
";
        assert_eq!(
            macs_from_ifconfig(listing),
            [
                "bridge0:36:1c:2b:00:00:01",
                "en0:a4:83:e7:12:34:56",
                "eth0:52:54:00:ab:cd:ef",
            ]
        );
    }

    /// The key file is the key and a newline (the relay's legacy migration
    /// reads it back), in a directory created for it, owner-only on unix.
    #[test]
    fn machine_key_file_holds_the_key_line_for_its_owner_alone() {
        use super::*;
        use crate::test_support::Scratch;

        let scratch = Scratch::new("machine-key");
        let path = scratch.0.join("pb-mapper-server").join("msg_header_key");
        write_key_file(&path, "first").unwrap();
        write_key_file(&path, "0123456789abcdefghijklmnopqrstuv").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "0123456789abcdefghijklmnopqrstuv\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
