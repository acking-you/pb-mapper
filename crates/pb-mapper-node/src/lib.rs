//! Node-API surface over [`pb_mapper_client::sdk`].
//!
//! Three things happen at this boundary and nowhere else:
//!
//! * **Numbers narrow.** JavaScript has no `u64`, so napi maps the SDK's
//!   unsigned ids and timestamps to `i64` and its page sizes to `u32`. Every
//!   inbound narrowing is checked — see `as_u64` and `narrow_page_size` —
//!   because a silent `as` cast turns a caller's mistake into wrong data
//!   instead of an error.
//! * **Enums become strings.** `"tcp"`, `"connected"`, `"allow"`: idiomatic on
//!   the JS side, and parsed back into the SDK's enums here.
//! * **Errors flatten.** Every SDK error becomes a JS `Error` carrying its
//!   rendered message, since napi has no structured error channel.
//!
//! Beyond that this crate holds no logic: anything worth testing belongs in the
//! SDK, which is exercised by `crates/pb-mapper-cli/tests/sdk_e2e.rs`.

mod admin;
mod client;
mod convert;
mod diagnostics;
mod readers;
mod tunnel;
