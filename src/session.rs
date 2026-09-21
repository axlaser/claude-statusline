//! Session identity and the temp root every session-scoped state file sits in.
//!
//! `git-refresh`, `subagent` and the status line derive paths from the same
//! session id, and agree only because one function answers "what is this
//! session's id" and one "where does its state live". A second copy would
//! drift silently: a hook invalidating a path nothing reads still renders fine.

use std::path::{Path, PathBuf};

/// Ids longer than this are truncated. Nothing Claude Code emits comes near it
/// (a UUID is 36 characters), but a name the filesystem refuses makes every
/// state write for that session fail, silently, for as long as it lasts.
const MAX_SESSION_ID: usize = 128;

/// Strips everything outside `[a-zA-Z0-9_-]` from a session id. Characters
/// are *removed*, not replaced, which is what all four scripts do:
/// `../../foo/bar` becomes `foobar`. A port that substituted would derive
/// different paths for the same session and silently stop invalidating the
/// cache. It is also the whole defence against path traversal: a separator or
/// `..` surviving here would let a component write or delete outside the temp
/// directory.
pub fn sanitize_session_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(MAX_SESSION_ID)
        .collect()
}

/// The temp directory the scripts use.
///
/// Spelled out rather than `std::env::temp_dir`, which consults `TMP` before
/// `TEMP` on Windows. The scripts read `%TEMP%`, and where the two differ the
/// hook would delete from one directory while the status line writes to the
/// other, invisibly, because a cache never invalidated still renders.
pub fn temp_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(unix)]
    {
        std::env::var_os("TMPDIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    }
}

/// The directory name this binary groups its state under, minus the per-user
/// suffix from `platform::current_owner()` (a uid on Unix, a digest of the
/// token SID on Windows). The suffix is not a security control: any user can
/// create any name in a shared `/tmp`. It buys an unambiguous owner check:
/// with one shared name a foreign owner might be a second user legitimately
/// there first; with the owner in the name, a mismatch is always wrong and the
/// failure is confined to one uid.
const STATE_DIR_PREFIX: &str = "claude-statusline-";

/// Where this session's state lives, and whether this binary owns that
/// directory. The second field is why this is a struct: `state::write_guarded`
/// also serves the flat temp root, `~/.claude` and the harness's scratch
/// roots, and must create only the state directory privately, because the
/// private-directory check fails `/tmp` (mode 1777, root-owned) and with it
/// every state write on Linux. Paths and existence cannot recover the
/// distinction: where the temp root does not yet exist, "parent absent" would
/// route `/tmp` back into the guard. Derefs to `Path` so the eight path
/// builders that only `join` onto it need no signature change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRoot {
    path: PathBuf,
    guarded: bool,
}

impl StateRoot {
    /// A root this binary did not create and must not judge: the flat temp
    /// root, or any directory a test stages directly.
    pub fn inherited(path: PathBuf) -> Self {
        Self {
            path,
            guarded: false,
        }
    }

    /// True when this binary creates the directory privately on first write.
    pub fn is_guarded(&self) -> bool {
        self.guarded
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for StateRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for StateRoot {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// Resolves the state directory under `temp_root`. **Creates nothing**:
/// `Roots::from_env` runs before the payload is parsed, and a tick whose
/// payload does not parse must leave the temp directory untouched
/// (`degraded_input_renders_the_notice_and_touches_no_state`); creation
/// happens on the first guarded write. Falls back to `temp_root` itself,
/// unguarded, when the candidate exists and does not verify, or when the owner
/// cannot be read: a name with no owner in it would put every user on the
/// machine in one directory, the opposite of what the suffix is for.
pub fn state_dir_in(temp_root: &Path) -> StateRoot {
    let Some(owner) = crate::platform::current_owner() else {
        crate::debug::log(|| {
            "state_dir: owner unavailable, using the temp root directly".to_string()
        });
        return StateRoot::inherited(temp_root.to_path_buf());
    };

    let candidate = temp_root.join(format!("{STATE_DIR_PREFIX}{owner}"));
    match crate::platform::dir_verdict(&candidate) {
        crate::platform::DirVerdict::Absent | crate::platform::DirVerdict::Private => StateRoot {
            path: candidate,
            guarded: true,
        },
        crate::platform::DirVerdict::Hostile => {
            let shown = candidate.display().to_string();
            crate::debug::log(move || {
                format!("state_dir: {shown} did not verify, using the temp root directly")
            });
            StateRoot::inherited(temp_root.to_path_buf())
        }
    }
}

/// Production wrapper: `state_dir_in` over the real temp root. The root is read
/// here and nowhere else, so resolution stays a pure function of its argument
/// and tests can drive it without touching process environment from a
/// threaded test binary.
pub fn state_dir() -> StateRoot {
    state_dir_in(&temp_dir())
}
