//! The git row: branch, working-tree counts, upstream divergence, and stashes.
//!
//! Git is invoked as a subprocess. A pure-Rust implementation would have to
//! reimplement git's *configuration* surface — `core.autocrlf` normalisation
//! before `--shortstat` counts lines, `.gitattributes` rules, rename detection,
//! untracked-directory collapsing — and fixtures are captured on default-config
//! scratch repos, so a divergence there would pass CI and render a plausible
//! wrong number on a user's machine, where silent degradation guarantees it
//! never announces itself. Calling `git` cannot diverge from `git`.
//!
//! [`parse_porcelain_v2`] is a pure function over text: it unit-tests without
//! a repository and is the seam a later `gix` evaluation would be measured
//! against.
//!
//! The 5-second TTL is ported because subprocess cost is real — 73.9 ms for
//! the pair on the maintainer's Windows machine. Its expiry is also a
//! correctness device: the cache is keyed on `.git/index` mtime, which neither
//! an untracked file nor `git fetch` touches, so without the expiry those rows
//! would stay wrong indefinitely.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::clock::Clock;
use crate::debug;
use crate::session;
use crate::state;

/// How long a cached git reading may be reused; also the staleness bound.
pub const TTL_SECS: i64 = 5;

/// The field separator in the cache record, matching the scripts byte for byte.
const SEP: char = '\x1f';

/// Everything the git row renders.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// Empty means no git segment: no repository, unreadable, or unborn HEAD.
    pub branch: String,
    pub insertions: u64,
    pub deletions: u64,
    pub untracked: u64,
    pub ahead: u64,
    pub behind: u64,
    pub stash: u64,
    /// The browsable `https://` form of `remote.origin.url`, or empty. Empty is
    /// the ordinary case: no origin, a non-HTTP remote, or a URL carrying
    /// credentials -- see [`normalize_remote`].
    pub remote: String,
}

/// One `--porcelain=v2 --branch --show-stash` reading, before the branch traps.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Porcelain {
    pub branch: String,
    /// `(initial)` on an unborn HEAD — a sentinel, not an object id.
    pub head_oid: String,
    pub untracked: u64,
    pub ahead: u64,
    pub behind: u64,
    pub stash: u64,
}

/// Parses porcelain v2 output. Pure over text, so every trap below is testable
/// without the repository state that produces it. All are real git output:
///
/// - **No `# branch.ab` line** without an upstream; ahead and behind stay 0.
/// - **No `# stash` line** without stashes, rather than a zero.
/// - **`# branch.oid (initial)`** on an unborn HEAD: a word where a hash goes.
/// - **`# branch.head (detached)`**, which a branch may also literally be
///   *named*. This reports what git said; disambiguating is
///   [`resolve_branch`]'s job, and it costs another subprocess.
pub fn parse_porcelain_v2(text: &str) -> Porcelain {
    let mut out = Porcelain::default();
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            out.branch = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.oid ") {
            out.head_oid = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // `+3 -0`. The scripts take the first space-delimited token and the
            // last, then strip one leading sign from each, so a malformed line
            // with no space reads both counts from the same token.
            out.ahead = digits(
                first_token(rest)
                    .strip_prefix('+')
                    .unwrap_or(first_token(rest)),
            );
            out.behind = digits(
                last_token(rest)
                    .strip_prefix('-')
                    .unwrap_or(last_token(rest)),
            );
        } else if let Some(rest) = line.strip_prefix("# stash ") {
            out.stash = digits(rest);
        } else if line.starts_with("? ") {
            out.untracked += 1;
        }
    }
    out
}

/// `^[0-9]+$` or zero, which is the scripts' guard against a malformed field
/// reaching arithmetic.
fn digits(s: &str) -> u64 {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().unwrap_or(0)
    } else {
        0
    }
}

fn first_token(s: &str) -> &str {
    match s.find(' ') {
        Some(i) => &s[..i],
        None => s,
    }
}

