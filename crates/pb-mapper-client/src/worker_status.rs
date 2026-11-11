//! What a tunnel worker reports about itself, and the text the public status
//! callbacks receive for it.
//!
//! Workers report a [`WorkerStatus`] to a [`StatusSink`]. The SDK maps it onto
//! its `TunnelStatus`; the public entry points that take a [`StatusCallback`]
//! wrap it with [`text_sink`], which is the only place the text is written.

use std::borrow::Cow;

/// Receives a tunnel's status as text: `connected`, `retrying`, `failed`, or
/// `failed: <reason>` once it will not come up.
pub type StatusCallback = Box<dyn Fn(&str) + Send + Sync>;

/// One tunnel worker's view of itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkerStatus {
    /// Registered, or the remote service confirmed: the tunnel carries
    /// traffic.
    Connected,
    /// Connecting, or reconnecting after a failure that may clear.
    Retrying,
    /// It will not come up, for the reason given when there is one.
    Failed(Option<String>),
}

impl WorkerStatus {
    /// The text a [`StatusCallback`] receives for this status.
    pub(crate) fn text(&self) -> Cow<'static, str> {
        match self {
            Self::Connected => "connected".into(),
            Self::Retrying => "retrying".into(),
            Self::Failed(None) => "failed".into(),
            Self::Failed(Some(reason)) => format!("failed: {reason}").into(),
        }
    }
}

/// Where a tunnel worker reports its status.
pub(crate) type StatusSink = Box<dyn Fn(&WorkerStatus) + Send + Sync>;

/// A sink handing `callback` the text of each status.
pub(crate) fn text_sink(callback: StatusCallback) -> StatusSink {
    Box::new(move |status| callback(&status.text()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Every text a public callback can receive.
    #[test]
    fn a_text_sink_writes_the_public_text() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let sink = text_sink(Box::new(move |text: &str| {
            record.lock().unwrap().push(text.to_owned());
        }));
        for status in [
            WorkerStatus::Connected,
            WorkerStatus::Retrying,
            WorkerStatus::Failed(None),
            WorkerStatus::Failed(Some("code: message".into())),
        ] {
            sink(&status);
        }
        assert_eq!(
            *seen.lock().unwrap(),
            ["connected", "retrying", "failed", "failed: code: message"]
        );
    }
}
