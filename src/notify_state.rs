//! Edge-triggered alerts and the per-session notification latch.
//!
//! The status line fires two alerts, context usage and a rate-limit window
//! crossing their thresholds. Both are edge triggered — one notification per
//! crossing, not per refresh — and the edge lives in a state file because each
//! refresh is a fresh process. The decision is a pure function ([`decide`]) so
//! the latch matrix is reachable from the case table without a clock, a
//! filesystem, or a spawned child.

use std::path::{Path, PathBuf};

use crate::focus::Key;
use crate::session::sanitize_session_id;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Latch {
    pub context_high: bool,
    pub rate_limit: bool,
    /// The `resets_at` the rate alert last fired against; a rollover clears
    /// the latch, else one busy window would suppress the alert forever.
    pub rate_resets_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatchState {
    /// Absent (never notified) or read cleanly.
    Usable(Latch),
    /// Present but untrusted, unreadable, or unparseable. Fail closed: notify
    /// nothing and write nothing this refresh, because a torn read left blank
    /// would read as "never notified" and re-fire the alert on every refresh.
    Unusable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    pub event: &'static str,
    pub value: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub alerts: Vec<Alert>,
    pub latch: Latch,
    /// Whether the latch differs from what was on disk and should be rewritten.
    pub changed: bool,
}

pub fn latch_path(temp_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let safe = sanitize_session_id(session_id);
    if safe.is_empty() {
        return None;
    }
    Some(temp_dir.join(format!("statusline-notify-{safe}.json")))
}

/// Reads the latch through the state guard. Absent is [`LatchState::Usable`]
/// with everything false (a session's first refresh); anything else that
/// cannot produce three good fields is [`LatchState::Unusable`].
pub fn read_latch(path: &Path) -> LatchState {
    if std::fs::symlink_metadata(path).is_err() {
        return LatchState::Usable(Latch::default());
    }
    let Some(bytes) = crate::state::read_trusted(path) else {
        return LatchState::Unusable;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return LatchState::Unusable;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return LatchState::Unusable;
    };
    let Some(map) = value.as_object() else {
        return LatchState::Unusable;
    };
    LatchState::Usable(Latch {
        context_high: map
            .get("notified_context_high")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        rate_limit: map
            .get("notified_rate_limit")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        rate_resets_at: map
            .get("last_rate_resets_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// Serialised exactly as the scripts write it, field order included.
pub fn latch_json(latch: &Latch) -> String {
    format!(
        r#"{{"notified_context_high":{},"notified_rate_limit":{},"last_rate_resets_at":{}}}"#,
        latch.context_high,
        latch.rate_limit,
        // serde owns the escape. The scripts' backslash-and-quote pair left
        // control bytes raw; one in a payload `resets_at` then wrote a latch
        // `read_latch` could never parse, and since the repairing rewrite is
        // gated on usable, the session's notifications stayed dead.
        serde_json::to_string(&latch.rate_resets_at).unwrap_or_else(|_| "\"\"".to_string())
    )
}

/// The whole edge decision, given this tick's percentages and the stored latch.
pub fn decide(
    state: LatchState,
    ctx_pct: i64,
    ctx_threshold: i64,
    rate_max: i64,
    rate_threshold: i64,
    rate_resets_now: &str,
) -> Decision {
    let LatchState::Usable(mut latch) = state else {
        return Decision {
            alerts: Vec::new(),
            latch: Latch::default(),
            changed: false,
        };
    };

    let mut alerts = Vec::new();
    let mut changed = false;

    if ctx_pct >= ctx_threshold && !latch.context_high {
        alerts.push(Alert {
            event: "context_high",
            value: ctx_pct,
        });
        latch.context_high = true;
        changed = true;
    } else if ctx_pct < ctx_threshold && latch.context_high {
        // Re-arm, so the next crossing fires again; without this the alert is
        // once per session.
        latch.context_high = false;
        changed = true;
    }

    if rate_resets_now != latch.rate_resets_at {
        latch.rate_limit = false;
        changed = true;
    }
    if rate_max >= rate_threshold && !latch.rate_limit {
        alerts.push(Alert {
            event: "rate_limit",
            value: rate_max,
        });
        latch.rate_limit = true;
        changed = true;
    }
    latch.rate_resets_at = rate_resets_now.to_string();

    Decision {
        alerts,
        latch,
        changed,
    }
}

/// The child's argv: `notify <event> <value>`, plus the click key as a fourth
/// value when the tick captured one. The child cannot capture for itself (no
/// console, parent gone), so the key travels in argv, visible to same-user
/// processes, which are inside the boundary already.
pub fn spawn_args(alert: &Alert, key: Option<&Key>) -> Vec<String> {
    let mut args = vec![
        "notify".to_string(),
        alert.event.to_string(),
        alert.value.to_string(),
    ];
    if let Some(key) = key {
        args.push(key.as_string());
    }
    args
}

/// Re-executes this binary as `notify <event> <value> [<key>]`, detached and
/// never waited on. The render path must not deliver inline: a notification
/// that blocks is a status line that stops refreshing. This mirrors
/// the scripts' `&` and `Start-Process -WindowStyle Hidden`.
pub fn spawn(alert: &Alert, key: Option<&Key>) {
    let Ok(exe) = std::env::current_exe() else {
        crate::debug::log(|| "notify spawn: cannot resolve current_exe".to_string());
        return;
    };
    let mut command = std::process::Command::new(exe);
    command
        .args(spawn_args(alert, key))
        // The child must not inherit this tick's pipes: a null stdin ends the
        // hook-payload read at once, and an inherited stdout would hold the
        // parent's pipe open past exit and stall its reader.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Without this a console window flashes on every crossing when the parent
    // has no console to inherit; the scripts never hit it because PowerShell
    // was already the console.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    match command.spawn() {
        // Not waited on: init reaps the child on Unix once this process exits.
        Ok(_) => crate::debug::log(|| format!("notify: {} fired at {}%", alert.event, alert.value)),
        Err(e) => crate::debug::log(move || format!("notify spawn failed: {e}")),
    }
}
