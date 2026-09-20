//! Guarded reads and writes of predictable-path state files.
//!
//! One module owns every guard so the failure that cost this project nine days
//! is unrepresentable: two sibling guards over one dependency failing in
//! opposite directions. See
//! `docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`.
//!
//! Fail directions, stated once, deliberately:
//!
//! - **Symlink / reparse point present** → refuse. This is the load-bearing
//!   guard against symlink planting in a shared temp directory.
//! - **Owner resolvable and foreign** → refuse.
//! - **Owner NOT resolvable** → *pass*, degrading to the symlink guard. Failing
//!   closed here is the exact inversion that silently killed every read-side
//!   cache on Windows and re-fired the context alert every two seconds.
//! - **File exists but cannot be parsed** → the caller's conservative value.
//!   For the notification latch that means "already notified", so a corrupt
//!   latch suppresses rather than spams. For the focus record it means "no
//!   record": the same symlink and foreign-owner refusals and the same
//!   undeterminable-owner pass as its siblings, and a missing, unparsable,
//!   over-size or unknown-version record makes the click do nothing while the
//!   next visual alert regenerates it.
//! - **Parent state directory unverifiable** → the two sites answer for
//!   different reasons, and both are recorded because a guard whose direction is
//!   not in this table is the shape of the defect above. At
//!   `session::state_dir_in`, an unreadable owner *passes*: failing closed there
//!   is survivable, since the resolver would simply fall back to the flat temp
//!   root, but it would cost the state directory on every machine where the
//!   lookup is unavailable. At `platform::create_private_dir`, an unreadable
//!   owner *passes* too, and there it is load-bearing: resolution has already
//!   committed to the subdirectory and no fallback remains, so failing closed
//!   would kill every state write on such a machine, silently and forever.

use std::path::Path;

use crate::platform;

/// `#[must_use]` because the failure directions below are invisible at runtime:
/// a dropped `SkippedHostile` or `Failed` means the caller's cache or record
/// never persisted, and the silent-degradation contract guarantees no other
/// signal. A caller that genuinely does not care still has to say so with
/// `let _ =`.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The bytes are on disk.
    Written,
    /// The target was hostile and could not be made safe, so nothing was
    /// written and nothing was followed.
    SkippedHostile,
    /// The write itself failed (permissions, disk, rename).
    Failed,
}

/// The tested fail-direction predicate.
///
/// `None` means "I could not ask the question", which is not the same as "the
/// answer is no" — collapsing the two is precisely the defect this project
/// already paid for.
pub fn owner_check_passes(owner: Option<u64>, trusted: &[u64]) -> bool {
    match owner {
        Some(o) => trusted.contains(&o),
        None => true,
    }
}

/// True when the path is a symlink/reparse point, or exists and is owned by
/// someone else.
fn is_hostile(path: &Path) -> bool {
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return false; // absent is not hostile
    };
    if md.file_type().is_symlink() {
        return true;
    }
    let trusted = platform::trusted_owners();
    // Our own identity is unknown: degrade to the symlink guard above.
    if trusted.is_empty() {
        return false;
    }
    !owner_check_passes(platform::file_owner(path), &trusted)
}

/// Reads `path` only when it passes the guard. `None` covers absent,
/// untrusted, and unreadable alike — the caller decides what that means.
pub fn read_trusted(path: &Path) -> Option<Vec<u8>> {
    if is_hostile(path) {
        return None;
    }
    std::fs::read(path).ok()
}

/// Removes a hostile path and **re-evaluates** the guard. `false` means the
/// path could not be made safe and nothing may be written to it.
///
/// The re-check is the load-bearing half: on a sticky directory the unlink
/// fails silently, and a remove-then-write without it would write straight
/// through an attacker's symlink into a victim-owned file.
fn make_safe(path: &Path) -> bool {
    if !is_hostile(path) {
        return true;
    }
    if std::fs::remove_file(path).is_err() {
        return false;
    }
    !is_hostile(path)
}

/// Writes atomically through the guard, inheriting whatever the parent
/// directory already is.
///
/// This is the right entry point for every parent this binary does not own:
/// `~/.claude`, the flat temp root on the fallback path, and the harness's
/// scratch roots. For a write into the guarded state directory, use
/// `write_guarded_under`.
pub fn write_guarded(path: &Path, bytes: &[u8]) -> WriteOutcome {
    write_inner(path, bytes, false)
}

