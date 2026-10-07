//! Two of the areas platform-conditional code is confined to: file-ownership
//! checks, and process handling — process-entry stream handling, plus the
//! process snapshot, facts and termination that reclaim stranded processes.
//! Notification delivery is a third and lives in `notify`, with its click side
//! in `focus`, which shares the snapshot walker here.
//!
//! Keep new `#[cfg]` code here rather than scattering it —
//! `platform_conditional_code_stays_in_its_areas` asserts the file list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// One process as a Toolhelp snapshot saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub ppid: u64,
    /// A process created suspended and never resumed has exactly one.
    pub threads: u32,
    /// The image's file name as recorded at creation, so a later rename of the
    /// file (the installer's move to `.old`) does not change it.
    pub name: String,
}

/// Every process at one instant. Empty on Unix, where only stale files are
/// cleaned up and focus capture reads `/proc` or `libproc` instead.
#[derive(Debug, Default)]
pub struct ProcessSnapshot {
    /// When the walk began, in `FILETIME` units (100 ns since 1601). Read
    /// before the walk, so a process whose handle reports a creation time at
    /// or after this is one the snapshot never saw: its PID was reused, and
    /// the entry under that PID describes a different process.
    pub taken: u64,
    pub entries: HashMap<u64, ProcessEntry>,
}

/// One snapshot of every process; empty when it cannot be taken.
pub fn process_snapshot() -> ProcessSnapshot {
    imp::process_snapshot()
}

/// What the process holding a candidate's parent PID says about the parent.
/// Raw facts: the liveness rule, `ancestor_chain`'s, belongs to the
/// caller's pure policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentStart {
    /// No process in the snapshot holds the parent PID.
    Gone,
    /// The creation time of the process holding it. Later than the
    /// candidate's own means the PID was reused and the real parent is gone.
    At(u64),
    /// That creation time could not be read, which must count as alive.
    Unreadable,
}

/// Everything a stranded-process rule asks of one process. The snapshot
/// fields are always known; every fact read through the handle is `None` when
/// it could not be read — never a zero, an empty path or a dead parent standing
/// in for "unknown", because each of those answers is a reason to kill
/// (`docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFacts {
    pub pid: u64,
    pub ppid: u64,
    pub name: String,
    pub threads: u32,
    /// `QueryFullProcessImageNameW`'s path, as reported: after a rename it
    /// names the new file, in the same directory.
    pub image: Option<PathBuf>,
    /// Creation time, in `FILETIME` units.
    pub created: Option<u64>,
    /// Kernel plus user time, in 100 ns units.
    pub cpu: Option<u64>,
    /// Comparable with `current_owner()`.
    pub owner: Option<u64>,
    /// Arguments after the program, by `argument_count`.
    pub args: Option<usize>,
    pub parent: ParentStart,
}

/// A process opened once, with what was read through that one handle. The
/// handle is the only way to act on it, and holding it pins the process
/// object, so a termination always lands on the process the facts describe,
/// never on a successor that reused its PID.
pub struct Candidate {
    pub facts: ProcessFacts,
    handle: Option<imp::ProcessHandle>,
}

impl Candidate {
    /// Terminates the process through the handle its facts were read from.
    /// `false` when it was never opened or the call failed.
    pub fn terminate(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(imp::ProcessHandle::terminate)
    }
}

/// Opens `pid` with `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE |
/// SYNCHRONIZE` and reads its facts. `None` when the snapshot does not hold it;
/// a process that cannot be opened comes back with every handle fact `None`.
pub fn open_candidate(snapshot: &ProcessSnapshot, pid: u64) -> Option<Candidate> {
    imp::open_candidate(snapshot, pid)
}

/// Waits for terminated candidates to exit, on the handles they were judged
/// and terminated through, against one shared deadline: an installer removes
/// the old binary right after, which fails while any image is still mapped.
/// One handle at a time rather than `WaitForMultipleObjects`, which stops at
/// 64 and a week of leaks measured 116 ticks. Returns the PIDs that had not
/// exited, for the caller to log; they are left as they are.
pub fn wait_for_exit(terminated: &[Candidate], within: Duration) -> Vec<u64> {
    let deadline = Instant::now() + within;
    terminated
        .iter()
        .filter(|c| {
            let left = deadline.saturating_duration_since(Instant::now());
            !c.handle.as_ref().is_some_and(|h| h.wait(left))
        })
        .map(|c| c.facts.pid)
        .collect()
}

