//! Logging system for FFI interface.

use std::ffi::{c_char, c_int, CString};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Log callback function type.
/// level: 0=trace, 1=debug, 2=info, 3=warn, 4=error
/// message: UTF-8 string allocated by Rust (free with pb_mapper_free_string)
/// timestamp: unix timestamp (seconds)
pub type LogCallback = extern "C" fn(level: c_int, message: *const c_char, timestamp: u64);

/// Global log callback
static LOG_CALLBACK: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

/// Internal function to send log to callback
pub(crate) fn send_log(level: c_int, message: &str) {
    let ptr = LOG_CALLBACK.load(Ordering::SeqCst);
    if ptr.is_null() {
        return;
    }

    if let Ok(c_msg) = CString::new(message) {
        let callback: LogCallback = unsafe { std::mem::transmute(ptr) };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let leaked = c_msg.into_raw();
        callback(level, leaked, timestamp);
    }
}

/// Set log callback function.
///
/// # Safety
/// `callback` must be a valid function pointer or null to disable logging.
#[no_mangle]
pub extern "C" fn pb_mapper_set_log_callback(callback: Option<LogCallback>) {
    let ptr = callback.map(|f| f as *mut ()).unwrap_or(ptr::null_mut());
    LOG_CALLBACK.store(ptr, Ordering::SeqCst);
}

/// Free a string allocated by the library (e.g., from log callback or JSON result).
///
/// # Safety
/// `s` must be a valid pointer returned from this library, or null.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}

/// Custom tracing layer that forwards logs to FFI callback.
pub(crate) struct FfiLogLayer;

impl<S> tracing_subscriber::Layer<S> for FfiLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = match *event.metadata().level() {
            tracing::Level::TRACE => 0,
            tracing::Level::DEBUG => 1,
            tracing::Level::INFO => 2,
            tracing::Level::WARN => 3,
            tracing::Level::ERROR => 4,
        };

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = format!(
            "[{}] {}",
            event.metadata().target(),
            visitor.message.unwrap_or_default()
        );
        send_log(level, &message);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else if self.message.is_none() {
            self.message = Some(format!("{}: {value:?}", field.name()));
        } else if let Some(msg) = self.message.take() {
            self.message = Some(format!("{msg}, {}: {value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else if self.message.is_none() {
            self.message = Some(format!("{}: {value}", field.name()));
        } else if let Some(msg) = self.message.take() {
            self.message = Some(format!("{msg}, {}: {value}", field.name()));
        }
    }
}

/// Initialize logging with FFI callback support.
///
/// # Safety
/// Can be called multiple times safely.
#[no_mangle]
pub extern "C" fn pb_mapper_init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let _ = tracing_subscriber::registry()
        .with(FfiLogLayer)
        .with({
            let filter = tracing_subscriber::EnvFilter::from_default_env();
            let filter = match "pb_mapper=info".parse() {
                Ok(directive) => filter.add_directive(directive),
                Err(_) => filter,
            };
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_filter(filter)
        })
        .try_init();
}
