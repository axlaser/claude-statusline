//! Subagent rows: one line per Task-tool subagent, and the model-to-window
//! resolution they depend on.
//!
//! Rows come from **one tier per refresh, never merged**: the tasks feed
//! `cmd::subagent` tees when it is fresh, otherwise per-agent transcript
//! parsing. Merging would double-count a task present in both.
//!
//! Both tiers share a done signal and a linger of [`DONE_LINGER_SECS`], so a
//! row that completes between refreshes is still seen. The stamp lives in a
//! state file (`statusline-sa-<session>-task-<id>.txt` for the feed tier,
//! `statusline-sa-<session>-<agent-base>.txt` for the fallback) so it survives
//! the process that observed the completion. The namespaces are distinct
//! because [`disappeared_rows`] scans the `-task-` prefix and would render a
//! fallback file there as a vanished task a second time.
//!
//! The fallback tier once lacked the stamp and lingered for the full
//! [`FALLBACK_MAX_AGE_SECS`] window; the scripts carried it
//! (`eb56345:linux/statusline.sh:1287` and `:1357`). The same record doubles as
//! the mtime skip that spares re-reading every agent transcript per refresh.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::clock::Clock;
use crate::debug;
use crate::payload::sanitize_display;
use crate::session;
use crate::state;

/// How fresh the tasks feed must be to be used at all.
pub const FEED_TTL_SECS: i64 = 10;

/// How long a finished row lingers before it disappears.
pub const DONE_LINGER_SECS: i64 = 30;

/// Fallback-tier transcripts older than this belong to an earlier run.
pub const FALLBACK_MAX_AGE_SECS: i64 = 180;

/// The window assumed for a model nothing else can resolve.
pub const DEFAULT_WINDOW: u64 = 200_000;

/// One rendered subagent row's inputs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Row {
    pub used: u64,
    pub window: u64,
    pub model: String,
    pub display: String,
    pub effort: String,
    pub done: bool,
}

// --- Model identity ------------------------------------------------------

/// Strips a trailing `-YYYYMMDD` date suffix, and nothing else: the output is
/// the learned map's key and the `1m` tier's input, so stripping `[1m]` too
/// would rewrite every stored key and make that tier unreachable.
pub fn normalize_model_id(id: &str) -> String {
    if let Some(stem) = id.rfind('-').map(|i| (&id[..i], &id[i + 1..])) {
        let (head, tail) = stem;
        if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) {
            return head.to_string();
        }
    }
    id.to_string()
}

/// The identity used only to ask "is this the session's model?". Never a
/// storage key and never a resolver-tier input: it folds `[1m]` and `-1m`
/// away, which the variant tier still needs to see.
pub fn model_base_id(id: &str) -> String {
    let normalized = normalize_model_id(id);
    let trimmed = normalized
        .strip_suffix("[1m]")
        .or_else(|| normalized.strip_suffix("-1m"))
        .unwrap_or(&normalized);
    trimmed.to_lowercase()
}

/// Known model windows by normalized id; unlisted ids fall through.
pub fn seed_window(normalized: &str) -> Option<u64> {
    match normalized.strip_prefix("claude-").unwrap_or(normalized) {
        "fable-5" | "opus-4-8" | "opus-4-7" | "opus-4-6" | "sonnet-5" | "sonnet-4-6" => {
            Some(1_000_000)
        }
        "haiku-4-5" | "sonnet-4-5" | "opus-4-5" => Some(200_000),
        _ => None,
    }
}

/// The tiered window resolver.
#[derive(Debug, Default, Clone)]
pub struct Windows {
    session_model_id: String,
    session_window: Option<u64>,
    learned: BTreeMap<String, u64>,
}

impl Windows {
    pub fn new(
        session_model_id: &str,
        session_window: Option<u64>,
        learned: BTreeMap<String, u64>,
    ) -> Self {
        Self {
            session_model_id: session_model_id.to_string(),
            // A non-positive window is no window: it would divide by zero
            // downstream.
            session_window: session_window.filter(|w| *w > 0),
            learned,
        }
    }