/// How many arguments follow the program on a Windows command line, split the
/// way `CommandLineToArgvW` splits it: a program that opens with a quote runs
/// to the next quote, otherwise to the first space or tab; after it, spaces and
/// tabs outside quotes separate arguments, and a quote behind an odd run of
/// backslashes is literal. Counted here because `CommandLineToArgvW` lives in
/// `shell32`, a DLL this binary does not otherwise load.
pub fn argument_count(command_line: &str) -> usize {
    let blank = |c: &char| matches!(c, ' ' | '\t');
    let mut chars = command_line.chars().peekable();
    if chars.next_if_eq(&'"').is_some() {
        let _ = chars.by_ref().find(|c| *c == '"');
    } else {
        while chars.next_if(|c| !blank(c)).is_some() {}
    }
    let mut count = 0;
    loop {
        while chars.next_if(blank).is_some() {}
        if chars.peek().is_none() {
            return count;
        }
        count += 1;
        let (mut quoted, mut backslashes) = (false, 0usize);
        while let Some(c) = chars.next_if(move |c| quoted || !blank(c)) {
            match c {
                '\\' => {
                    backslashes += 1;
                    continue;
                }
                '"' if backslashes % 2 == 0 => quoted = !quoted,
                _ => {}
            }
            backslashes = 0;
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    /// Stranded processes are a Windows problem: MSYS2's suspended spawn and
    /// its console helper. The snapshot is empty, so no process is ever opened
    /// and this handle type has no values.
    pub enum ProcessHandle {}

    impl ProcessHandle {
        pub fn terminate(&self) -> bool {
            match *self {}
        }

        pub fn wait(&self, _within: std::time::Duration) -> bool {
            match *self {}
        }
    }

    pub fn process_snapshot() -> super::ProcessSnapshot {
        super::ProcessSnapshot::default()
    }

    pub fn open_candidate(
        _snapshot: &super::ProcessSnapshot,
        _pid: u64,
    ) -> Option<super::Candidate> {
        None
    }

    /// Nothing to clear: `exec` replaces fds 0-2 with whatever the child was
    /// given, and std opens every other descriptor close-on-exec.
    pub fn make_std_handles_uninheritable() {}

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

    pub fn trusted_tool(path: &Path) -> bool {
        let Ok(real) = std::fs::canonicalize(path) else {
            return false;
        };
        let Some(dir) = real.parent() else {
            return false;
        };
        let mut trusted = trusted_owners();
        trusted.push(0);
        [real.as_path(), dir].iter().all(|p| {
            std::fs::metadata(p)
                .is_ok_and(|md| super::tool_owner_passes(md.uid() as u64, md.mode(), &trusted))
        })
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
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, SetHandleInformation, ERROR_SUCCESS, FILETIME, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, UNICODE_STRING, WAIT_OBJECT_0,
    };
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
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
        QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, INFINITE,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    fn wide(s: &Path) -> Vec<u16> {
        s.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Takes the inherit flag off the three standard handles this process was
    /// given. `CreateProcess` hands a child every inheritable handle in the
    /// parent, whatever stdio the child was assigned, so without this the
    /// detached `notify` child holds Claude Code's stdout and stderr pipes
    /// open until it exits, and the refresh waits on it. Spawns are unaffected:
    /// std duplicates an inheritable copy of each handle a child asks for.
    ///
    /// Must run before `redirect_stderr_to_null`, which leaves the original
    /// stderr open but unreachable. The click helper starts with no standard
    /// handles at all; those slots are skipped.
    pub fn make_std_handles_uninheritable() {
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe {
                let h = GetStdHandle(which);
                if !h.is_null() && h != INVALID_HANDLE_VALUE {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
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
        token_owner(unsafe { GetCurrentProcess() })
    }

    /// The user a process runs as. `OpenProcessToken` needs only the limited
    /// query right, so the same code answers for this process and for a
    /// candidate opened with nothing more.
    fn token_owner(process: HANDLE) -> Option<u64> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
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

    /// Nothing is resolved from a fixed location on Windows: the toast's
    /// interpreter is addressed under `%SystemRoot%`, and the click helper
    /// spawns nothing.
    pub fn trusted_tool(path: &Path) -> bool {
        path.is_file()
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

    fn filetime_u64(t: FILETIME) -> u64 {
        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
    }

    /// Now in `FILETIME` units. std's clock on Windows is the system time, so
    /// this needs no binding of its own.
    fn filetime_now() -> u64 {
        const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        UNIX_EPOCH_AS_FILETIME + (since.as_nanos() / 100) as u64
    }

    /// Creation time, then kernel plus user time.
    fn times(handle: HANDLE) -> Option<(u64, u64)> {
        unsafe {
            let mut creation: FILETIME = std::mem::zeroed();
            let mut exit: FILETIME = std::mem::zeroed();
            let mut kernel: FILETIME = std::mem::zeroed();
            let mut user: FILETIME = std::mem::zeroed();
            let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
            (ok != 0).then(|| {
                let cpu = filetime_u64(kernel).saturating_add(filetime_u64(user));
                (filetime_u64(creation), cpu)
            })
        }
    }

    /// The creation time from a limited-information handle, which a standard
    /// user can open on every process of their own.
    pub fn process_start(pid: u64) -> Option<u64> {
        let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        times(handle.0).map(|(created, _)| created)
    }

    /// One Toolhelp snapshot, keyed by pid.
    pub fn process_snapshot() -> super::ProcessSnapshot {
        let taken = filetime_now();
        let mut entries = HashMap::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap.is_null() || snap == INVALID_HANDLE_VALUE {
                return super::ProcessSnapshot { taken, entries };
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut entry) != 0 {
                loop {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    entries.insert(
                        u64::from(entry.th32ProcessID),
                        super::ProcessEntry {
                            ppid: u64::from(entry.th32ParentProcessID),
                            threads: entry.cntThreads,
                            name: String::from_utf16_lossy(&entry.szExeFile[..len]),
                        },
                    );
                    if Process32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        super::ProcessSnapshot { taken, entries }
    }

    /// An open process handle, closed on drop.
    pub struct ProcessHandle(HANDLE);

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// `pid` opened with `access`; `None` when it does not fit a `DWORD` or
    /// the open fails.
    fn open(pid: u64, access: u32) -> Option<ProcessHandle> {
        let pid = u32::try_from(pid).ok()?;
        let raw = unsafe { OpenProcess(access, 0, pid) };
        // Never `then_some`: a null handle built eagerly would be closed on drop.
        if raw.is_null() {
            None
        } else {
            Some(ProcessHandle(raw))
        }
    }

    impl ProcessHandle {
        pub fn terminate(&self) -> bool {
            unsafe { TerminateProcess(self.0, 1) != 0 }
        }

        /// Whether the process exited within `within`. Never waits forever:
        /// `INFINITE` is a value of the same parameter.
        pub fn wait(&self, within: std::time::Duration) -> bool {
            let ms = within.as_millis().min(u128::from(INFINITE - 1)) as u32;
            unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
        }
    }

    pub fn open_candidate(snapshot: &super::ProcessSnapshot, pid: u64) -> Option<super::Candidate> {
        let entry = snapshot.entries.get(&pid)?;
        let mut facts = super::ProcessFacts {
            pid,
            ppid: entry.ppid,
            name: entry.name.clone(),
            threads: entry.threads,
            image: None,
            created: None,
            cpu: None,
            owner: None,
            args: None,
            parent: super::ParentStart::Unreadable,
        };
        let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE;
        let Some(handle) = open(pid, access) else {
            return Some(super::Candidate {
                facts,
                handle: None,
            });
        };
        let raw = handle.0;
        facts.image = image_path(raw);
        if let Some((created, cpu)) = times(raw) {
            facts.created = Some(created);
            facts.cpu = Some(cpu);
        }
        facts.owner = token_owner(raw);
        facts.args = command_line(raw).map(|line| super::argument_count(&line));
        // The parent is not opened with the candidate's rights: only its
        // creation time is needed, and asking for more would turn a parent
        // this user may query but not terminate into an unreadable one.
        facts.parent = if snapshot.entries.contains_key(&entry.ppid) {
            process_start(entry.ppid).map_or(super::ParentStart::Unreadable, super::ParentStart::At)
        } else {
            super::ParentStart::Gone
        };
        Some(super::Candidate {
            facts,
            handle: Some(handle),
        })
    }

    fn image_path(handle: HANDLE) -> Option<PathBuf> {
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }

    /// The command line through `NtQueryInformationProcess`, which reads it on
    /// a limited-information handle without touching the target's memory.
    /// ntdll is mapped into every process, so this loads nothing.
    fn command_line(handle: HANDLE) -> Option<String> {
        unsafe {
            let mut needed = 0u32;
            NtQueryInformationProcess(
                handle,
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &mut needed,
            );
            let size = needed as usize;
            if size < std::mem::size_of::<UNICODE_STRING>() {
                return None;
            }
            // Whole `u64`s, so the `UNICODE_STRING` heading the buffer is aligned.
            let mut buf = vec![0u64; size.div_ceil(8)];
            let status = NtQueryInformationProcess(
                handle,
                ProcessCommandLineInformation,
                buf.as_mut_ptr().cast(),
                needed,
                &mut needed,
            );
            if status < 0 {
                return None;
            }
            let header = &*(buf.as_ptr() as *const UNICODE_STRING);
            // The kernel points `Buffer` just past the header, inside `buf`;
            // anything else is not read.
            let start = buf.as_ptr() as usize;
            let text = header.Buffer as usize;
            let bytes = usize::from(header.Length);
            if header.Buffer.is_null() || text < start || text + bytes > start + buf.len() * 8 {
                return None;
            }
            let units = std::slice::from_raw_parts(header.Buffer, bytes / 2);
            Some(String::from_utf16_lossy(units))
        }
    }
}

pub use imp::{make_std_handles_uninheritable, redirect_stderr_to_null};

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

/// Whether a tool found in a fixed location, rather than on `PATH`, may be
/// run: the executable, with symlinks resolved, and the directory holding it
/// are owned by this user or root and writable by nobody else. So another
/// account's Homebrew on a shared Mac is never run on every alert. Resolving
/// first is what lets Homebrew's group-writable `bin` pass: its entries link
/// into kegs only the owner can write, and a link re-pointed at a foreign
/// file fails on the target.
pub fn trusted_tool(path: &Path) -> bool {
    imp::trusted_tool(path)
}

/// The tested predicate behind `trusted_tool`: a trusted owner, and no write
/// bit for group or others.
pub fn tool_owner_passes(owner: u64, mode: u32, trusted: &[u64]) -> bool {
    crate::state::owner_check_passes(Some(owner), trusted) && mode & 0o022 == 0
}
