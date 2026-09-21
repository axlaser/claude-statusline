//! `subagent` — the `subagentStatusLine` handler that tees the tasks feed.
//!
//! Claude Code hands this process every visible task once per refresh tick. It
//! writes a trimmed copy to a session-scoped file and **prints nothing**:
//! anything on stdout replaces Claude Code's default agent panel outright.
//!
//! The feed is a data store, not a performance cache: its freshness window is
//! a render input, which is why `git-refresh` leaves it alone.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::debug;
use crate::session::sanitize_session_id;
use crate::state::{self, WriteOutcome};

/// The per-task fields the status line reads back, in the order the scripts
/// emit them. Order is observable: `feed_bytes_match_the_captured_fixtures`
/// asserts the projected bytes against the captured feed fixtures, so a
/// reordering fails the case table even though every row would render the same.
pub const TASK_FIELDS: [&str; 10] = [
    "id",
    "name",
    "type",
    "description",
    "status",
    "model",
    "effort",
    "contextWindowSize",
    "tokenCount",
    "startTime",
];

/// Where this session's feed lives.
pub fn feed_path(temp: &Path, safe_id: &str) -> PathBuf {
    temp.join(format!("statusline-tasks-{safe_id}.json"))
}

/// What one tick did: the three ways of writing nothing look identical on disk.
#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    /// The feed now holds this tick's bytes.
    Wrote(PathBuf),
    /// Nothing written, previous feed kept: no usable id or untrusted payload.
    Skipped,
    /// The target could not be made safe, so the write was abandoned.
    Hostile,
    /// The write itself failed (permissions, disk, rename).
    Failed,
}

/// Projects one task object down to the fields the reader consumes. Absent
/// and null fields are dropped, not emitted as null or `""`: Claude Code
/// reports `effort` only for an explicit override, so its presence is the
/// signal to render that segment at all.
fn project_task(task: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for field in TASK_FIELDS {
        match task.get(field) {
            Some(v) if !v.is_null() => {
                out.insert(field.to_string(), v.clone());
            }
            _ => {}
        }
    }
    out
}

/// Builds the exact bytes this payload tees plus the sanitized session id (so
/// the caller cannot derive it differently), or `None` when the tick must be
/// skipped: an unparseable or non-object payload, no usable session id, or a
/// `tasks` field of the wrong type. Each leaves the last good feed in place —
/// the malformed-tick isolation that got the raw-tee prototype rejected in
/// `docs/performance.md` §7.
pub fn project(payload: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let root = value.as_object()?;

    let safe_id = sanitize_session_id(root.get("session_id").and_then(Value::as_str).unwrap_or(""));
    // An id that sanitises to nothing would write `statusline-tasks-.json`, a
    // path every such session would share.
    if safe_id.is_empty() {
        return None;
    }

    let tasks: Vec<Value> = match root.get("tasks") {
        // Absent is a real state, no visible tasks, and writes an empty list.
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            // A non-object task is dropped rather than emitted as `{}`: jq's
            // `select(type == "object")` drops it, the PowerShell script
            // emitted `{}`, and the bash behaviour won: `{}` is not a task.
            .filter_map(Value::as_object)
            .map(|t| Value::Object(project_task(t)))
            .collect(),
        // Present but not an array: jq fails outright and writes nothing, the
        // isolation above, so this is a malformed tick.
        Some(_) => return None,
    };

    let mut root_out = Map::new();
    root_out.insert("tasks".to_string(), Value::Array(tasks));
    Some((safe_id, Value::Object(root_out).to_string()))
}

/// Tees one tick's payload to this session's feed file through the shared
/// guard (`state::write_guarded_under`), abandoning a write whose target
/// cannot be made safe: on a shared `/tmp` the path is predictable from the
/// session id, which would otherwise make this an arbitrary-write primitive.
pub fn run(payload: &str, temp: &crate::session::StateRoot) -> Tick {
    let Some((safe_id, bytes)) = project(payload) else {
        debug::log(|| "subagent-statusline: tick skipped, feed left as-is".to_string());
        return Tick::Skipped;
    };

    let path = feed_path(temp, &safe_id);
    match state::write_guarded_under(temp, &path, bytes.as_bytes()) {
        WriteOutcome::Written => {
            if debug::is_enabled() {
                let n = bytes.len();
                debug::log(move || format!("subagent-statusline: wrote {n} byte(s) to the feed"));
            }
            Tick::Wrote(path)
        }
        WriteOutcome::SkippedHostile => {
            let p = path.display().to_string();
            debug::log(move || format!("subagent-statusline: hostile feed target, skipped {p}"));
            Tick::Hostile
        }
        WriteOutcome::Failed => {
            let p = path.display().to_string();
            debug::log(move || format!("subagent-statusline: feed write failed for {p}"));
            Tick::Failed
        }
    }
}