    /// Loads `~/.claude/statusline-model-windows.json`. Anything that is not an
    /// object of usable numbers yields an empty map, degrading to the seed
    /// table: the learned map is an optimization, never a prerequisite.
    pub fn load_learned(path: &Path) -> BTreeMap<String, u64> {
        let mut map = BTreeMap::new();
        let Some(bytes) = state::read_trusted(path) else {
            return map;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return map;
        };
        let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(&text) else {
            return map;
        };
        for (key, value) in entries {
            if key.is_empty() {
                continue;
            }
            // Numbers and numeric strings alike: jq stringifies both before
            // bash's `^[0-9]+$` guard sees them.
            let window = match &value {
                Value::Number(n) => n.as_u64(),
                Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                    s.parse().ok()
                }
                _ => None,
            };
            if let Some(w) = window {
                map.insert(key, w);
            }
        }
        map
    }

    /// Session → learned → seed table → `1m` marker → default. The session tier
    /// leads because it is live truth; a learned entry may be stale or wrong.
    pub fn resolve(&self, model_id: &str) -> u64 {
        let normalized = normalize_model_id(model_id);

        if !model_id.is_empty() && !self.session_model_id.is_empty() {
            if let Some(window) = self.session_window {
                if model_base_id(model_id) == model_base_id(&self.session_model_id) {
                    return window;
                }
            }
        }

        if let Some(w) = self.learned.get(&normalized) {
            return *w;
        }

        if let Some(w) = seed_window(&normalized) {
            return w;
        }

        // The marker tier reads the *normalized* id, which still carries the
        // variant suffix — this is what `model_base_id` must never be used for.
        if normalized.contains("[1m]") || normalized.contains("-1m") {
            return 1_000_000;
        }

        DEFAULT_WINDOW
    }
}

// --- Feed tier -----------------------------------------------------------

/// One task as the feed describes it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FeedTask {
    pub id: String,
    pub display: String,
    pub status: String,
    pub model: String,
    /// `None` when the feed omitted it — Claude Code below v2.1.205 — which
    /// sends the row through the tiered resolver instead.
    pub window: Option<u64>,
    pub tokens: u64,
    pub start: String,
    pub effort: String,
}

/// Whether a feed status means the task is running. Deny-list on purpose: a
/// status Claude Code adds later reads as working rather than vanishing, and a
/// finished task that leaves the feed is caught by the disappeared signal.
pub fn status_is_active(status: &str) -> bool {
    !matches!(
        status.to_lowercase().as_str(),
        "completed"
            | "complete"
            | "done"
            | "finished"
            | "failed"
            | "cancelled"
            | "canceled"
            | "killed"
            | "stopped"
            | "error"
    )
}

/// Parses the feed payload. `None` means "not a feed", which drops the tier;
/// an object whose `tasks` is absent is a valid empty feed.
pub fn parse_feed(raw: &str) -> Option<Vec<FeedTask>> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let tasks = match object.get("tasks") {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return None,
    };

    let mut out = Vec::new();
    for item in tasks {
        let Some(task) = item.as_object() else {
            continue;
        };
        let text = |key: &str| -> String {
            match task.get(key) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                _ => String::new(),
            }
        };
        // First of description, type, name that is non-blank after scrubbing,
        // so a title of "|||" falls through instead of rendering as spaces.
        let display = ["description", "type", "name"]
            .iter()
            .map(|k| sanitize_display(&text(k)))
            .find(|candidate| !candidate.is_empty())
            .unwrap_or_default();

        let window = match task.get("contextWindowSize") {
            Some(Value::Number(n)) => n.as_u64().filter(|w| *w > 0),
            Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                s.parse().ok().filter(|w: &u64| *w > 0)
            }
            _ => None,
        };
        let tokens = match task.get("tokenCount") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                s.parse().unwrap_or(0)
            }
            _ => 0,
        };

        // Scrubbed here, where the scripts scrubbed: these land in the
        // `|`-separated task record, and a `|` would shift every later field on
        // read-back. The sink scrubs too, which still covers records an older
        // binary wrote, but by then the field boundaries are already lost.
        let candidate = FeedTask {
            id: text("id"),
            display,
            status: text("status"),
            model: sanitize_display(&text("model")),
            window,
            tokens,
            start: sanitize_display(&text("startTime")),
            effort: sanitize_display(&text("effort")),
        };
        if candidate.id.is_empty()
            && candidate.display.is_empty()
            && candidate.status.is_empty()
            && candidate.model.is_empty()
        {
            continue;
        }
        out.push(candidate);
    }
    Some(out)
}

