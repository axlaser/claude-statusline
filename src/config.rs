//! `~/.claude/notify-config.json`: the per-event `sound` and `visual` flags
//! `notify` delivers by, and the `context_high` / `rate_limit` thresholds the
//! status line fires edge alerts at.
//!
//! Every failure degrades to the defaults. A missing file is the common case,
//! and an unparseable one must not silence notifications, because the user
//! could not tell "muted" from "broken".

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Defaults for the two edge-triggered alerts, used when the key is absent or
/// not an integer.
pub const DEFAULT_CONTEXT_HIGH_THRESHOLD: i64 = 70;
pub const DEFAULT_RATE_LIMIT_THRESHOLD: i64 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventConfig {
    pub sound: bool,
    pub visual: bool,
}

impl Default for EventConfig {
    /// Both on. An absent config means "notify me", not "stay quiet".
    fn default() -> Self {
        Self {
            sound: true,
            visual: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct NotifyConfig {
    root: Option<Value>,
}

impl NotifyConfig {
    /// `~/.claude/notify-config.json` for this user, if home resolves.
    pub fn default_path() -> Option<PathBuf> {
        crate::claude_dir().map(|d| d.join("notify-config.json"))
    }

    /// Reads and parses the file, degrading to defaults on every failure.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        Self {
            root: serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object),
        }
    }

    /// The delivery flags for one event. A flag is off **only** when it is the
    /// JSON literal `false`; absent, null, a string or a number leave it on.
    ///
    /// This is the one place the port deliberately does not reproduce the
    /// shipped bash behaviour. Both shell scripts read the flag as
    /// `jq -r '.[$e].sound // true'`, and jq's `//` yields its right-hand
    /// side for `false` as well as null, so muting has never worked on macOS
    /// or Linux. Intended behaviour wins over reproducing a bug.
    pub fn event(&self, event: &str) -> EventConfig {
        let mut cfg = EventConfig::default();
        let Some(entry) = self.root.as_ref().and_then(|r| r.get(event)) else {
            return cfg;
        };
        if entry.get("sound") == Some(&Value::Bool(false)) {
            cfg.sound = false;
        }
        if entry.get("visual") == Some(&Value::Bool(false)) {
            cfg.visual = false;
        }
        cfg
    }

    /// The percentage at which an edge-triggered alert fires. Only
    /// `context_high` and `rate_limit` have one; any other event reports its
    /// own default so a caller cannot silently get someone else's.
    pub fn threshold(&self, event: &str) -> i64 {
        let fallback = match event {
            "context_high" => DEFAULT_CONTEXT_HIGH_THRESHOLD,
            _ => DEFAULT_RATE_LIMIT_THRESHOLD,
        };
        self.root
            .as_ref()
            .and_then(|r| r.get(event))
            .and_then(|e| e.get("threshold"))
            .and_then(Value::as_i64)
            .unwrap_or(fallback)
    }
}