fn last_token(s: &str) -> &str {
    match s.rfind(' ') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

/// Pulls the `N insertion` / `N deletion` counts out of `git diff --shortstat`.
///
/// Absent counts stay zero: `--shortstat` omits the clause when a side is
/// empty, so ` 1 file changed, 3 insertions(+)` has no deletion count.
pub fn parse_shortstat(text: &str) -> (u64, u64) {
    (
        count_before(text, " insertion"),
        count_before(text, " deletion"),
    )
}

/// The digit run immediately preceding `label`, mirroring the scripts'
/// `([0-9]+)\ <label>` match. Occurrences without a digit run are skipped
/// rather than ending the search, which is what backtracking would do.
fn count_before(text: &str, label: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(label) {
        let at = from + rel;
        let mut start = at;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start < at {
            return text[start..at].parse().unwrap_or(0);
        }
        from = at + 1;
    }
    0
}

/// Applies the two branch traps to a porcelain reading.
///
/// `detached_hash` is a closure consulted only on the `(detached)` sentinel,
/// so a branch merely *named* `(detached)` in a test does not pay the extra
/// subprocess.
///
/// Unborn HEAD renders **no git segment, on every platform** — a resolved
/// divergence that breaks Windows, which substitutes the literal `HEAD`. A
/// bare `git init` has no `.git/index`, on which the whole git block is
/// guarded, so that repo already renders no segment anywhere and cannot change
/// without redesigning the index-mtime cache key; keeping the literal would
/// make the row's presence depend on whether an index file happens to exist.
/// `HEAD` is also simply wrong: porcelain reports `# branch.head main`.
pub fn resolve_branch(p: &Porcelain, detached_hash: impl FnOnce() -> Option<String>) -> String {
    if p.branch == "(detached)" {
        // Ask git for the abbreviation rather than truncating the oid: git
        // lengthens abbreviations for uniqueness, so a fixed width can differ.
        return match detached_hash() {
            Some(h) if !h.is_empty() => h,
            _ => "HEAD".to_string(),
        };
    }
    if p.head_oid == "(initial)" {
        return String::new();
    }
    p.branch.clone()
}

/// Where this session's git reading is cached.
///
/// Same name and record shape as the scripts, deliberately: `git-refresh`
/// ports the deletion of exactly `statusline-git-<id>.txt`, so caching
/// anywhere else would keep a stale git row alive through every file-modifying
/// tool call. `temp` is passed rather than read from the environment so a
/// fixture replay can stage a clean root per case.
pub fn cache_path(temp: &Path, session_id: &str) -> Option<PathBuf> {
    let safe = session::sanitize_session_id(session_id);
    if safe.is_empty() {
        return None;
    }
    Some(temp.join(format!("statusline-git-{safe}.txt")))
}

/// Parses a cache record into the index mtime it was taken at and its reading.
pub fn parse_cache_record(raw: &str) -> Option<(i64, GitStatus)> {
    let fields: Vec<&str> = raw.trim_end_matches(['\r', '\n']).split(SEP).collect();
    // A record from a build before the remote field simply fails to parse and
    // is refetched: the field count is the format version.
    if fields.len() != 9 {
        return None;
    }
    let mtime = fields[0].parse().ok()?;
    Some((
        mtime,
        GitStatus {
            branch: fields[1].to_string(),
            insertions: digits(fields[2]),
            deletions: digits(fields[3]),
            untracked: digits(fields[4]),
            ahead: digits(fields[5]),
            behind: digits(fields[6]),
            stash: digits(fields[7]),
            // Re-normalised on the way out: a planted record is untrusted text
            // and must not reach a link handler unchecked.
            remote: normalize_remote(fields[8]).unwrap_or_default(),
        },
    ))
}

/// Renders a cache record. Numeric fields come back in through [`digits`], so
/// a planted value costs a zero rather than reaching arithmetic.
pub fn cache_record(index_mtime: i64, s: &GitStatus) -> String {
    format!(
        "{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}",
        index_mtime,
        s.branch,
        s.insertions,
        s.deletions,
        s.untracked,
        s.ahead,
        s.behind,
        s.stash,
        s.remote
    )
}

/// `.git/config` is read no further than this. It is an ini file a few hundred
/// bytes long in practice; the bound is what keeps a planted one from being
/// read whole.
const CONFIG_PREFIX: u64 = 64 * 1024;

/// `remote.origin.url` out of `.git/config` text. Pure over the file contents.
///
/// Only `origin` -- a repo with several remotes has no single right answer, and
/// guessing one is worse than rendering no link. Section headers are matched on
/// the exact `[remote "origin"]` spelling git writes; the subsection name is
/// case-sensitive in git, so this is not a case-insensitive compare.
pub fn parse_origin_url(config: &str) -> Option<&str> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.starts_with("[remote \"origin\"]");
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") {
            return Some(value.trim());
        }
    }
    None
}

