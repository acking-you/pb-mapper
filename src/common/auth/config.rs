//! Authentication configuration and platform state-directory defaults.
use super::*;

pub fn default_auth_state_dir() -> PathBuf {
    std::env::var_os("PB_MAPPER_AUTH_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(platform_default_auth_state_dir)
}

/// Linux systemd/Docker keep `/var/lib/pb-mapper/auth` when that path is usable
/// (root, or an already-writable service directory). Unprivileged Linux,
/// macOS, and Windows binaries need an application data directory instead.
pub(crate) fn platform_default_auth_state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("pb-mapper").join("auth")
    }
    #[cfg(target_os = "macos")]
    {
        match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("pb-mapper")
                .join("auth"),
            None => PathBuf::from("/Library/Application Support/pb-mapper/auth"),
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        linux_default_auth_state_dir(
            unix_effective_uid(),
            linux_system_auth_dir_usable(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn linux_default_auth_state_dir(
    euid: u32,
    system_dir_usable: bool,
    xdg_data_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> PathBuf {
    if euid == 0 || system_dir_usable {
        return PathBuf::from(DEFAULT_AUTH_STATE_DIR);
    }
    if let Some(xdg) = xdg_data_home
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("pb-mapper").join("auth");
    }
    if let Some(home) = home
        && !home.is_empty()
    {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("pb-mapper")
            .join("auth");
    }
    PathBuf::from(DEFAULT_AUTH_STATE_DIR)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn unix_effective_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn linux_system_auth_dir_usable() -> bool {
    let path = Path::new(DEFAULT_AUTH_STATE_DIR);
    path.is_dir() && unix_path_is_writable(path)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unix_path_is_writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe extern "C" {
        fn access(pathname: *const std::os::raw::c_char, mode: i32) -> i32;
    }
    const W_OK: i32 = 2;
    unsafe { access(c_path.as_ptr(), W_OK) == 0 }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            state_dir: default_auth_state_dir(),
            max_temporary_keys: env_usize(
                "PB_MAPPER_AUTH_MAX_TEMP_KEYS",
                DEFAULT_TEMP_KEY_CAPACITY,
                1,
                MAX_TEMP_KEY_CAPACITY,
            ),
            max_temporary_key_ttl: Duration::from_secs(env_u64(
                "PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS",
                DEFAULT_MAX_TEMP_KEY_TTL.as_secs(),
                MIN_TEMP_KEY_TTL.as_secs(),
                MAX_TEMP_KEY_TTL.as_secs(),
            )),
            legacy_protocol: legacy_protocol_from_env(),
        }
    }
}

fn legacy_protocol_from_env() -> LegacyProtocolPolicy {
    match std::env::var("PB_MAPPER_LEGACY_PROTOCOL") {
        Err(std::env::VarError::NotPresent) => LegacyProtocolPolicy::Allow,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::error!(
                event = "legacy_protocol_config_invalid",
                "PB_MAPPER_LEGACY_PROTOCOL is not UTF-8; denying legacy framing"
            );
            LegacyProtocolPolicy::Deny
        }
        Ok(value) => parse_legacy_protocol_policy(&value).unwrap_or_else(|| {
            tracing::error!(
                event = "legacy_protocol_config_invalid",
                value,
                "PB_MAPPER_LEGACY_PROTOCOL must be `allow` or `deny`; denying legacy framing"
            );
            LegacyProtocolPolicy::Deny
        }),
    }
}

pub(super) fn parse_legacy_protocol_policy(value: &str) -> Option<LegacyProtocolPolicy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allow" => Some(LegacyProtocolPolicy::Allow),
        "deny" => Some(LegacyProtocolPolicy::Deny),
        _ => None,
    }
}

fn env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    env_bounded(name, default, min, max)
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    env_bounded(name, default, min, max)
}

fn env_bounded<T>(name: &str, default: T, min: T, max: T) -> T
where
    T: std::str::FromStr + PartialOrd + Copy + fmt::Display,
{
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => default,
        Ok(raw) => match raw.parse::<T>() {
            Ok(value) if value >= min && value <= max => value,
            _ => {
                tracing::warn!(
                    event = "auth_config_value_invalid",
                    variable = name,
                    value = raw,
                    min = %min,
                    max = %max,
                    fallback = %default,
                    "invalid authentication configuration value; using the default"
                );
                default
            }
        },
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                event = "auth_config_value_invalid",
                variable = name,
                min = %min,
                max = %max,
                fallback = %default,
                "authentication configuration value is not UTF-8; using the default"
            );
            default
        }
    }
}
