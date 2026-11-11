//! Where the process's `tracing` events go: stdout, in the format
//! `PB_MAPPER_LOG_FORMAT` names, filtered by `RUST_LOG` (`info` when unset).
//!
//! `pb-mapper-testkit` includes this file as its own module, so the tests log
//! exactly as the binary does. It must therefore name nothing in this crate.

use std::sync::Once;

use pb_mapper_core::config::{LogFormat, PB_MAPPER_LOG_FORMAT, parse_log_format};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

fn log_format_from_env() -> LogFormat {
    std::env::var(PB_MAPPER_LOG_FORMAT)
        .ok()
        .map(|value| parse_log_format(&value))
        .unwrap_or(LogFormat::Pretty)
}

fn default_env_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .from_env_lossy()
}

pub(crate) fn init_tracing() {
    static INIT_TRACING: Once = Once::new();
    INIT_TRACING.call_once(|| {
        let result = match log_format_from_env() {
            LogFormat::Pretty => {
                let subscriber = tracing_subscriber::registry().with(
                    fmt::layer()
                        .pretty()
                        .with_writer(std::io::stdout)
                        .with_filter(default_env_filter()),
                );
                tracing::subscriber::set_global_default(subscriber)
            }
            LogFormat::Compact => {
                let subscriber = tracing_subscriber::registry().with(
                    fmt::layer()
                        .compact()
                        .with_writer(std::io::stdout)
                        .with_filter(default_env_filter()),
                );
                tracing::subscriber::set_global_default(subscriber)
            }
            LogFormat::Json => {
                let subscriber = tracing_subscriber::registry().with(
                    fmt::layer()
                        .json()
                        .flatten_event(true)
                        .with_writer(std::io::stdout)
                        .with_filter(default_env_filter()),
                );
                tracing::subscriber::set_global_default(subscriber)
            }
        };

        if let Err(e) = result {
            eprintln!("failed to initialize tracing subscriber: {e}");
        }
    });
}
