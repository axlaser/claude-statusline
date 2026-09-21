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
    status_observed(clock, temp, cwd, session_id).0
}

/// [`status`], with the number of `git` children the call actually spawned.
///
/// Returned rather than counted in a global: `cargo test` runs these in
/// parallel, and a shared counter would make the one rule this unit can break
/// silently into a flaky assertion. See [`UPFRONT`] for why that rule matters.
pub fn status_observed(
    clock: &dyn Clock,
    temp: &crate::session::StateRoot,
    cwd: &Path,
    session_id: &str,
) -> (Option<GitStatus>, usize) {
    let index = cwd.join(".git").join("index");
    if !index.is_file() {
        return (None, 0);
    }
    let index_mtime = clock.mtime_unix(&index).unwrap_or(0);
    let cache = cache_path(temp, session_id);

    if let Some(path) = cache.as_deref() {
        if let Some(hit) = read_fresh_cache(clock, path, index_mtime) {
            debug::log(|| "git: cache hit".to_string());
            return (Some(hit), 0);
        }
    }

    let (fresh, spawned) = read_from_git(cwd);

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
    (Some(fresh), spawned)
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

/// One `git` invocation this tick may make.
///
/// Named rather than spelled inline so the process count per repo state is
/// something a test can assert. It is the one rule this unit can break
/// silently: every state renders identically whether the pair runs sequentially,
/// concurrently, or three children deep, so byte-diffing cannot see it move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    Status,
    Diff,
    RevParse,
}

impl Child {
    fn args(self) -> &'static [&'static str] {
        match self {
            // `--show-stash` needs git >= 2.15; on older git the whole call
            // fails and the row renders empty through the same path as
            // "no repository".
            Self::Status => &["status", "--porcelain=v2", "--branch", "--show-stash"],
            // See `run_git` for why the diff carries its own `-c`.
            Self::Diff => &[
                "-c",
                "diff.autoRefreshIndex=false",
                "diff",
                "--shortstat",
                "HEAD",
            ],
            Self::RevParse => &["rev-parse", "--short", "HEAD"],
        }
    }
}

/// The children spawned before anything is drained.
///
/// Both, always, on any tick that reaches `git` at all. They are independent
/// reads of the same repository and git's `lockfile.h` blocks only writers — a
/// loser of an `O_CREAT|O_EXCL` race returns `-1` silently rather than creating
/// a file — so overlapping them is safe and the pair costs `max(a, b)` instead
/// of `a + b`. Measured 98.0 ms sequential against 57.2 ms concurrent.
pub const UPFRONT: [Child; 2] = [Child::Status, Child::Diff];

fn read_from_git(cwd: &Path) -> (GitStatus, usize) {
    // The remote is read regardless of what the subprocess returns: it is a
    // property of the repository, not of the working tree, so it survives a git
    // old enough to fail `--show-stash`.
    let mut out = GitStatus {
        remote: read_remote(cwd),
        ..GitStatus::default()
    };

    // One deadline for the pair, taken before either is spawned. The children
    // overlap, so the bound covers the slower of the two rather than their sum:
    // two bounded calls in sequence were 4.5 s of worst case, and the
    // three-child detached-HEAD path 6.75 s, past the 5 s cache TTL.
    let deadline = std::time::Instant::now() + GIT_TIMEOUT;
    let status_child = spawn_git(cwd, Child::Status.args());
    let diff_child = spawn_git(cwd, Child::Diff.args());
    let mut spawned = usize::from(status_child.is_some()) + usize::from(diff_child.is_some());

    let v2 = status_child.and_then(|p| collect_git(p, deadline));
    let diff = diff_child.and_then(|p| collect_git(p, deadline));

    if let Some(text) = v2.as_deref().filter(|t| !t.is_empty()) {
        let parsed = parse_porcelain_v2(text);
        out.untracked = parsed.untracked;
        out.ahead = parsed.ahead;
        out.behind = parsed.behind;
        out.stash = parsed.stash;
        out.branch = resolve_branch(&parsed, || {
            // Deferred on purpose. Whether this is needed is known only once
            // status has parsed, so hoisting it would make three children the
            // common case — a strict increase on every tick, which §2 forbids.
            let child = spawn_git(cwd, Child::RevParse.args())?;
            spawned += 1;
            collect_git(child, deadline)
        });
    }

    // The gate consumes the diff's result; it does not decide whether the child
    // ran. An unborn HEAD resolves to an empty branch and still renders no git
    // segment, which is what this guards — moving the spawn must not move it.
    if !out.branch.is_empty() {
        if let Some(stat) = diff {
            let (insertions, deletions) = parse_shortstat(&stat);
            out.insertions = insertions;
            out.deletions = deletions;
        }
    }

    (out, spawned)
}

