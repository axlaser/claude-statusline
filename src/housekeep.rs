//! `housekeep` — the cleanup hook Claude Code runs when a prompt is submitted
//! and when a turn goes idle, never per tick.
//!
//! Two jobs. It terminates processes that cancelled refreshes stranded on
//! Windows: `claude-statusline.exe` created suspended by MSYS2 and never
//! resumed, and two-argument `cygwin-console-helper.exe` whose bash died
//! before signalling it. And it deletes abandoned staging files and week-old
//! per-session state from the private state directory, on every platform.
//!
//! The decision is a pure function over `platform::ProcessFacts`, so the case
//! table drives it on every OS; `platform` only collects facts and terminates.
//! Off Windows the snapshot is empty and only the sweep does anything. Every
//! fact a rule cannot read means skip, never kill
//! (`docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::debug;
use crate::platform::{self, ParentStart, ProcessFacts};
use crate::session::StateRoot;

/// Set by the test harness: the pass skips process reclamation entirely, so
/// `cargo test` can never terminate a process on the machine running it.
pub const SKIP_RECLAIM_ENV: &str = "STATUSLINE_SKIP_PROCESS_RECLAIM";

/// How old a process must be before any rule may terminate it. A detached
/// `notify` child legitimately runs with a dead parent; this is what keeps a
/// rule away from it. It must outlast every bounded wait such a process
/// performs, which `the_age_floor_outlasts_every_bounded_wait` holds it to.
pub const STRANDED_AGE_FLOOR: Duration = Duration::from_secs(60);

/// Staging files (`.<name>.<pid>.tmp`) older than this were abandoned by a
/// write that died between the write and the rename.
pub const STAGING_MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// Per-session state untouched this long belongs to a session nobody resumed.
/// One that is resumed after it may repeat one threshold alert or re-read its
/// transcript once.
pub const STATE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The image a stranded tick runs, by its Toolhelp name at creation, so a file
/// the installer has since renamed to `.old` still matches.
const TICK_IMAGE: &str = "claude-statusline.exe";
const HELPER_IMAGE: &str = "cygwin-console-helper.exe";
/// Where Git for Windows and MSYS2 install the helper; lowercase, as compared.
const HELPER_PATH_SUFFIX: &str = r"\usr\bin\cygwin-console-helper.exe";

/// The prefixes of the seven state families. The two subagent families share
/// `statusline-sa-`. `statusline-oc-` is not one: nothing writes it here.
const FAMILIES: [&str; 6] = [
    "statusline-git-",
    "statusline-tasks-",
    "statusline-notify-",
    "statusline-tokens-",
    "statusline-focus-",
    "statusline-sa-",
];

/// `FILETIME` units per second.
const FILETIME_PER_SEC: u64 = 10_000_000;

/// How long the pass waits, on all terminated handles together, for their
/// images to unmap: an installer removes the old binary right after.
const EXIT_WAIT: Duration = Duration::from_secs(1);

/// What one pass compares every candidate against. `None` is a fact this
/// process could not read about itself, which skips every rule needing it.
pub struct Context {
    /// The snapshot's `taken`, in `FILETIME` units.
    pub taken: u64,
    pub own_pid: u64,
    /// `current_exe()`, as it reports it.
    pub own_exe: Option<String>,
    /// `platform::current_owner()`.
    pub owner: Option<u64>,
}

/// Why a candidate was left alone; counted per pass for the debug line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// This process.
    Own,
    /// Neither image a rule covers.
    Name,
    /// A fact the rule needs could not be read.
    Unreadable,
    /// A tick that has run: more than one thread, or CPU time.
    Running,
    /// A tick outside this binary's directory, or a helper outside `usr\bin`.
    Elsewhere,
    /// A helper without exactly two arguments: three serve a pseudo console.
    Arguments,
    /// Created at or after the snapshot: the PID was reused since.
    Reused,
    /// Younger than `STRANDED_AGE_FLOOR`.
    Young,
    ParentAlive,
    /// Another user's process.
    Foreign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Terminate,
    Skip(Reason),
}

/// The stranded-tick and stranded-helper rules. Every condition must hold;
/// the first that fails names the reason. Paths are compared as strings,
/// `\\?\` stripped and lowercased, because the policy is tested on hosts where
/// `\` is no separator.
pub fn judge(f: &ProcessFacts, cx: &Context) -> Verdict {
    use Reason::*;
    let skip = Verdict::Skip;
    if f.pid == cx.own_pid {
        return skip(Own);
    }
    if !covers(&f.name) {
        return skip(Name);
    }
    let tick = f.name.eq_ignore_ascii_case(TICK_IMAGE);
    let (Some(image), Some(created), Some(owner), Some(me)) =
        (&f.image, f.created, f.owner, cx.owner)
    else {
        return skip(Unreadable);
    };
    let image = normalized(&image.to_string_lossy());
    if tick {
        let (Some(cpu), Some(own_exe)) = (f.cpu, &cx.own_exe) else {
            return skip(Unreadable);
        };
        if f.threads != 1 || cpu != 0 {
            return skip(Running);
        }
        // The directory only, never the file name: what the image query
        // reports after the installer's rename is not worth depending on.
        let own_exe = normalized(own_exe);
        match (directory(&image), directory(&own_exe)) {
            (Some(theirs), Some(ours)) if theirs == ours => {}
            _ => return skip(Elsewhere),
        }
    } else {
        if !image.ends_with(HELPER_PATH_SUFFIX) {
            return skip(Elsewhere);
        }
        match f.args {
            None => return skip(Unreadable),
            Some(2) => {}
            Some(_) => return skip(Arguments),
        }
    }
    if created >= cx.taken {
        return skip(Reused);
    }
    if cx.taken - created < STRANDED_AGE_FLOOR.as_secs() * FILETIME_PER_SEC {
        return skip(Young);
    }
    // Focus capture's rule: a parent PID held by a process younger than the
    // child was reused, and the real parent is gone.
    match f.parent {
        ParentStart::Gone => {}
        ParentStart::At(start) if start > created => {}
        ParentStart::At(_) => return skip(ParentAlive),
        ParentStart::Unreadable => return skip(Unreadable),
    }
    if owner != me {
        return skip(Foreign);
    }
    Verdict::Terminate
}

