//! Two of the areas platform-conditional code is confined to: file-ownership
//! checks and process-entry stream handling. Notification delivery is the third
//! and lives in `notify`, with its click side in `focus`.
//!
//! Keep new `#[cfg]` code here rather than scattering it —
//! `platform_conditional_code_stays_in_its_areas` asserts the file list.

use std::path::Path;

pub mod focus;
pub mod notify;

/// What a candidate state directory is, from `dir_verdict` (looks only) or
/// `create_private_dir` (creates first). After a creation attempt, `Absent`
/// means the mkdir failed for a mundane reason and maps to
/// `WriteOutcome::Failed`, where `Hostile` maps to `SkippedHostile`;
/// `state::write_guarded` draws that line, and a bool would collapse it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirVerdict {
    /// Nothing is there. Safe to create.
    Absent,
    /// A real directory, trusted owner, no group or other bits on Unix.
    Private,
    /// A symlink, a reparse point, a non-directory, a foreign owner, or — on
    /// Unix — a mode that lets anyone else in. Never adopted.
    Hostile,
}

/// Looks at `path` without creating anything, on any path through it: the
/// state directory is resolved before the payload is parsed, and
/// `degraded_input_renders_the_notice_and_touches_no_state` asserts that a
/// tick which could not be understood leaves the temp directory empty.
///
/// Fail directions match `state::is_hostile` deliberately, so the two guards
/// cannot disagree about the same question one level apart:
///
/// - symlink or reparse point → `Hostile`
/// - not a directory → `Hostile`
/// - owner resolvable and foreign → `Hostile`
/// - owner **not** resolvable → falls through to the symlink check, so a real
///   directory reads `Private`. Failing closed would cost the state directory
///   wherever the ownership lookup is unavailable; see
///   `state::owner_check_passes` for why that inversion already cost nine days.
pub fn dir_verdict(path: &Path) -> DirVerdict {
    match std::fs::symlink_metadata(path) {
        // Something is there; the handle decides. The open refuses to follow
        // links, so a symlink or plain file reads hostile with no stat to race.
        Ok(_) => imp::verify_through_handle(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirVerdict::Absent,
        // Unreadable otherwise: refusing costs only the flat-root fallback.
        Err(_) => DirVerdict::Hostile,
    }
}

/// Creates `path` privately if it is absent, then verifies it through an open
/// handle. Tolerates `AlreadyExists`, because two of this binary's three
/// per-tick processes can race here; `Absent` on return means the creation
/// failed for a mundane reason.
///
/// The verification deliberately does **not** re-stat the path:
/// `lstat`-then-use-by-path is the shape that produced CVE-2025-71176 in
/// pytest, a symlink swapped in after the check. The handle is opened refusing
/// to follow links, and every question is then asked of it.
pub fn create_private_dir(path: &Path) -> DirVerdict {
    // The ancestors first, unguarded and best-effort: `mkdir_private` creates
    // exactly one level, so a missing temp root would otherwise fail every
    // state write for the life of the session, silently. The ancestors are the
    // OS temp root, which this binary never owned, so building them the
    // ordinary way restores the pre-state-directory behaviour without
    // weakening the one level that is ours.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match imp::mkdir_private(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return DirVerdict::Absent,
    }
    imp::verify_through_handle(path)
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    /// Layer 1 of silent degradation. A stack overflow or allocation failure
    /// writes straight to fd 2 from the runtime, below the panic hook, so the
    /// descriptor is redirected before anything runs.
    pub fn redirect_stderr_to_null() {
        unsafe {
            let path = b"/dev/null\0";
            let fd = libc::open(path.as_ptr() as *const libc::c_char, libc::O_WRONLY);
            if fd >= 0 {
                libc::dup2(fd, libc::STDERR_FILENO);
                if fd != libc::STDERR_FILENO {
                    libc::close(fd);
                }
            }
        }
    }

    pub fn current_owner() -> Option<u64> {
        Some(unsafe { libc::geteuid() } as u64)
    }

    /// The current user alone; Unix has no counterpart to Administrators.
    pub fn trusted_owners() -> Vec<u64> {
        current_owner().into_iter().collect()
    }

    /// `symlink_metadata`, so a symlink reports its own owner, not its target's.
    pub fn file_owner(path: &Path) -> Option<u64> {
        std::fs::symlink_metadata(path).ok().map(|m| m.uid() as u64)
    }

    /// Creates the directory at `0700` in a single `mkdir(2)`. A `create_dir`
    /// followed by `set_permissions` would leave it at the process umask for
    /// the width of that gap, and on a shared `/tmp` that is long enough for
    /// any local user to enter it and plant.
    pub fn mkdir_private(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }

    pub fn verify_through_handle(path: &Path) -> super::DirVerdict {
        use std::os::unix::fs::OpenOptionsExt;

        // `O_NOFOLLOW` and `O_DIRECTORY` make the open itself refuse a symlink
        // and a non-directory; asking by path and then opening is the ordering
        // that produced CVE-2025-71176.
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)
        else {
            return super::DirVerdict::Hostile;
        };
        let Ok(md) = file.metadata() else {
            return super::DirVerdict::Hostile;
        };
        // Any group or other bit is a way in for someone else. It is also what
        // lets the design stop at one level: nobody else can traverse it, so
        // there is nothing for `openat2` and `RESOLVE_BENEATH` to defend.
        if md.mode() & 0o077 != 0 {
            return super::DirVerdict::Hostile;
        }
        let trusted = super::trusted_owners();
        // Same shape as `state::is_hostile`: an unknown identity degrades to the
        // checks above rather than refusing.
        if !trusted.is_empty() && !crate::state::owner_check_passes(Some(md.uid() as u64), &trusted)
        {
            return super::DirVerdict::Hostile;
        }
        super::DirVerdict::Private
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, GetSecurityInfo, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, GetLengthSid, GetTokenInformation, TokenUser,
        WinBuiltinAdministratorsSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn wide(s: &Path) -> Vec<u16> {
        s.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Layer 1, Windows form: swap the standard error handle for one on `NUL`.
    pub fn redirect_stderr_to_null() {
        unsafe {
            let name: Vec<u16> = "NUL\0".encode_utf16().collect();
            let h = CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if !h.is_null() && h as isize != -1 {
                SetStdHandle(STD_ERROR_HANDLE, h);
            }
        }
    }

    /// Hashes a SID's bytes into a comparable id; only equality matters here.
    unsafe fn sid_id(sid: PSID) -> Option<u64> {
        if sid.is_null() {
            return None;
        }
        let len = GetLengthSid(sid) as usize;
        if len == 0 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(sid as *const u8, len);
        // FNV-1a: stable across runs, which is all an equality check needs.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some(hash)
    }

    pub fn current_owner() -> Option<u64> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut needed: u32 = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                CloseHandle(token);
                return None;
            }
            let mut buf = vec![0u8; needed as usize];
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut _,
                needed,
                &mut needed,
            );
            CloseHandle(token);
            if ok == 0 {
                return None;
            }
            let tu = &*(buf.as_ptr() as *const TOKEN_USER);
            sid_id(tu.User.Sid)
        }
    }

    /// The Administrators group, which owns everything an elevated process
    /// creates. A standard user cannot produce a file owned by it, so accepting
    /// it does not widen the set of principals the guard defends against: an
    /// administrator already owns the binary, `settings.json`, and the ability
    /// to take ownership of anything else. `install.ps1` has accepted admin
    /// ownership of the install directory since it was written.
    fn administrators() -> Option<u64> {
        unsafe {
            let mut size: u32 = 0;
            CreateWellKnownSid(
                WinBuiltinAdministratorsSid,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            );
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            if CreateWellKnownSid(
                WinBuiltinAdministratorsSid,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as PSID,
                &mut size,
            ) == 0
            {
                return None;
            }
            sid_id(buf.as_ptr() as PSID)
        }
    }

    pub fn trusted_owners() -> Vec<u64> {
        let mut out = Vec::with_capacity(2);
        out.extend(current_owner());
        out.extend(administrators());
        out
    }

    /// Creates the directory, inheriting the parent's ACL. There is no Windows
    /// counterpart to `0700` — a restrictive DACL would have to be built, and
    /// `%TEMP%` is per-user already — so `verify_through_handle` makes no
    /// permission claim on this platform; see the `DirVerdict` docs and the
    /// accepted divergence in `docs/performance.md` §4.
    pub fn mkdir_private(path: &Path) -> std::io::Result<()> {
        std::fs::create_dir(path)
    }

    /// The owner of an already-open handle. `SE_KERNEL_OBJECT`, not
    /// `SE_FILE_OBJECT`: the question is about the object the handle is pinned
    /// to. `file_owner` below re-walks the name by path, which is safe for the
    /// per-file guard that stats the same path immediately and not here, where
    /// a swap between the reparse-point check and the owner call would answer
    /// for the new target.
    unsafe fn handle_owner(handle: HANDLE) -> Option<u64> {
        let mut owner: PSID = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let rc = GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let id = sid_id(owner);
        if !sd.is_null() {
            LocalFree(sd as *mut _);
        }
        id
    }

    pub fn verify_through_handle(path: &Path) -> super::DirVerdict {
        unsafe {
            let w = wide(path);
            // `FILE_FLAG_BACKUP_SEMANTICS` allows opening a directory at all;
            // `FILE_FLAG_OPEN_REPARSE_POINT` opens a planted junction itself
            // rather than silently following it.
            let handle = CreateFileW(
                w.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            );
            if handle.is_null() || handle as isize == -1 {
                return super::DirVerdict::Hostile;
            }

            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(handle, &mut info) == 0 {
                CloseHandle(handle);
                return super::DirVerdict::Hostile;
            }
            // A junction, the realistic squat in `%TEMP%`, is no symlink to
            // `symlink_metadata`; rejecting every reparse point covers both,
            // before the owner is asked so the answer cannot describe a target.
            if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
            {
                CloseHandle(handle);
                return super::DirVerdict::Hostile;
            }

            let owner = handle_owner(handle);
            CloseHandle(handle);

            let trusted = super::trusted_owners();
            // Same shape as `state::is_hostile`: an unknown identity degrades to
            // the reparse-point check rather than refusing.
            if !trusted.is_empty() && !crate::state::owner_check_passes(owner, &trusted) {
                return super::DirVerdict::Hostile;
            }
            super::DirVerdict::Private
        }
    }

    /// Returns `None` when the owner cannot be determined. That is not a
    /// failure signal — see `state::owner_check_passes` for why it must
    /// degrade to the symlink guard rather than failing closed.
    pub fn file_owner(path: &Path) -> Option<u64> {
        unsafe {
            let w = wide(path);
            let mut owner: PSID = std::ptr::null_mut();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let rc = GetNamedSecurityInfoW(
                w.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            );
            if rc != ERROR_SUCCESS {
                return None;
            }
            let id = sid_id(owner);
            if !sd.is_null() {
                LocalFree(sd as *mut _);
            }
            id
        }
    }
}

pub use imp::redirect_stderr_to_null;

/// The current user's comparable owner id, or `None` when it cannot be read.
pub fn current_owner() -> Option<u64> {
    imp::current_owner()
}

/// Every owner id a state file may legitimately carry: this process's user,
/// plus the Administrators group on Windows.
pub fn trusted_owners() -> Vec<u64> {
    imp::trusted_owners()
}

/// The owner id of `path` itself (not its symlink target), or `None`.
pub fn file_owner(path: &Path) -> Option<u64> {
    imp::file_owner(path)
}
