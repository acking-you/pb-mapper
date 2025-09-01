use rinf::RustSignal;
use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{Event, Level, Subscriber, field::Field};
use tracing_subscriber::{
    field,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    registry::LookupSpan,
};

use crate::signals::LogMessage;

pub struct FlutterLogCollector;

impl<S, N> FormatEvent<S, N> for FlutterLogCollector
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        // Get the current timestamp
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Get the log level
        let level = match *event.metadata().level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };

        // Collect all fields into a message
        pub struct StringVisitor<'a> {
            string: &'a mut String,
        }

        impl<'a> field::Visit for StringVisitor<'a> {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                write!(self.string, "{} = {:?}; ", field.name(), value).unwrap();
            }
        }
        let mut message = String::new();
        event.record(&mut StringVisitor {
            string: &mut message,
        });
        // write to log file for debugging
        writeln!(writer, "[{level}] {timestamp}: {message}")?;

        // Send the log message to Flutter
        LogMessage {
            level: level.to_string(),
            message,
            timestamp,
        }
        .send_signal_to_dart();
        Ok(())
    }
}