/// Whether a rule covers an image of this Toolhelp name.
fn covers(name: &str) -> bool {
    name.eq_ignore_ascii_case(TICK_IMAGE) || name.eq_ignore_ascii_case(HELPER_IMAGE)
}

fn normalized(path: &str) -> String {
    path.strip_prefix(r"\\?\").unwrap_or(path).to_lowercase()
}

fn directory(path: &str) -> Option<&str> {
    path.rsplit_once(['\\', '/']).map(|(dir, _)| dir)
}

/// Deletes abandoned staging files and week-old state from the state
/// directory, and returns how many it removed.
///
/// Only when `root` is the guarded directory and it verifies private now: the
/// flat fallback is the shared temp root, where deleting predictable names is
/// what `docs/performance.md` §4 refused. Never creates the directory. Acts on
/// regular files only, by their own metadata, so a link or reparse point is
/// neither followed nor removed; a future mtime reads young, and a file a
/// concurrent pass removed first is no error.
pub fn sweep(root: &StateRoot, now: SystemTime) -> usize {
    if !root.is_guarded() || platform::dir_verdict(root.path()) != platform::DirVerdict::Private {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(root.path()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(limit) = name.to_str().and_then(max_age) else {
            continue;
        };
        // The entry's own metadata, which never traverses a link: what
        // `symlink_metadata` reads, without a second call per file on Windows.
        let Ok(md) = entry.metadata() else {
            continue;
        };
        let stale = md.is_file()
            && md
                .modified()
                .is_ok_and(|m| now.duration_since(m).is_ok_and(|age| age > limit));
        if !stale {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let path = entry.path().display().to_string();
                debug::log(move || format!("housekeep: cannot remove {path}: {e}"));
            }
        }
    }
    removed
}

/// How old a file named `name` may grow before the sweep removes it, or `None`
/// for a name it never touches. Staging names are `state::write_guarded`'s:
/// `.` + the final name + `.<pid>.tmp`.
fn max_age(name: &str) -> Option<Duration> {
    let family = |n: &str| FAMILIES.iter().any(|f| n.starts_with(f));
    match name.strip_prefix('.') {
        Some(staged) => {
            let (target, pid) = staged.strip_suffix(".tmp")?.rsplit_once('.')?;
            let pid_shaped = !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit());
            (pid_shaped && family(target)).then_some(STAGING_MAX_AGE)
        }
        None => family(name).then_some(STATE_MAX_AGE),
    }
}

/// What one reclamation pass did, for the debug line.
#[derive(Default)]
struct Tally {
    /// Covered processes opened and judged. This process is skipped before
    /// it is opened, so it counts under `skipped`'s `Own` and not here.
    candidates: usize,
    terminated: usize,
    /// Judged stranded, but `TerminateProcess` failed.
    refused: usize,
    /// Terminated, but not exited when the shared wait ran out.
    not_exited: Vec<u64>,
    skipped: BTreeMap<Reason, usize>,
}

/// Opens every other process a rule's image name covers, judges it on that
/// one handle, terminates the stranded ones through the same handle, then
/// waits for them once. Toolhelp's name filters first, so nothing else is
/// opened, and this process never is: `judge` would skip it anyway.
fn reclaim() -> Tally {
    let snapshot = platform::process_snapshot();
    let cx = Context {
        taken: snapshot.taken,
        own_pid: u64::from(std::process::id()),
        own_exe: std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        owner: platform::current_owner(),
    };
    let mut tally = Tally::default();
    let mut terminated = Vec::new();
    for (&pid, entry) in &snapshot.entries {
        if !covers(&entry.name) {
            continue;
        }
        if pid == cx.own_pid {
            *tally.skipped.entry(Reason::Own).or_default() += 1;
            continue;
        }
        let Some(candidate) = platform::open_candidate(&snapshot, pid) else {
            continue;
        };
        tally.candidates += 1;
        match judge(&candidate.facts, &cx) {
            Verdict::Terminate if candidate.terminate() => terminated.push(candidate),
            Verdict::Terminate => tally.refused += 1,
            Verdict::Skip(reason) => *tally.skipped.entry(reason).or_default() += 1,
        }
    }
    tally.terminated = terminated.len();
    tally.not_exited = platform::wait_for_exit(&terminated, EXIT_WAIT);
    tally
}

/// One pass: reclaim, unless the test harness switched it off, then sweep;
/// one debug line either way. Prints nothing.
pub fn run(root: &StateRoot) {
    let tally = std::env::var_os(SKIP_RECLAIM_ENV).is_none().then(reclaim);
    let removed = sweep(root, SystemTime::now());
    debug::log(move || match tally {
        Some(t) => format!(
            "housekeep: candidates={} terminated={} refused={} not_exited={:?} \
             skipped={:?} files_removed={removed}",
            t.candidates, t.terminated, t.refused, t.not_exited, t.skipped
        ),
        None => format!("housekeep: reclaim=off files_removed={removed}"),
    });
}