/// How long one `git` invocation may take before it is killed. Two seconds is
/// past any healthy local status read, and short enough that a stalled one
/// degrades to a missing git row within a tick or two. It is deliberately under
/// the 5s cache TTL, so a timing-out repo refreshes on a healthy one's cadence.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The longest the wait may sleep before re-checking the deadline.
///
/// It used to be how often the deadline was checked, full stop: the loop slept
/// this long between `try_wait` calls, so every invocation paid up to 5 ms of
/// pure latency noticing an exit that had already happened. The wait is now on
/// the drain channel, which wakes the moment the child closes stdout — its own
/// exit, for a healthy `git` — and this survives only as the upper bound. It
/// has to survive: a descendant that inherited the pipe can hold EOF back
/// indefinitely, which is the case `join()` was replaced for, and without a
/// bound that child would never be killed at its deadline.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// How long the reap naps between `try_wait` calls once the pipe has drained.
///
/// Once EOF has arrived there is nothing left to wake the loop early, and a
/// full poll interval here would hand straight back the latency the channel
/// wait removed. EOF and process termination are **not** the same instant: the
/// child closes its handles and the kernel then tears the process down, which
/// measured at about half a millisecond on Windows. A doubling backoff from
/// 100 us landed the reap a median 1.06 ms after EOF, most of it overshoot; a
/// flat nap this size lands it at 0.55 ms, which is the teardown itself.
const REAP_NAP: std::time::Duration = std::time::Duration::from_micros(50);

/// How long the reap naps at [`REAP_NAP`] before falling back to
/// [`POLL_INTERVAL`].
///
/// Bounds the fine-grained polling to about forty `try_wait` calls, for the
/// child that closes stdout and then keeps running — that one must not be
/// polled at 50 us for two seconds.
const REAP_FINE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2);

/// How long the stdout drain may still take once the child is resolved. The
/// child's deadline does not cover the drain: on the kill path nothing is left
/// of it, and that is exactly when the reader needs to notice the closed pipe.
/// The floor bounds the worst case at `GIT_TIMEOUT` plus this, still under the
/// 5s cache TTL, without discarding output that had already arrived.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// The real `git.exe` beside a Git-for-Windows stub on `PATH`, or `None`.
///
/// `C:\Program Files\Git\cmd\git.exe` is a 46 KB wrapper whose only job is to
/// set `MSYSTEM` and spawn the 4.2 MB binary in `mingw64\bin`. Calling the real
/// one directly removes a process and a `PATH` search from every invocation:
/// measured on the maintainer's machine at status 44.7 -> 31.6 ms and diff
/// 49.2 -> 35.7 ms, byte-identical output. Supported since Git for Windows
/// 2.25.1, which notices the missing `MSYSTEM` and does the stub's work itself
/// -- an accepted floor, recorded in `docs/performance.md` §7, because the only
/// way to *check* the version is to run `git --version`, a per-tick subprocess
/// the hard rule forbids.
///
/// `exists` is injected so the whole table is testable without a Git install.
/// The `cmd` test runs before any of them, so on a `PATH` where this layout
/// cannot occur -- every Unix one -- the walk costs string comparisons and no
/// syscalls at all, which is why this needs no platform branch.
pub fn real_git_beside_stub(
    path_var: Option<&std::ffi::OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for entry in std::env::split_paths(path_var?) {
        if !entry
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("cmd"))
        {
            continue;
        }
        // The stub itself must be there. Without this check a directory merely
        // named `cmd` beside an unrelated `mingw64` tree would be adopted.
        if !exists(&entry.join("git.exe")) {
            continue;
        }
        let Some(root) = entry.parent() else {
            continue;
        };
        // Both bitnesses: 32-bit Git for Windows ships `mingw32`.
        for arch in ["mingw64", "mingw32"] {
            let real = root.join(arch).join("bin").join("git.exe");
            if exists(&real) {
                return Some(real);
            }
        }
    }
    None
}

