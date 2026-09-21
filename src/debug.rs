//! `STATUSLINE_DEBUG` logging: the second half of Silent Degradation. Never
//! write to stderr, and log errors here, or a field failure is
//! indistinguishable from none. The message is a closure so its cost is never
//! paid when logging is off.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by [`enable`]: logging switched on by a caller, not the environment.
static FORCED: AtomicBool = AtomicBool::new(false);

/// True when `STATUSLINE_DEBUG` is set to anything other than empty or `0`,
/// or when [`enable`] was called.
pub fn is_enabled() -> bool {
    if FORCED.load(Ordering::Relaxed) {
        return true;
    }
    match std::env::var("STATUSLINE_DEBUG") {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// Switches logging on for the rest of this process. The OS launches the click
/// handlers without the session's environment, so the flag travels in the
/// focus record and is applied here. Nothing switches it off again.
pub fn enable() {
    FORCED.store(true, Ordering::Relaxed);
}

/// Default log path: `~/.claude/statusline-debug.log`, matching what the
/// scripts write and what README's troubleshooting section tells users to read.
pub fn default_path() -> Option<PathBuf> {
    crate::claude_dir().map(|d| d.join("statusline-debug.log"))
}

/// Appends one line to `path` when `enabled`. Never evaluates `msg` otherwise.
/// Every failure is swallowed: a broken debug log must not become a visible
/// failure of the status line itself.
pub fn log_to<F>(path: &Path, enabled: bool, msg: F)
where
    F: FnOnce() -> String,
{
    if !enabled {
        return;
    }
    let line = msg();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// [`log_to`] with the ambient enable flag and the default path.
pub fn log<F>(msg: F)
where
    F: FnOnce() -> String,
{
    if !is_enabled() {
        return;
    }
    if let Some(p) = default_path() {
        log_to(&p, true, msg);
    }
}