/// The per-task state file that carries the done stamp across refreshes.
fn task_state_path(temp: &Path, session_id: &str, task_id: &str) -> Option<PathBuf> {
    let session = session::sanitize_session_id(session_id);
    let task = session::sanitize_session_id(task_id);
    if session.is_empty() || task.is_empty() {
        return None;
    }
    Some(temp.join(format!("statusline-sa-{session}-task-{task}.txt")))
}

/// The per-agent state file the fallback tier keys on: `-<base>`, never
/// `-task-<id>`, which `disappeared_rows` would read back as a vanished feed
/// task (see the module doc). The scripts kept the two apart the same way.
fn agent_state_path(temp: &Path, session_id: &str, agent_base: &str) -> Option<PathBuf> {
    let session = session::sanitize_session_id(session_id);
    let base = session::sanitize_session_id(agent_base);
    if session.is_empty() || base.is_empty() {
        return None;
    }
    Some(temp.join(format!("statusline-sa-{session}-{base}.txt")))
}

/// The per-agent record:
/// `mtime|stop_reason|input|cache_write|cache_read|model|display|done|size`.
///
/// Field order is the scripts' verbatim, with `size` appended: a different
/// layout would read back as a wrong token count rather than a miss, and
/// appending keeps every existing field where it was. The location moved under
/// `<temp>/claude-statusline-<owner>/`, so old flat files go unread after a
/// mid-session upgrade at the cost of one re-scan (`docs/performance.md` §4);
/// a record written before `size` existed has eight fields and is rejected
/// whole by the same strictness, at the same cost.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct AgentState {
    mtime: i64,
    /// Paired with `mtime` as the staleness key. mtime alone is strictly
    /// weaker than the main transcript's `(mtime, size)` and is exposed to the
    /// documented Windows behaviour where a writer's mtime does not move until
    /// its handle closes — an agent appending through an open handle would be
    /// read once and then never again.
    size: u64,
    stop_reason: String,
    input_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
    model: String,
    display: String,
    done_at: Option<i64>,
}

impl AgentState {
    fn parse(raw: &str) -> Option<Self> {
        let fields: Vec<&str> = raw.trim_end_matches(['\r', '\n']).split('|').collect();
        // Exactly nine; a shorter record is torn, foreign, or written before
        // `size` joined the key, and guessing would render a confident wrong
        // number. The field count is the format version.
        if fields.len() != 9 {
            return None;
        }
        let at = |i: usize| fields[i];
        Some(Self {
            mtime: at(0).parse().ok()?,
            size: at(8).parse().ok()?,
            stop_reason: at(1).to_string(),
            input_tokens: at(2).parse().unwrap_or(0),
            cache_write_tokens: at(3).parse().unwrap_or(0),
            cache_read_tokens: at(4).parse().unwrap_or(0),
            model: at(5).to_string(),
            display: at(6).to_string(),
            done_at: at(7).parse().ok(),
        })
    }

    fn to_line(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.mtime,
            scrub_field(&self.stop_reason),
            self.input_tokens,
            self.cache_write_tokens,
            self.cache_read_tokens,
            scrub_field(&self.model),
            scrub_field(&self.display),
            self.done_at.map(|d| d.to_string()).unwrap_or_default(),
            self.size
        )
    }

    fn used(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

/// Keeps a `|` in a value from shifting every later field on read-back.
fn scrub_field(s: &str) -> String {
    s.chars()
        .map(|c| if c == '|' || c.is_control() { ' ' } else { c })
        .collect()
}

/// The per-task record: `tokens|window|model|display|done|start|effort`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TaskState {
    tokens: u64,
    window: u64,
    model: String,
    display: String,
    done_at: Option<i64>,
    start: String,
    effort: String,
}

