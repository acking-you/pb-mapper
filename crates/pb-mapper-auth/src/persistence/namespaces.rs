//! The namespace allocator's high-water mark: the last number it handed out.
//!
//! This lives beside `server-instance-id`, outside the encrypted snapshot and WAL,
//! because like that file it has to outlive them:
//!
//! ```text
//! root rotation      new empty snapshot                 file untouched
//! auth-state reset   new empty snapshot                 file untouched
//! damaged snapshot   cannot say what was ever handed    file still can
//!                    out, and the operator must reset
//! ```
//!
//! Key ids tolerate the last row because a reset also rotates the instance id, so a
//! reissued id cannot revive an old credential. A namespace has no such second
//! input: its holder keeps the number, and handing it out again would put two
//! tenants in one namespace. So the mark sits where only the loss of the whole
//! directory can take it. It is a count, not a secret.
//!
//! A file that exists but cannot be read refuses namespace operations and nothing
//! else. Guessing a floor could repeat a number; existing keys carry their
//! namespace in the encrypted state and keep working.

use super::super::*;
use super::atomic_write;

const NAMESPACE_ALLOCATOR_FILE: &str = "namespace-allocator";

fn allocator_unavailable(detail: String) -> AuthFailure {
    AuthFailure::new("namespace_allocator_unavailable", detail, false)
}

/// The last namespace handed out, or 0 if none has been.
pub(crate) fn read_namespace_high_water(state_dir: &Path) -> Result<u64, AuthFailure> {
    let path = state_dir.join(NAMESPACE_ALLOCATOR_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(allocator_unavailable(format!(
                "failed to read `{}`: {error}",
                path.display()
            )));
        }
    };
    match text.trim().parse::<u64>() {
        Ok(high_water) if high_water <= MAX_ALLOCATED_NAMESPACE => Ok(high_water),
        _ => Err(allocator_unavailable(format!(
            "`{}` does not hold a namespace count",
            path.display()
        ))),
    }
}

/// Durably record `namespace` as the last one handed out.
pub(crate) fn write_namespace_high_water(
    state_dir: &Path,
    namespace: u64,
) -> Result<(), AuthFailure> {
    atomic_write(
        &state_dir.join(NAMESPACE_ALLOCATOR_FILE),
        namespace.to_string().as_bytes(),
        0o600,
    )
}
