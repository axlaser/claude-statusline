//! `git-refresh` — the PostToolUse hook that invalidates the git cache.
//!
//! The pilot component. It is the smallest of the four, which makes it
//! the one that proves the pipeline — capture, port, fixture, delete — before
//! anything harder is attempted.
//!
//! Claude Code runs this after every tool use, so it is a hot path in the same
//! sense the status line is: it must exit 0, write nothing to stderr, and do as
//! little as possible.

use std::path::{Path, PathBuf};

use crate::debug;
use crate::session::sanitize_session_id;

/// The tools whose use can change git state. Kept verbatim from the scripts:
/// this list also appears as the hook's `matcher` in `settings.json`, so the
/// two have to agree or the hook fires for tools this ignores.
pub const INVALIDATING_TOOLS: [&str; 5] = ["Edit", "Write", "MultiEdit", "Bash", "NotebookEdit"];

/// The two caches a file-modifying tool invalidates.
///
/// The tasks feed, the notification latch and the focus record are deliberately
/// absent: they are data stores rather than performance caches, and deleting
/// them here would drop subagent rows, re-fire alerts, and orphan the toasts a
/// session already raised, on every edit.
pub fn cache_paths(temp: &Path, safe_id: &str) -> Vec<PathBuf> {
    vec![
        temp.join(format!("statusline-git-{safe_id}.txt")),
        // Vestigial since state moved into `<temp>/claude-statusline-<owner>/`.
        // The binary never writes an output cache; this entry existed to clean
        // up after a script-era install, and a script-era install could only
        // have written flat in the temp root — which `temp` is no longer. So the
        // unlink now always misses.
        //
        // Kept rather than deleted for two reasons: the git-refresh fixtures
        // record it as a deleted path, so removing it re-resolves captures for
        // no behavioural gain, and the flat sweep it stands in for is still
        // genuinely live in both uninstallers. Retire it there and here
        // together.
        temp.join(format!("statusline-oc-{safe_id}.txt")),
    ]
}

/// Decides which paths this payload invalidates, without touching the disk.
///
/// Returns an empty vector for every degraded case — unparseable input, a tool
/// that does not change files, an absent or unusable session id.
pub fn targets(payload: &str, temp: &Path) -> Vec<PathBuf> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Vec::new();
    };

    let tool = value
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !INVALIDATING_TOOLS.contains(&tool) {
        return Vec::new();
    }

    let raw = value
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let safe_id = sanitize_session_id(raw);
    // An id that sanitises to nothing would produce `statusline-git-.txt`, a
    // path shared by every such session. The scripts stop on an empty id; so
    // does this.
    if safe_id.is_empty() {
        return Vec::new();
    }

    cache_paths(temp, &safe_id)
}

/// Deletes the caches this payload invalidates and returns what it removed.
///
/// A missing file is a no-op, not an error: the common case is that the status
/// line has not rendered since the last edit, so there is nothing to remove.
pub fn run(payload: &str, temp: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for path in targets(payload, temp) {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let p = path.display().to_string();
                debug::log(move || format!("git-refresh: cannot remove {p}: {e}"));
            }
        }
    }
    if debug::is_enabled() {
        let n = removed.len();
        debug::log(move || format!("git-refresh: removed {n} cache file(s)"));
    }
    removed
}
