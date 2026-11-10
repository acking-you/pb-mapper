//! What `AuthConfig` reads from the environment, and what an override keeps it
//! from reading. Environment writes must not share a process with other
//! threads, so this harness-free executable stays single-threaded: do not start
//! a runtime or spawn threads here. Cargo runs it apart from the unit tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pb_mapper_auth::{
    AuthConfig, AuthConfigOverrides, DEFAULT_MAX_TEMP_KEY_TTL, DEFAULT_TEMP_KEY_CAPACITY,
    LegacyProtocolPolicy,
};
use tracing::{Event, Metadata, Subscriber, span};

static EVENTS: AtomicUsize = AtomicUsize::new(0);

/// Counts the events logged while it is this thread's subscriber.
struct CountEvents;

impl Subscriber for CountEvents {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, _: &Event<'_>) {
        EVENTS.fetch_add(1, Ordering::Relaxed);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

/// `build`'s configuration, and how many events it logged.
fn logged_while(build: impl FnOnce() -> AuthConfig) -> (AuthConfig, usize) {
    EVENTS.store(0, Ordering::Relaxed);
    let config = tracing::subscriber::with_default(CountEvents, build);
    (config, EVENTS.load(Ordering::Relaxed))
}

fn main() {
    // SAFETY: this executable has no test-runner thread pool or async runtime,
    // and nothing below starts a thread.
    unsafe {
        std::env::set_var("PB_MAPPER_AUTH_STATE_DIR", "state-from-environment");
        std::env::set_var("PB_MAPPER_AUTH_MAX_TEMP_KEYS", "many");
        std::env::set_var("PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS", "long");
        std::env::set_var("PB_MAPPER_LEGACY_PROTOCOL", "maybe");
    }

    // Read, each invalid value is logged and falls back: to its default, or to
    // deny for the legacy protocol.
    let (config, logged) = logged_while(AuthConfig::default);
    assert_eq!(logged, 3);
    assert_eq!(config.state_dir, PathBuf::from("state-from-environment"));
    assert_eq!(config.max_temporary_keys, DEFAULT_TEMP_KEY_CAPACITY);
    assert_eq!(config.max_temporary_key_ttl, DEFAULT_MAX_TEMP_KEY_TTL);
    assert_eq!(config.legacy_protocol, LegacyProtocolPolicy::Deny);

    // Overriding one variable leaves the others read.
    let (config, logged) = logged_while(|| {
        AuthConfig::from_env_with(AuthConfigOverrides {
            legacy_protocol: Some(LegacyProtocolPolicy::Allow),
            ..AuthConfigOverrides::default()
        })
    });
    assert_eq!(logged, 2);
    assert_eq!(config.state_dir, PathBuf::from("state-from-environment"));
    assert_eq!(config.legacy_protocol, LegacyProtocolPolicy::Allow);

    // Overridden, none is read: every override applies, and nothing is logged.
    let (config, logged) = logged_while(|| {
        AuthConfig::from_env_with(AuthConfigOverrides {
            state_dir: Some(PathBuf::from("state-from-override")),
            max_temporary_keys: Some(7),
            max_temporary_key_ttl: Some(Duration::from_secs(120)),
            legacy_protocol: Some(LegacyProtocolPolicy::Allow),
        })
    });
    assert_eq!(logged, 0);
    assert_eq!(config.state_dir, PathBuf::from("state-from-override"));
    assert_eq!(config.max_temporary_keys, 7);
    assert_eq!(config.max_temporary_key_ttl, Duration::from_secs(120));
    assert_eq!(config.legacy_protocol, LegacyProtocolPolicy::Allow);
}
