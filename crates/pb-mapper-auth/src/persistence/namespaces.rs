//! The namespace allocator's high-water mark: the last number it handed out.
//!
//! It is kept twice, because each copy survives what the other does not:
//!
//! ```text
//!                         encrypted state             this file
//! root rotation, reset    carried into the snapshot   untouched
//! damaged snapshot/WAL    lost                        still knows
//! this file lost          still knows                 put back at the next start
//! ```
//!
//! Key ids tolerate losing their state because a reset also rotates the instance id,
//! so a reissued id cannot revive an old credential. A namespace has no such second
//! input: its holder keeps the number, and handing it out again would put two tenants
//! in one namespace. Only losing both copies, in practice the whole directory, can
//! repeat a number. Neither is secret: the mark is a count.
//!
//! The highest namespace any key still holds would not do as a backup. It says nothing
//! about a namespace whose keys have all gone, which is every quiet tenant's.
//!
//! A file that exists but cannot be read refuses namespace operations and nothing else
//! until it is removed, which is the operator saying the other copy is good. Existing
//! keys carry their namespace in the encrypted state and keep working.

use super::super::*;
use super::atomic_write;

const NAMESPACE_ALLOCATOR_FILE: &str = "namespace-allocator";

fn allocator_unavailable(detail: String) -> AuthFailure {
    AuthFailure::new(codes::NAMESPACE_ALLOCATOR_UNAVAILABLE, detail, false)
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

/// Reconcile this file with the encrypted state's copy at startup, returning the mark.
///
/// Putting a lost file back here, rather than at the next allocation, means a later
/// damaged snapshot still has a copy to fall back on.
pub(crate) fn restore_namespace_high_water(state_dir: &Path, persisted: u64) -> u64 {
    match read_namespace_high_water(state_dir) {
        Ok(high_water) if high_water >= persisted => high_water,
        Ok(behind) => {
            tracing::warn!(
                event = "namespace_allocator_restored",
                behind,
                persisted,
                "the namespace allocator file was lost or behind; restoring it from the encrypted state"
            );
            if let Err(error) = write_namespace_high_water(state_dir, persisted) {
                tracing::error!(
                    event = "namespace_allocator_restore_failed",
                    reason = %error.code,
                    error = %error.message,
                    "could not restore the namespace allocator file; the next allocation will retry"
                );
            }
            persisted
        }
        Err(error) => {
            tracing::error!(
                event = "namespace_allocator_damaged",
                reason = %error.code,
                error = %error.message,
                "namespace operations are refused until the damaged allocator file is removed"
            );
            persisted
        }
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
