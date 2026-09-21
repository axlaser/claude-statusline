//! claude-statusline: one multi-call binary replacing the three per-platform
//! script trees. The library half exists so the single integration test file
//! can reach internal behaviour a binary-only crate cannot expose.

pub mod clock;
pub mod cmd;
pub mod config;
pub mod debug;
pub mod entry;
pub mod focus;
pub mod git;
pub mod notify_state;
pub mod payload;
pub mod platform;
pub mod render;
pub mod session;
pub mod settings;
pub mod state;
pub mod subagent;
pub mod transcript;
pub mod update;

use std::path::PathBuf;

/// The user's home directory, however this platform spells it. The one
/// resolver for the crate: every predictable state path derives from it.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// `~/.claude`, where Claude Code keeps `settings.json` and this tool keeps its
/// config, its data stores, and its binary.
pub fn claude_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude"))
}

/// The payload `self-check` renders: the same file the `self-check` case feeds
/// to the scripts, compiled in.
pub const SELF_CHECK_PAYLOAD: &str = include_str!("../tests/harness/payloads/minimal.json");

/// Output `self-check` compares against: the fixture the case table asserts,
/// never a hand-maintained literal, which would drift on the first render
/// change and then fail every install. The three platforms' captures are
/// identical (the case table holds that claim), so which one is compiled in
/// does not matter.
pub const SELF_CHECK_FIXTURE: &str =
    include_str!("../tests/fixtures/statusline/self-check/expected/linux.txt");

/// What the compiled-in payload's `{REPO}` placeholder stands in for: a fixed
/// synthetic path, never the real working directory. The row renders a
/// two-segment tail, so any path ending `/repo/work` reproduces the capture,
/// whereas the real directory would render a git row on some machines and
/// fail the check for a reason unrelated to the binary.
const SELF_CHECK_REPO: &str = "/claude-statusline/repo/work";

/// The instant the case pins. Nothing in this payload renders a time, so it
/// only has to be fixed, not meaningful.
const SELF_CHECK_CLOCK: i64 = 1_767_225_600;

/// Renders the built-in fixture and compares it to the built-in expectation.
///
/// Exempt from the exit-0 catch: the installer's only guard against a binary
/// that launches but renders wrongly, so it has to be able to fail. Not routed
/// through `cmd::statusline::run`, which would consult git, the home directory
/// and the temp root: machine state that can fail a perfect binary.
pub fn self_check() -> (String, i32) {
    let rendered = if std::env::var_os("STATUSLINE_FORCE_SELFCHECK_MISMATCH").is_some() {
        "self-check mismatch\n".to_string()
    } else {
        render_self_check()
    };

    let code = if rendered == SELF_CHECK_FIXTURE { 0 } else { 1 };
    (rendered, code)
}

fn render_self_check() -> String {
    let payload_text = SELF_CHECK_PAYLOAD.replace("{REPO}", SELF_CHECK_REPO);
    let Some(payload) = payload::Payload::parse(&payload_text) else {
        return String::new();
    };
    render::render(&render::Inputs {
        payload: &payload,
        // Every non-payload input absent, as the `self-check` case captures.
        home: None,
        git: None,
        scan: None,
        record: None,
        subagents: &[],
        now: SELF_CHECK_CLOCK,
        update_available: false,
    })
}
