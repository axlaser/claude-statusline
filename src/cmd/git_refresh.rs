//! `git-refresh` — the PostToolUse hook that invalidates the git cache. The
//! pilot component: the smallest of the four, so it proved the pipeline first.
//!
//! Claude Code runs this after every tool use, so it is a hot path like the
//! status line: it must exit 0, write nothing to stderr, and do as little as
//! possible.

use std::path::{Path, PathBuf};

use crate::debug;
use crate::session::sanitize_session_id;

/// The tools whose use can change git state.
///
/// This list also appears as the hook's `matcher` in `settings.json`, so the
/// two have to agree or the hook fires for tools this ignores -- asserted by
/// `the_invalidating_tools_and_the_settings_matcher_agree`, because nothing
/// derives one from the other.
///
/// **`Bash` is deliberately absent**, though the scripts had it. It was half
/// of every tool call in a real session and three quarters of the entries
/// here, so every `ls` and every `grep` deleted the git cache and made the next
/// tick pay a full miss. Dropping it is safe because the cache is keyed on
/// `.git/index` mtime: a shell command that touches the index -- `git commit`,
/// `git add` -- is still observed on the very next tick, through the key rather
/// than through the hook. What changes is that a command altering only
/// untracked or worktree state is seen within the 5s TTL instead of
/// immediately, which is an accepted divergence recorded in
/// `docs/performance.md` §4. The alternatives were worse: deletion already
/// coalesces, so marking the cache stale saves nothing, and reading the
/// payload's command to tell `git commit` from `ls` adds parsing to a path
/// that runs after every tool call and replaces one guess with another.
pub const INVALIDATING_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The two caches a file-modifying tool invalidates.
///
/// The tasks feed, notification latch and focus record are deliberately absent:
/// they are data stores, not performance caches, and deleting them here would
/// drop subagent rows, re-fire alerts and orphan raised toasts on every edit.
pub fn cache_paths(temp: &Path, safe_id: &str) -> Vec<PathBuf> {
    vec![
        temp.join(format!("statusline-git-{safe_id}.txt")),
        // Vestigial since state moved into `<temp>/claude-statusline-<owner>/`:
        // the binary never writes an output cache, and a script-era install
        // could only have written flat in the temp root, which `temp` no longer
        // is, so this unlink always misses. Kept because the git-refresh
        // fixtures record it as a deleted path and the flat sweep it stands in
        // for is still live in both uninstallers. Retire it there and here
        // together.
        temp.join(format!("statusline-oc-{safe_id}.txt")),
    ]
}

/// Decides which paths this payload invalidates, without touching the disk.
/// Empty for every degraded case: unparseable input, a tool that does not
/// change files, an absent or unusable session id.
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
    // An empty id would produce `statusline-git-.txt`, shared by every such
    // session. The scripts stop on an empty id; so does this.
    if safe_id.is_empty() {
        return Vec::new();
    }

    cache_paths(temp, &safe_id)
}

/// Deletes the caches this payload invalidates and returns what it removed. A
/// missing file is a no-op, not an error: usually the status line has not
/// rendered since the last edit, so there is nothing to remove.
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