/// The program every `git` invocation runs, resolved once per process.
///
/// The fallback is unconditional and load-bearing: portable, MinGit, scoop and
/// package-manager layouts all differ, and an absent absolute path must degrade
/// to today's behaviour rather than to a missing git row.
pub fn git_program() -> &'static Path {
    static PROGRAM: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PROGRAM.get_or_init(|| {
        match real_git_beside_stub(std::env::var_os("PATH").as_deref(), |p| p.is_file()) {
            Some(real) => {
                let shown = real.display().to_string();
                debug::log(move || format!("git: bypassing the wrapper, using {shown}"));
                real
            }
            None => PathBuf::from("git"),
        }
    })
}

/// Spawns one `git` invocation, ready to be collected later.
///
/// `--no-optional-locks` keeps the *status* read from writing the index, which
/// would make the status line invalidate its own cache on every tick. It says
/// nothing about the other calls: it gates `cmd_status` alone, and the diff
/// call carries its own `-c diff.autoRefreshIndex=false` for the same reason.
fn spawn_git(cwd: &Path, args: &[&str]) -> Option<Pending> {
    let mut command = Command::new(git_program());
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

    spawn_bounded(command)
}

/// Drains one spawned `git` and returns its trimmed stdout.
///
/// Trailing newlines are stripped because the scripts read these through a
/// command substitution, which eats them.
fn collect_git(pending: Pending, deadline: std::time::Instant) -> Option<String> {
    let stdout = collect_bounded(pending, deadline).0?;
    Some(
        String::from_utf8_lossy(&stdout)
            .trim_end_matches(['\n', '\r'])
            .to_string(),
    )
}

/// What the bounded wait actually did.
///
/// The saving in U6 is latency in a *wait*, well inside the git-miss path's own
/// noise floor, so no median can show it. These are the observables that can.
/// A loop that has gone back to sleeping between `try_wait` calls never sets
/// `woken_by_pipe`, because it never touches the channel at all.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    /// The pipe reached EOF before the child was reaped, so the wait was woken
    /// by the child closing stdout rather than by its own timeout expiring.
    /// False is not a failure: a descendant that inherited the pipe holds EOF
    /// back, and then the interval bound is what resolves the wait.
    pub woken_by_pipe: bool,
    /// Time spent asleep between EOF and the successful reap. EOF and process
    /// termination are microseconds apart, so this is the window a full poll
    /// interval used to land in.
    pub napped_after_drain: std::time::Duration,
    /// How long after the pipe reached EOF the child was reaped, or `None` when
    /// the wait never happened because the child had already exited by the
    /// first `try_wait`.
    ///
    /// This is the latency itself rather than a proxy for it: whatever the wait
    /// is built from, a loop that notices the exit on its own schedule records
    /// up to a whole poll interval here, and one woken by the pipe records
    /// microseconds. `Option` rather than a zero, because "never waited" and
    /// "waited no time at all" are the same number and not the same fact — a
    /// test that could not tell them apart would pass a polling loop whenever
    /// the fixture child happened to be fast enough.
    pub eof_to_reap: Option<std::time::Duration>,
    /// The child outlived its deadline and was killed.
    pub killed: bool,
}

/// A spawned child and the thread draining its stdout.
///
/// Spawning and collecting are separate so a caller can put two children up
/// before draining either. Sequential `run_bounded` calls would cost their sum.
pub struct Pending {
    child: std::process::Child,
    rx: std::sync::mpsc::Receiver<(Vec<u8>, std::time::Instant)>,
}

/// Runs a prepared command and returns its raw stdout, bounded end to end.
/// `None` for any failure, including the deadline.
///
/// `read_from_git` spawns and collects separately; this is the single-child
/// shape, and the seam the test file uses to drive a child that sleeps past
/// `timeout` — which the hardcoded `git` invocation could not express without
/// racing `PATH` across the test binary. The debug lines keep their `git:`
/// prefix so the log stays greppable.
pub fn run_bounded(command: Command, timeout: std::time::Duration) -> Option<Vec<u8>> {
    run_bounded_observed(command, timeout).0
}

