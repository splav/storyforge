//! Shared helper for locating sibling binaries from integration tests.

use std::path::PathBuf;

/// Path to a binary that Cargo places next to the current test executable.
///
/// Walks out of the `deps/` subdirectory when present. Profile-agnostic
/// (resolves under `target/<profile>/` via `current_exe`, unlike a hardcoded
/// `target/debug/...`).
pub fn sibling_bin(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    if !path.join(name).exists() && path.ends_with("deps") {
        path.pop();
    }
    path.push(name);
    path
}
