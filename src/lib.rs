//! claude-statusline: one multi-call binary replacing the three per-platform
//! script trees.
//!
//! The library half exists so the single integration test file can reach
//! internal behaviour — the state guards and the clock — which a binary-only
//! crate cannot expose.

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

use std::path::PathBuf;

/// The user's home directory, however this platform spells it.
///
/// One resolver for the whole crate: two of them would eventually disagree, and
/// every predictable state path is derived from this one.
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

/// The payload the `self-check` subcommand renders.
///
/// The same file the `self-check` case feeds to the scripts, compiled in.
pub const SELF_CHECK_PAYLOAD: &str = include_str!("../tests/harness/payloads/minimal.json");

/// Output the `self-check` subcommand compares against.
///
/// `include_str!` of the fixture the case table asserts, never a
/// hand-maintained literal: a literal drifts from the renderer on the first
/// render change and then fails every install, which the installer answers by
/// refusing to upgrade. The three platforms' captures of this case are identical, so which
/// one is compiled in does not matter — the case table holds that claim.
pub const SELF_CHECK_FIXTURE: &str =
    include_str!("../tests/fixtures/statusline/self-check/expected/linux.txt");

/// The working directory the compiled-in payload's `{REPO}` placeholder stands
/// in for.
///
/// A fixed synthetic path, never the real working directory. The row renders a
/// truncated two-segment tail, so any path ending `/repo/work` reproduces the
/// capture — and consulting the real directory would render a git row on a
/// developer's machine and none on a server, failing the check for a reason
/// that has nothing to do with the binary.
const SELF_CHECK_REPO: &str = "/claude-statusline/repo/work";

/// The instant the case pins. Nothing in this payload renders a time, so it
/// only has to be fixed, not meaningful.
const SELF_CHECK_CLOCK: i64 = 1_767_225_600;

/// Renders the built-in fixture and compares it to the built-in expectation.
///
/// Exempt from the exit-0 catch: this is the installer's only guard against
/// placing a binary that launches but renders wrongly, so it has to be able to
/// fail.
///
/// Deliberately not routed through `cmd::statusline::run`. That would resolve
/// the git working directory, read the home directory, and stat the temp root —
/// state the installing machine has no reason to match, every bit of it able to
/// fail an otherwise perfect binary. The renderer is what the check is about.
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
        // Every non-payload input is absent, which is exactly the state the
        // `self-check` case captures: no repository, no transcript, no feed.
        home: None,
        git: None,
        scan: None,
        record: None,
        subagents: &[],
        now: SELF_CHECK_CLOCK,
    })
}
