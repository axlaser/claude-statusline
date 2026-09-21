//! Guarded reads and writes of predictable-path state files.
//!
//! One module owns every guard so two sibling guards over one dependency can
//! never fail in opposite directions, the defect that cost this project nine
//! days. See
//! `docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`.
//!
//! Fail directions, stated once:
//!
//! - **Symlink / reparse point present** → refuse. The load-bearing guard
//!   against symlink planting in a shared temp directory.
//! - **Owner resolvable and foreign** → refuse.
//! - **Owner NOT resolvable** → *pass*, degrading to the symlink guard. Failing
//!   closed here is the inversion that silently killed every read-side cache
//!   on Windows and re-fired the context alert every two seconds.
//! - **File exists but cannot be parsed** → the caller's conservative value.
//!   For the notification latch that is "already notified", so a corrupt latch
//!   suppresses rather than spams. For the focus record it is "no record", with
//!   the same symlink and foreign-owner refusals and undeterminable-owner pass
//!   as its siblings; a missing, unparsable, over-size or unknown-version
//!   record makes the click do nothing until the next visual alert regenerates
//!   it.
//! - **Parent state directory unverifiable** → an unreadable owner *passes* at
//!   both sites, for different reasons. At `session::state_dir_in` failing
//!   closed would be survivable, a fallback to the flat temp root, but would
//!   cost the state directory on every machine where the lookup is
//!   unavailable. At `platform::create_private_dir` resolution has already
//!   committed to the subdirectory and no fallback remains, so failing closed
//!   would kill every state write on such a machine, silently and forever.

use std::path::Path;

use crate::platform;

/// `#[must_use]` because a dropped `SkippedHostile` or `Failed` means the
/// caller's record never persisted, and the silent-degradation contract
/// guarantees no other signal. A caller that does not care says so with
/// `let _ =`.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The bytes are on disk.
    Written,
    /// The target was hostile and could not be made safe; nothing was written.
    SkippedHostile,
    /// The write itself failed (permissions, disk, rename).
    Failed,
}

/// The tested fail-direction predicate.
///
/// `None` means "I could not ask the question", not "the answer is no";
/// collapsing the two is the defect this project already paid for.
pub fn owner_check_passes(owner: Option<u64>, trusted: &[u64]) -> bool {
    match owner {
        Some(o) => trusted.contains(&o),
        None => true,
    }
}

/// True for a symlink/reparse point, or a file owned by someone else.
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

/// Removes a hostile path and re-evaluates the guard; `false` means nothing
/// may be written to it. The re-check is load-bearing: on a sticky directory
/// the unlink fails silently, and a remove-then-write without it would write
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
/// directory already is: the entry point for every parent this binary does
/// not own (`~/.claude`, the flat temp root, the harness's scratch roots).
/// For the guarded state directory use `write_guarded_under`.
pub fn write_guarded(path: &Path, bytes: &[u8]) -> WriteOutcome {
    write_inner(path, bytes, false)
}

/// Writes atomically through the guard, creating the parent privately when
/// `root` is the directory this binary owns. Takes the root rather than a bool
/// so the decision comes from the value that resolved the path, and a path
/// that is not a direct child of the root degrades to inherited behaviour
/// instead of silently claiming a directory.
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
        // The one directory this binary creates where it does not control the
        // parent. `create_dir_all` cannot be used: it returns `Ok` for a symlink
        // to a directory, turning an unverified adoption into the ordinary case.
        //
        // Residual TOCTOU, at its true width: between `session::state_dir_in`'s
        // read-only verdict and this creation, every path built from the root
        // traverses the parent unverified. Writes are covered twice
        // (`create_private_dir` re-verifies through a fresh handle; `make_safe`
        // plus `create_new` refuse a planted final path) and a plant costs one
        // tick. Reads keep the per-file owner check in `read_trusted`. The two
        // deletes (`cmd::git_refresh` and `subagent`'s linger expiry) have no
        // per-file check; what bounds them is the name,
        // `statusline-<sanitized session id>`, which an attacker cannot
        // choose. Routing them through a guarded remove is a behaviour change
        // that needs its own case.
        match crate::platform::create_private_dir(parent) {
            crate::platform::DirVerdict::Private => {}
            // Hostile and Failed are different answers: `cmd::subagent` maps
            // them to different ticks and different log lines.
            crate::platform::DirVerdict::Hostile => return WriteOutcome::SkippedHostile,
            crate::platform::DirVerdict::Absent => return WriteOutcome::Failed,
        }
    } else if std::fs::create_dir_all(parent).is_err() {
        return WriteOutcome::Failed;
    }

    // Temp-then-rename so a concurrent reader never sees a torn file.
    // The scripts do the same for the latch and the learned map.
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id()
    ));
    // The staging path is guarded too, and matters most: `rename` never follows
    // the final path, so the exploitable symlink is one planted at this
    // predictable temporary name. Both shell handlers drop a planted temp
    // target and skip the write if it survives; `make_safe` reproduces that,
    // and turns an undeletable plant into `SkippedHostile`.
    if !make_safe(&tmp) {
        return WriteOutcome::SkippedHostile;
    }
    // `make_safe` leaves a window between its check and the open where a
    // re-planted link would still be followed by a plain `write`; `create_new`
    // refuses anything that already exists, links included, so a plant in the
    // gap fails the write instead of redirecting it. The unlink first is
    // load-bearing the other way: a stale leftover from a crashed run at this
    // pid-derived name would otherwise fail every write for the session.
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