/// The browsable `https://` form of a remote URL, or `None` when there is not
/// one worth linking.
///
/// Three refusals matter more than the conversions:
///
/// - **Credentials.** `https://x-access-token:ghp_...@host/owner/repo` is a
///   valid remote and a real token. Handing it to a terminal's link handler
///   would put it in a browser's history and address bar, so a URL carrying
///   userinfo is refused outright rather than stripped -- stripping would
///   silently produce a working link from a file the user may not know leaks.
/// - **Scheme.** Only `http`/`https` and the `git@host:path` SSH shorthand
///   convert. `file://`, `git://` and anything unrecognised render no link.
/// - **Control characters.** `hyperlink` guards these too, but refusing here
///   keeps a mangled value out of the cache record as well.
pub fn normalize_remote(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) || raw.contains(SEP) {
        return None;
    }

    // `git@github.com:owner/repo.git` -> `github.com/owner/repo`
    let rest = if let Some(tail) = raw.strip_prefix("git@") {
        let (host, path) = tail.split_once(':')?;
        format!("{host}/{}", path.trim_start_matches('/'))
    } else {
        raw.strip_prefix("https://")
            .or_else(|| raw.strip_prefix("http://"))
            .or_else(|| raw.strip_prefix("ssh://"))?
            .to_string()
    };

    // Userinfo survives every branch above, so the check goes here once.
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') || authority.is_empty() || !rest.contains('/') {
        return None;
    }

    let trimmed = rest.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    Some(format!("https://{trimmed}"))
}

/// `remote.origin.url` for the repository at `cwd`, already normalised.
///
/// A file read, deliberately not `git config --get`: a second subprocess on the
/// per-tick path is the one thing `docs/performance.md` forbids outright, and
/// `status` has already established that `cwd/.git` is a real directory.
fn read_remote(cwd: &Path) -> String {
    let path = cwd.join(".git").join("config");
    let Some(bytes) = state::read_trusted_prefix(&path, CONFIG_PREFIX) else {
        return String::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return String::new();
    };
    parse_origin_url(&text)
        .and_then(normalize_remote)
        .unwrap_or_default()
}

/// The directory the git row describes: the payload's, or this process's own.
pub fn resolve_cwd(payload_git_cwd: &str) -> PathBuf {
    if payload_git_cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(payload_git_cwd)
    }
}

/// Reads the git row, through the cache when it is fresh.
///
/// `None` means no git segment. The `.git/index` test decides that, and it is
/// the scripts' test verbatim — so a linked worktree or submodule, where
/// `.git` is a file, renders no git row today. Changing it would be a feature.
pub fn status(
    clock: &dyn Clock,
    temp: &crate::session::StateRoot,
    cwd: &Path,
    session_id: &str,
) -> Option<GitStatus> {
    let index = cwd.join(".git").join("index");
    if !index.is_file() {
        return None;
    }
    let index_mtime = clock.mtime_unix(&index).unwrap_or(0);
    let cache = cache_path(temp, session_id);

    if let Some(path) = cache.as_deref() {
        if let Some(hit) = read_fresh_cache(clock, path, index_mtime) {
            debug::log(|| "git: cache hit".to_string());
            return Some(hit);
        }
    }

    let fresh = read_from_git(cwd);

    if let Some(path) = cache.as_deref() {
        // Reported, not discarded: a cache that never lands means every tick
        // pays the subprocess forever with no other signal, the blind spot
        // `docs/solutions/best-practices/byte-diff-cannot-see-cache-hit-regressions.md`
        // exists to close. The path is named so a reader triaging a stale row
        // knows *which* file refused the write.
        let outcome =
            state::write_guarded_under(temp, path, cache_record(index_mtime, &fresh).as_bytes());
        if outcome != state::WriteOutcome::Written {
            let p = path.display().to_string();
            debug::log(move || format!("git: cache not persisted to {p}: {outcome:?}"));
        }
    }
    Some(fresh)
}

fn read_fresh_cache(clock: &dyn Clock, path: &Path, index_mtime: i64) -> Option<GitStatus> {
    let bytes = state::read_trusted(path)?;
    let text = String::from_utf8(bytes).ok()?;
    let (recorded_mtime, status) = parse_cache_record(&text)?;
    if recorded_mtime != index_mtime {
        return None;
    }
    // The record ages from its write, and both halves go through the injected
    // clock so a fixture can reach both sides of the boundary without sleeping.
    match clock.age_secs(path) {
        Some(age) if age < TTL_SECS => Some(status),
        _ => None,
    }
}