/// Writes atomically through the guard, creating the parent privately when
/// `root` is the directory this binary owns.
///
/// Callers pass the root rather than a bare bool so the decision is made from
/// the same value that resolved the path, and so a path that is somehow not a
/// direct child of the root degrades to the inherited behaviour instead of
/// silently claiming a directory.
pub fn write_guarded_under(
    root: &crate::session::StateRoot,
    path: &Path,
    bytes: &[u8],
) -> WriteOutcome {
    let owns_parent = root.is_guarded() && path.parent() == Some(root.path());
    write_inner(path, bytes, owns_parent)
}

fn write_inner(path: &Path, bytes: &[u8], create_parent_privately: bool) -> WriteOutcome {
    if !make_safe(path) {
        return WriteOutcome::SkippedHostile;
    }

    let Some(parent) = path.parent() else {
        return WriteOutcome::Failed;
    };
    if create_parent_privately {
        // The one directory this binary creates in a location it does not
        // control. `create_dir_all` cannot be used here: it returns `Ok` when
        // the path is a symlink to a directory, because `mkdir` reports
        // `EEXIST` and `Path::is_dir()` then follows the link and finds a
        // directory. One level down from the temp root that turns an unverified
        // adoption into the ordinary case.
        //
        // Residual TOCTOU, recorded at its true width, because an
        // understatement here is worse than none: between
        // `session::state_dir_in`'s read-only verdict and this creation, every
        // path built from the root traverses the parent unverified. The three
        // kinds of traversal do not have the same cover.
        //
        // Writes are covered twice: `create_private_dir` re-verifies through a
        // fresh handle, and `make_safe` plus `create_new` refuse a planted final
        // path. A plant landing in this window costs one tick of writes, because
        // the next tick's verdict sees the hostile directory and routes to the
        // flat root.
        //
        // Reads keep the per-file owner check in `read_trusted`, which is
        // carrying alone there.
        //
        // The two deletes have **no** per-file check at all — `remove_file` in
        // `cmd::git_refresh` and in `subagent`'s linger expiry both unlink
        // directly. Nothing is carrying, and unlike a write, a delete the plant
        // induced is not undone by the next tick. What bounds it is the name:
        // every path is `statusline-<sanitized session id>`, which an attacker
        // cannot choose. Routing those two through a guarded remove would close
        // it and is a behaviour change, so it needs its own case rather than a
        // quiet edit here.
        match crate::platform::create_private_dir(parent) {
            crate::platform::DirVerdict::Private => {}
            // Hostile and Failed are different answers and callers branch on
            // them differently — `cmd::subagent` maps them to different ticks
            // and different log lines.
            crate::platform::DirVerdict::Hostile => return WriteOutcome::SkippedHostile,
            crate::platform::DirVerdict::Absent => return WriteOutcome::Failed,
        }
    } else if std::fs::create_dir_all(parent).is_err() {
        return WriteOutcome::Failed;
    }

    // Temp-then-rename so a concurrent reader never sees a torn file. The
    // scripts do the same for the latch and the learned map.
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id()
    ));
    // The staging path is guarded too, and it is the one that matters most:
    // `rename` replaces the final path without ever following it, so the
    // symlink an attacker can actually exploit is the one planted at this
    // predictable temporary name. Both shell handlers drop a planted temp
    // target and skip the write if it survives; `make_safe` reproduces that,
    // and turns an undeletable plant into `SkippedHostile`.
    if !make_safe(&tmp) {
        return WriteOutcome::SkippedHostile;
    }
    // What `make_safe` cannot close is the window between its check and the
    // open — a link re-planted in that gap would still be followed by a plain
    // `write`. `create_new` (O_CREAT|O_EXCL on Unix, CREATE_NEW on Windows)
    // refuses anything that already exists, links included, so a plant landing
    // in the gap fails the write instead of redirecting it. The unlink first
    // is load-bearing the other way: a stale leftover from a crashed run at
    // this same pid-derived name would otherwise fail every write for the
    // rest of the session.
    let _ = std::fs::remove_file(&tmp);
    let staged = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, bytes));
    if staged.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return WriteOutcome::Failed;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return WriteOutcome::Failed;
    }
    WriteOutcome::Written
}

/// Whether the notification latch should suppress a repeat alert.
///
/// Absent → `false` (never notified). Present but unreadable or unparseable →
/// `true`, the conservative value: a corrupt latch must not re-fire the alert
/// on every tick.
pub fn latch_reads_as_notified(path: &Path) -> bool {
    if std::fs::symlink_metadata(path).is_err() {
        return false;
    }
    let Some(bytes) = read_trusted(path) else {
        return true;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return true;
    };
    match value.as_object() {
        Some(map) => map
            .iter()
            .any(|(k, v)| k.starts_with("notified_") && v.as_bool() == Some(true)),
        None => true,
    }
}
