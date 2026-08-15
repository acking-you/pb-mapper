//! Telling the window that something changed.
//!
//! Events are **invalidation hints, not state**: they carry identity and
//! nothing else, and the receiver re-reads through the normal API. That is
//! deliberate. A dropped or coalesced event costs nothing, because the next one
//! re-reads everything anyway; shipping the new state inside the event would
//! mean taking the state lock on the emit path and guessing which projection
//! the receiver wants.

use std::ffi::{c_char, CString};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::callback::CallbackSlot;
use crate::ctl::Origin;

/// Invoked from tokio worker threads, so Dart must bind it with
/// `NativeCallable.listener`. The payload is a JSON [`StateChange`] the caller
/// frees with `pb_mapper_free_string`.
pub type ChangeCallback = extern "C" fn(payload: *const c_char);

static CHANGE_CALLBACK: CallbackSlot<ChangeCallback> = CallbackSlot::empty();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Which list went stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Services,
    Clients,
    Server,
    Config,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateChange {
    pub kind: ChangeKind,
    /// Set when the change concerns one service rather than the whole list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Lets the window tell *someone else did this*. A change it made itself
    /// needs no announcement; one a terminal made should say so.
    pub origin: Origin,
    /// Monotonic, so a receiver can drop an event that arrives behind a slow
    /// reload rather than reloading twice.
    pub seq: u64,
}

/// Announce a change. Cheap and non-blocking when nothing is listening.
pub fn emit(kind: ChangeKind, key: Option<&str>, origin: Origin) {
    let Some(callback) = CHANGE_CALLBACK.load() else {
        return;
    };
    let change = StateChange {
        kind,
        key: key.map(|k| k.to_string()),
        origin,
        seq: SEQ.fetch_add(1, Ordering::SeqCst),
    };
    let Ok(json) = serde_json::to_string(&change) else {
        return;
    };
    if let Ok(payload) = CString::new(json) {
        callback(payload.into_raw());
    }
}

/// Install the listener. Mirrors `pb_mapper_set_log_callback`.
///
/// # Safety
/// `callback` must stay valid until it is replaced or cleared with null.
#[no_mangle]
pub extern "C" fn pb_mapper_set_change_callback(callback: Option<ChangeCallback>) {
    CHANGE_CALLBACK.store(callback);
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn an_event_serialises_with_everything_a_receiver_needs() {
        let change = StateChange {
            kind: ChangeKind::Services,
            key: Some("home".into()),
            origin: Origin::Cli,
            seq: 7,
        };
        let json = serde_json::to_string(&change).unwrap();
        assert!(json.contains("\"kind\":\"services\""), "{json}");
        assert!(json.contains("\"key\":\"home\""), "{json}");
        assert!(json.contains("\"origin\":\"cli\""), "{json}");
        assert!(json.contains("\"seq\":7"), "{json}");
    }

    /// A whole-list change carries no key, and must not send a null one that
    /// the Dart side would have to special-case.
    #[test]
    fn a_list_wide_change_omits_the_key() {
        let change = StateChange {
            kind: ChangeKind::Config,
            key: None,
            origin: Origin::Ui,
            seq: 0,
        };
        let json = serde_json::to_string(&change).unwrap();
        assert!(!json.contains("key"), "{json}");
    }

    /// Emitting with nothing installed must be a no-op rather than a panic:
    /// mobile never installs one, and the tests here do not either.
    #[test]
    fn emitting_into_the_void_is_harmless() {
        emit(ChangeKind::Server, None, Origin::Internal);
    }
}