impl TaskState {
    fn parse(raw: &str) -> Self {
        let fields: Vec<&str> = raw.trim_end_matches(['\r', '\n']).split('|').collect();
        let at = |i: usize| fields.get(i).copied().unwrap_or_default();
        Self {
            tokens: at(0).parse().unwrap_or(0),
            window: at(1).parse().unwrap_or(0),
            model: at(2).to_string(),
            display: at(3).to_string(),
            done_at: at(4).parse().ok(),
            start: at(5).to_string(),
            effort: at(6).to_string(),
        }
    }

    fn to_line(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.tokens,
            self.window,
            self.model,
            self.display,
            self.done_at.map(|d| d.to_string()).unwrap_or_default(),
            self.start,
            self.effort
        )
    }
}

/// Builds the feed tier's rows, stamping and expiring done markers as it goes.
/// `None` means the payload is not a feed, which sends the caller to the
/// fallback tier.
pub fn rows_from_feed(
    clock: &dyn Clock,
    temp: &crate::session::StateRoot,
    session_id: &str,
    feed_json: &str,
    windows: &Windows,
) -> Option<Vec<Row>> {
    let tasks = parse_feed(feed_json)?;
    let now = clock.now_unix();
    let mut seen: Vec<String> = Vec::new();
    // Sorted by the same key the scripts sort on — start time, then id — so the
    // row order is stable across refreshes rather than following feed order.
    let mut candidates: Vec<(String, Row)> = Vec::new();

    for task in &tasks {
        let safe_id = session::sanitize_session_id(&task.id);
        if !safe_id.is_empty() {
            seen.push(safe_id);
        }
        let window = task.window.unwrap_or_else(|| windows.resolve(&task.model));
        let path = task_state_path(temp, session_id, &task.id);
        let previous = path
            .as_deref()
            .and_then(state::read_trusted)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|t| TaskState::parse(&t));

        let done_at = if status_is_active(&task.status) {
            None
        } else {
            // First observation stamps; later ones keep the original stamp so
            // the linger measures from completion, not from noticing.
            Some(previous.as_ref().and_then(|p| p.done_at).unwrap_or(now))
        };

        if let Some(path) = path.as_deref() {
            let record = TaskState {
                tokens: task.tokens,
                window,
                model: task.model.clone(),
                display: task.display.clone(),
                done_at,
                start: task.start.clone(),
                effort: task.effort.clone(),
            };
            // Only when it changed: this store used to rewrite every visible
            // task's file on every tick.
            if previous.as_ref() != Some(&record) {
                let outcome = state::write_guarded_under(temp, path, record.to_line().as_bytes());
                if outcome != state::WriteOutcome::Written {
                    let p = path.display().to_string();
                    debug::log(move || {
                        format!("subagent: task state not persisted to {p}: {outcome:?}")
                    });
                }
            }
        }

        if let Some(stamp) = done_at {
            if now - stamp > DONE_LINGER_SECS {
                continue;
            }
        }
        candidates.push((
            sort_key(&task.start, &task.id),
            Row {
                used: task.tokens,
                window,
                model: task.model.clone(),
                display: task.display.clone(),
                effort: task.effort.clone(),
                done: done_at.is_some(),
            },
        ));
    }

    candidates.extend(disappeared_rows(clock, temp, session_id, &seen));
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let count = candidates.len();
    debug::log(move || format!("subagents: feed tier, {count} row(s)"));
    Some(candidates.into_iter().map(|(_, row)| row).collect())
}