/// [`run_bounded`], with what the wait did alongside the output.
pub fn run_bounded_observed(
    command: Command,
    timeout: std::time::Duration,
) -> (Option<Vec<u8>>, Wait) {
    let deadline = std::time::Instant::now() + timeout;
    match spawn_bounded(command) {
        Some(pending) => collect_bounded(pending, deadline),
        None => (None, Wait::default()),
    }
}

/// Starts the child and the thread that drains it. `None` if it cannot start.
pub fn spawn_bounded(mut command: Command) -> Option<Pending> {
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
    //
    // The channel now does second duty as the wait itself, in `collect_bounded`.
    let mut pipe = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = pipe.as_mut() {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
        // The instant travels with the buffer so the reap latency can be
        // measured from EOF rather than from the wait's own bookkeeping.
        // A failed send means the deadline passed: that *is* the timeout.
        let _ = tx.send((buf, std::time::Instant::now()));
    });

    Some(Pending { child, rx })
}

/// Waits for one spawned child and returns its raw stdout.
///
/// `deadline` is absolute and may be shared by several children, which is what
/// makes a concurrent pair cost the slower of the two rather than their sum. A
/// child that has *already* finished is collected even past the deadline: the
/// loop tries the child before it looks at the clock, so a fast second child is
/// never killed for the sins of a slow first one.
pub fn collect_bounded(pending: Pending, deadline: std::time::Instant) -> (Option<Vec<u8>>, Wait) {
    /// Where the child's stdout has got to.
    enum Pipe {
        /// Still open: a wait on the channel can still be woken by EOF.
        Open,
        Drained(Vec<u8>),
        /// The drain thread went away without sending. Nothing will wake the
        /// loop now, and waiting on the channel again would spin.
        Broken,
    }

    let Pending { mut child, rx } = pending;
    let mut wait = Wait::default();
    let mut stdout = Pipe::Open;
    let mut drained_at: Option<std::time::Instant> = None;
    let mut eof_at: Option<std::time::Instant> = None;

    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                wait.eof_to_reap = eof_at.map(|at| at.elapsed());
                break Some(s);
            }
            Ok(None) => {
                let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                    let _ = child.kill();
                    let _ = child.wait();
                    wait.killed = true;
                    debug::log(|| "git: killed at the deadline".to_string());
                    // Returning here rather than draining first: the output of
                    // a killed child is discarded either way, and waiting the
                    // drain grace to discard it only delays the empty git row.
                    return (None, wait);
                };
                match stdout {
                    // Not a sleep. EOF arrives when the child closes stdout,
                    // which for a healthy `git` is its exit, so this returns at
                    // the exit rather than at the next multiple of the poll
                    // interval. The interval stays as the upper bound, for the
                    // descendant that holds EOF back.
                    Pipe::Open => {
                        let budget = left.min(POLL_INTERVAL);
                        match rx.recv_timeout(budget) {
                            Ok((buf, at)) => {
                                wait.woken_by_pipe = true;
                                eof_at = Some(at);
                                drained_at = Some(std::time::Instant::now());
                                stdout = Pipe::Drained(buf);
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                stdout = Pipe::Broken;
                            }
                        }
                    }
                    _ => {
                        let fine = drained_at.is_some_and(|at| at.elapsed() < REAP_FINE_WINDOW);
                        let nap = left.min(if fine { REAP_NAP } else { POLL_INTERVAL });
                        std::thread::sleep(nap);
                        wait.napped_after_drain += nap;
                    }
                }
            }
            Err(e) => {
                debug::log(move || format!("git: wait failed: {e}"));
                break None;
            }
        }
    };

    let stdout = match stdout {
        Pipe::Drained(buf) => buf,
        Pipe::Broken => {
            debug::log(|| "git: the stdout drain went away without reporting".to_string());
            return (None, wait);
        }
        // The child is resolved but a descendant still holds the pipe. The
        // deadline's remainder, floored so that case still gets a moment.
        Pipe::Open => {
            let budget = deadline
                .saturating_duration_since(std::time::Instant::now())
                .max(DRAIN_GRACE);
            match rx.recv_timeout(budget) {
                Ok((buf, _)) => buf,
                Err(_) => {
                    debug::log(|| {
                        "git: stdout drain did not finish before the deadline".to_string()
                    });
                    return (None, wait);
                }
            }
        }
    };
    match status {
        Some(s) if s.success() => (Some(stdout), wait),
        _ => (None, wait),
    }
}
