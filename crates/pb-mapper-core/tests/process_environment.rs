//! Environment writes must not share a process with DNS or other reader threads.
//! This harness-free executable stays single-threaded: do not start a runtime or
//! spawn threads here. Cargo runs it separately from the parallel unit tests.

use std::time::Duration;

use pb_mapper_core::checksum::{
    get_checksum, get_process_credential, set_process_msg_header_key, valid_checksum,
};
use pb_mapper_core::config::{
    PB_MAPPER_KEEP_ALIVE, PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX,
    PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL,
    keep_alive_from_env, registration_reject_backoff, server_lease_sweep_interval,
};

fn main() -> Result<(), String> {
    // SAFETY: this executable has no test-runner thread pool or async runtime,
    // and all the functions below run synchronously without starting threads.
    unsafe {
        std::env::remove_var(PB_MAPPER_KEEP_ALIVE);
        assert!(!keep_alive_from_env());
        for (value, expected) in [("ON", true), ("OFF", false), (" true ", true)] {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, value);
            assert_eq!(keep_alive_from_env(), expected, "must re-read {value}");
        }

        for name in [
            PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN,
            PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX,
            PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL,
        ] {
            std::env::remove_var(name);
        }
        let defaults = registration_reject_backoff();
        let sweep_default = server_lease_sweep_interval();
        assert!(!defaults.0.is_zero() && defaults.1 >= defaults.0);
        assert!(!sweep_default.is_zero());

        for (min, max, expected) in [
            ("30s", "2m", (30, 120)),
            ("0s", "0s", (defaults.0.as_secs(), defaults.1.as_secs())),
            ("0ms", "3m", (defaults.0.as_secs(), 180)),
            ("1m", "1s", (60, 60)),
        ] {
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, min);
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX, max);
            assert_eq!(
                registration_reject_backoff(),
                (
                    Duration::from_secs(expected.0),
                    Duration::from_secs(expected.1)
                )
            );
        }
        std::env::set_var(PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL, "0s");
        assert_eq!(server_lease_sweep_interval(), sweep_default);
    }

    // The public setter also mirrors its value into the environment. Exercise
    // it here, isolated from every resolver, as well as taking the state lock.
    let _guard = pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
    set_process_msg_header_key(Some("0123456789abcdefghijklmnopqrstuv"))?;
    let checksum = get_checksum(32);
    assert!(valid_checksum(32, checksum));
    set_process_msg_header_key(None)?;
    assert!(!valid_checksum(32, checksum));
    assert!(!valid_checksum(32, 32));
    assert!(get_process_credential().is_err());
    Ok(())
}