/// A task that has a state file but is no longer in a fresh feed has finished:
/// the second done signal, the one that catches a task which completes and
/// leaves the feed in the same tick, which the status signal never sees.
fn disappeared_rows(
    clock: &dyn Clock,
    temp: &crate::session::StateRoot,
    session_id: &str,
    seen: &[String],
) -> Vec<(String, Row)> {
    let session = session::sanitize_session_id(session_id);
    if session.is_empty() {
        return Vec::new();
    }
    // `strip_prefix`, never bash's greedy `${file##*-task-}`: an id containing
    // `-task-` (even `task-0001`) would then never match the seen list, and a
    // running task would render a second time as `done`.
    let prefix = format!("statusline-sa-{session}-task-");
    let now = clock.now_unix();
    let Ok(entries) = std::fs::read_dir(temp) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".txt"))
        else {
            continue;
        };
        if seen.iter().any(|s| s == id) {
            continue;
        }
        let path = entry.path();
        let Some(bytes) = state::read_trusted(&path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let mut record = TaskState::parse(&text);

        let stamp = match record.done_at {
            Some(stamp) => stamp,
            None => {
                record.done_at = Some(now);
                // A stamp that never lands re-stamps `now` every tick, so the
                // row lingers indefinitely; nothing else can report that.
                let outcome = state::write_guarded_under(temp, &path, record.to_line().as_bytes());
                if outcome != state::WriteOutcome::Written {
                    let p = path.display().to_string();
                    debug::log(move || {
                        format!("subagent: done stamp not persisted to {p}: {outcome:?}")
                    });
                }
                now
            }
        };
        if now - stamp > DONE_LINGER_SECS {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        out.push((
            sort_key(&record.start, id),
            Row {
                used: record.tokens,
                window: record.window,
                model: record.model,
                display: record.display,
                effort: record.effort,
                done: true,
            },
        ));
    }
    out
}

/// The scripts join start time and id with `\x1f` and byte-sort. Reproduced
/// literally rather than as a tuple compare: `\x1f` sorts below every printable
/// byte, so a short start time sorts before a longer one sharing its prefix.
fn sort_key(start: &str, id: &str) -> String {
    format!("{start}\u{1f}{id}")
}

// --- Fallback tier -------------------------------------------------------

/// Per-agent transcripts: `<project>/<session-base>/subagents/`.
pub fn subagents_dir(transcript_path: &str) -> Option<PathBuf> {
    let path = Path::new(transcript_path);
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    Some(parent.join(stem).join("subagents"))
}

/// A terminal stop reason means the agent is finished. `tool_use`, `pause_turn`
/// and "no assistant message yet" all mean it is still working.
pub fn stop_reason_is_done(stop_reason: &str) -> bool {
    matches!(
        stop_reason,
        "end_turn" | "max_tokens" | "refusal" | "model_context_window_exceeded" | "stop_sequence"
    )
}

/// What one agent transcript's last assistant entry reports.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AgentReading {
    pub stop_reason: String,
    pub input_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub model: String,
}

