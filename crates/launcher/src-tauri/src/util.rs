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

/// Walk every `.rs` file under this crate's `src/`, skipping any file named
/// `skip_file`, and return (files scanned, occurrences of `needle`).
///
/// For tests that pin where a call may appear. Callers assert on both numbers,
/// so a scan that found no files fails instead of passing vacuously, and build
/// their needle so their own source does not contain it contiguously.
#[cfg(test)]
pub(crate) fn count_in_sources(needle: &str, skip_file: Option<&str>) -> (usize, usize) {
    let mut root = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    // A Windows test binary cross-compiled from WSL carries a Linux path here,
    // which Windows cannot open. Cargo runs tests from the package root, so the
    // relative path names the same directory.
    if !root.is_dir() {
        root = std::path::PathBuf::from("src");
    }
    let mut dirs = vec![root];
    let (mut files, mut hits) = (0, 0);
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let name = path.file_name().and_then(|name| name.to_str());
            if path.extension().is_none_or(|ext| ext != "rs")
                || (skip_file.is_some() && name == skip_file)
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                files += 1;
                hits += text.matches(needle).count();
            }
        }
    }
    (files, hits)
}