fn read_from_git(cwd: &Path) -> GitStatus {
    // The remote is read regardless of what the subprocess returns: it is a
    // property of the repository, not of the working tree, so it survives a git
    // old enough to fail `--show-stash`.
    let mut out = GitStatus {
        remote: read_remote(cwd),
        ..GitStatus::default()
    };

    // `--show-stash` needs git >= 2.15; on older git the whole call fails and
    // the row renders empty through the same path as "no repository".
    let v2 = run_git(
        cwd,
        &["status", "--porcelain=v2", "--branch", "--show-stash"],
    );

    if let Some(text) = v2.as_deref().filter(|t| !t.is_empty()) {
        let parsed = parse_porcelain_v2(text);
        out.untracked = parsed.untracked;
        out.ahead = parsed.ahead;
        out.behind = parsed.behind;
        out.stash = parsed.stash;
        out.branch = resolve_branch(&parsed, || run_git(cwd, &["rev-parse", "--short", "HEAD"]));
    }

    if !out.branch.is_empty() {
        if let Some(stat) = run_git(cwd, &["diff", "--shortstat", "HEAD"]) {
            let (insertions, deletions) = parse_shortstat(&stat);
            out.insertions = insertions;
            out.deletions = deletions;
        }
    }

    out
}

/// How long one `git` invocation may take before it is killed. Two seconds is
/// past any healthy local status read, and short enough that a stalled one
/// degrades to a missing git row within a tick or two. It is deliberately under
/// the 5s cache TTL, so a timing-out repo refreshes on a healthy one's cadence.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How often the deadline is checked while the child runs.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// How long the stdout drain may still take once the child is resolved. The
/// child's deadline does not cover the drain: on the kill path nothing is left
/// of it, and that is exactly when the reader needs to notice the closed pipe.
/// The floor bounds the worst case at `GIT_TIMEOUT` plus this, still under the
/// 5s cache TTL, without discarding output that had already arrived.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Runs git and returns its trimmed stdout, or `None` for any failure.
///
/// `--no-optional-locks` keeps a status read from writing the index, which
/// would make the status line invalidate its own cache on every tick. Trailing
/// newlines are stripped because the scripts read these through `$(…)`.
fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        // Without this a console window flashes on every refresh when the
        // parent has no console of its own to inherit.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let stdout = run_bounded(command, GIT_TIMEOUT)?;
    let text = String::from_utf8_lossy(&stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string();
    Some(text)
}

/// Runs a prepared command and returns its raw stdout, bounded end to end.
/// `None` for any failure, including the deadline.
///
/// `run_git` is the only production caller; the debug lines keep their `git:`
/// prefix so the log stays greppable. The split lets the test file prove the
/// deadline with a child that sleeps past `timeout`, which the hardcoded `git`
/// invocation could not express without racing `PATH` across the test binary.
pub fn run_bounded(mut command: Command, timeout: std::time::Duration) -> Option<Vec<u8>> {
    // Deliberately not `output()`: it blocks unbounded on the render path, and
    // with this process respawned every couple of seconds a repo on a stalled
    // network mount left one blocked process per tick, without limit. The 5s
    // cache TTL bounds how *often* git runs, never how long it may block.
    command.stdout(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            debug::log(move || format!("git: cannot run: {e}"));
            return None;
        }
    };

    // Drained on a helper thread: a child that fills the pipe blocks on write,
    // so waiting for exit without reading would deadlock on a large status.
    // The buffer comes back over a channel rather than `join` because the drain
    // needs its own bound: killing the child closes only *its* write handle,
    // and a `core.fsmonitor` daemon it spawned keeps the pipe open (on Windows
    // `TerminateProcess` does not touch descendants at all), so a `join` would
    // block exactly as the `output()` this replaced did. The thread is left
    // running in that case; the process exits within milliseconds regardless.
    let mut pipe = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = pipe.as_mut() {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
        // A failed send means the deadline passed: that *is* the timeout.
        let _ = tx.send(buf);
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    debug::log(|| "git: killed at the deadline".to_string());
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                debug::log(move || format!("git: wait failed: {e}"));
                break None;
            }
        }
    };

    // The deadline's remainder, floored so the kill path still gets a moment.
    let budget = deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(DRAIN_GRACE);
    let stdout = match rx.recv_timeout(budget) {
        Ok(buf) => buf,
        Err(_) => {
            debug::log(|| "git: stdout drain did not finish before the deadline".to_string());
            return None;
        }
    };
    if !status?.success() {
        return None;
    }
    Some(stdout)
}