impl AgentReading {
    pub fn used(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

/// Reads the last assistant entry of an agent transcript. Unparseable lines
/// are skipped rather than ending the scan: a torn tail is routine in a file
/// being appended to, and the entry before it is still the best reading.
///
/// Scanned **backwards**, returning at the first assistant entry it finds.
/// Only the last one is ever kept, so a forward pass JSON-parsed every line of
/// the file in order to throw all but one away; from the end it parses one in
/// the ordinary case. Byte-oriented throughout, like the main transcript's
/// scan -- a reverse boundary is a byte offset, and any decoding step would
/// break the arithmetic.
pub fn read_agent(bytes: &[u8]) -> AgentReading {
    let out = AgentReading::default();
    // `rsplit` yields the trailing empty slice of a newline-terminated file
    // first; it fails to parse and is skipped like any other torn line.
    for line in bytes.rsplit(|b| *b == b'\n') {
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let message = value.get("message");
        let usage = message.and_then(|m| m.get("usage"));
        let number = |key: &str| -> u64 {
            usage
                .and_then(|u| u.get(key))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        return AgentReading {
            stop_reason: message
                .and_then(|m| m.get("stop_reason"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_tokens: number("input_tokens"),
            cache_write_tokens: number("cache_creation_input_tokens"),
            cache_read_tokens: number("cache_read_input_tokens"),
            model: message
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
    }
    out
}

/// Fallback row title: meta description, then agent type, then the agent id.
pub fn agent_display(meta: Option<&str>, agent_base: &str) -> String {
    if let Some(raw) = meta {
        if let Ok(value) = serde_json::from_str::<Value>(raw) {
            for key in ["description", "agentType"] {
                let candidate =
                    sanitize_display(value.get(key).and_then(Value::as_str).unwrap_or_default());
                if !candidate.is_empty() {
                    return candidate;
                }
            }
        }
    }
    sanitize_display(agent_base.strip_prefix("agent-").unwrap_or(agent_base))
}

/// Builds the fallback tier's rows by parsing each agent transcript.
pub fn rows_from_transcripts(
    clock: &dyn Clock,
    temp: &crate::session::StateRoot,
    session_id: &str,
    transcript_path: &str,
    windows: &Windows,
) -> Vec<Row> {
    let Some(dir) = subagents_dir(transcript_path) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let now = clock.now_unix();

    let mut rows: Vec<(String, Row)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("agent-") || !name.ends_with(".jsonl") {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let mtime = clock.mtime_unix(&path).unwrap_or(0);
        if now - mtime > FALLBACK_MAX_AGE_SECS {
            continue;
        }
        // The size half of the key comes from the real filesystem, as the main
        // transcript's does; only mtime goes through the injected clock.
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        let base = name.trim_end_matches(".jsonl");
        let state_path = agent_state_path(temp, session_id, base);

        let previous = state_path
            .as_deref()
            .and_then(state::read_trusted)
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| AgentState::parse(&t));

        // An unchanged mtime rebuilds from the previous record instead of
        // re-reading: the unconditional scan made the main transcript 4x
        // slower than the script it replaced. `dirty` is the scripts'
        // `sa_cache_dirty`: a fresh read, or a stamp that moved.
        let mut dirty = false;
        let mut record = match previous {
            Some(prev) if prev.mtime == mtime && prev.size == size => prev,
            prev => {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let reading = read_agent(&bytes);
                let meta = std::fs::read_to_string(dir.join(format!("{base}.meta.json"))).ok();
                dirty = true;
                AgentState {
                    mtime,
                    size,
                    stop_reason: reading.stop_reason,
                    input_tokens: reading.input_tokens,
                    cache_write_tokens: reading.cache_write_tokens,
                    cache_read_tokens: reading.cache_read_tokens,
                    model: reading.model,
                    display: agent_display(meta.as_deref(), base),
                    // Carried across a re-read: re-stamping on every content
                    // change would make the row linger forever.
                    done_at: prev.and_then(|p| p.done_at),
                }
            }
        };

        // Stamp once on a terminal stop reason; clear it otherwise, so a
        // resumed agent does not carry an earlier completion time.
        let done = stop_reason_is_done(&record.stop_reason);
        match (done, record.done_at) {
            (true, None) => {
                record.done_at = Some(now);
                dirty = true;
            }
            (false, Some(_)) => {
                record.done_at = None;
                dirty = true;
            }
            _ => {}
        }

        if dirty {
            if let Some(p) = state_path.as_deref() {
                let outcome = state::write_guarded_under(temp, p, record.to_line().as_bytes());
                if outcome != state::WriteOutcome::Written {
                    let path = p.display().to_string();
                    debug::log(move || {
                        format!("subagent: agent state not persisted to {path}: {outcome:?}")
                    });
                }
            }
        }

        // The linger the module doc promises for both tiers; the port once had
        // it on the feed tier only, diverging from the scripts
        // (eb56345:linux/statusline.sh:1357) with no fixture covering it.
        if done {
            if let Some(stamp) = record.done_at {
                if now - stamp > DONE_LINGER_SECS {
                    continue;
                }
            }
        }

        rows.push((
            sort_key("", base),
            Row {
                used: record.used(),
                window: windows.resolve(&record.model),
                model: record.model.clone(),
                display: record.display.clone(),
                // A transcript carries no effort override; none is inferred.
                effort: String::new(),
                done,
            },
        ));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let count = rows.len();
    debug::log(move || format!("subagents: fallback tier, {count} row(s)"));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// Whether the tasks feed is fresh enough to be the tier for this refresh.
pub fn feed_is_fresh(clock: &dyn Clock, feed_path: &Path) -> bool {
    matches!(clock.age_secs(feed_path), Some(age) if age <= FEED_TTL_SECS)
}
