//! Helpers shared across modules.

use std::fmt::Display;

/// Log a fire-and-forget failure instead of discarding it.
///
/// For calls whose failure changes nothing for the caller -- a window that will
/// not focus, an event with no listener -- but that must not vanish silently:
/// `let _ = ...` hides exactly the failures worth reading in a bug report.
pub(crate) fn log_if_err<T, E: Display>(what: &str, result: Result<T, E>) {
    if let Err(err) = result {
        tracing::warn!("{what} failed: {err}");
    }
}
