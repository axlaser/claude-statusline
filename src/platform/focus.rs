//! The platform half of click-to-focus: capture primitives, probes and
//! executors for the click path, and the URI registration on Windows.
//!
//! Notification delivery is one of the four areas platform-conditional code is
//! confined to, and this file is that area's click side: how a process learns
//! which terminal it is in, how a click is observed, and how a window is
//! raised. Every decision stays in `focus` and `cmd::focus`, which are
//! cfg-free; this file only observes and acts.
//!
//! The `imp` modules provide the same surface per platform. A function a
//! platform cannot answer returns `None`, and capture stores nothing for it,
//! so a missing primitive costs one identity field rather than the record.

use crate::focus::{self, Anchor, Observation, ProcessInfo, TOKEN_BYTES};

/// Everything capture needs from the machine, gathered once per alert.
///
/// `session` is the sanitised session id, which names the window property
/// the Windows capture sets (KTD9).
pub fn observe(session: &str) -> Observation {
    let chain = imp::ancestor_chain();
    let claude_pid = std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let (anchor, terminal) = focus::select_anchor(&chain, claude_pid);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let basename = std::path::Path::new(&cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let window = anchor
        .as_ref()
        .and_then(|a| imp::capture_window(&chain, a.pid, session, &basename));
    Observation {
        anchor: anchor.map(|p| Anchor {
            pid: p.pid,
            start: p.start,
            boot_id: imp::boot_id(),
        }),
        terminal: terminal.map(|p| (p.pid, p.start)),
        tty: imp::controlling_tty(),
        cwd,
        window,
        random: imp::random_bytes(),
    }
}

/// The start time of `pid` as the platform measures it, or `None` when the
/// process is gone or cannot be asked.
pub fn process_start(pid: u64) -> Option<u64> {
    imp::process_start(pid)
}

/// The boot id start times are measured against, where they are (Linux).
pub fn boot_id() -> Option<String> {
    imp::boot_id()
}

/// Random bytes for a token, straight from the OS. `None` on any failure:
/// there is no fallback source (KTD13).
pub fn random_bytes() -> Option<[u8; TOKEN_BYTES]> {
    imp::random_bytes()
}

/// The registry path of the per-user URI handler, under `HKEY_CURRENT_USER`.
pub const PROTOCOL_KEY: &str = "Software\\Classes\\claude-statusline";

/// The open command registered for the `claude-statusline:` scheme, or `None`
/// when nothing is registered or the platform has no such registry.
pub fn registered_protocol_command() -> Option<String> {
    imp::registered_protocol_command(PROTOCOL_KEY)
}

/// Walks a chain by asking `lookup` for each parent in turn, self first,
/// stopping at the root, a cycle, or a bound.
fn walk(self_pid: u64, lookup: impl Fn(u64) -> Option<ProcessInfo>) -> Vec<ProcessInfo> {
    let mut out = Vec::new();
    let mut pid = self_pid;
    for _ in 0..64 {
        let Some(info) = lookup(pid) else { break };
        let ppid = info.ppid;
        out.push(info);
        if ppid == 0 || ppid == pid || out.iter().any(|p| p.pid == ppid) {
            break;
        }
        pid = ppid;
    }
    out
}

#[cfg(unix)]
fn urandom() -> Option<[u8; TOKEN_BYTES]> {
    use std::io::Read;
    let mut buf = [0u8; TOKEN_BYTES];
    // A full read, never a short one: a token built from fewer than 24 fresh
    // bytes would be a weaker nonce than the record claims.
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .ok()
        .map(|()| buf)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{walk, ProcessInfo, TOKEN_BYTES};

    pub fn random_bytes() -> Option<[u8; TOKEN_BYTES]> {
        super::urandom()
    }

    /// `/proc/<pid>/stat`: the name sits in parentheses and may itself
    /// contain spaces or parentheses, so the fields are split after the last
    /// closing one. `ppid` is field 4 and the start time field 22, both
    /// counted from 1 with the name as field 2.
    fn stat(pid: u64) -> Option<ProcessInfo> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let name = text.get(open + 1..close)?.to_string();
        let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
        let ppid = rest.get(1)?.parse().ok()?;
        let start = rest.get(19)?.parse().ok()?;
        Some(ProcessInfo {
            pid,
            ppid,
            name,
            start,
        })
    }

    pub fn ancestor_chain() -> Vec<ProcessInfo> {
        walk(u64::from(std::process::id()), stat)
    }

    /// The chain above an arbitrary pid, for the click-time window search.
    pub fn ancestors_of(pid: u64) -> Vec<ProcessInfo> {
        walk(pid, stat)
    }

    pub fn capture_window(
        _chain: &[ProcessInfo],
        _anchor_pid: u64,
        _session: &str,
        _basename: &str,
    ) -> Option<crate::focus::WindowIdentity> {
        None
    }

    pub fn window_verifies(_window: &crate::focus::WindowIdentity, _session: &str) -> bool {
        false
    }

    pub fn raise_window(_handle: u64) -> bool {
        false
    }

    pub fn process_start(pid: u64) -> Option<u64> {
        stat(pid).map(|p| p.start)
    }

    /// Start times count from boot, so a pid and start time only identify a
    /// process within one boot. The temp directory can outlive a reboot.
    pub fn boot_id() -> Option<String> {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Only macOS tabs are matched by tty; nothing on Linux reads it.
    pub fn controlling_tty() -> Option<String> {
        None
    }

    pub fn registered_protocol_command(_key: &str) -> Option<String> {
        None
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{walk, ProcessInfo, TOKEN_BYTES};

    pub fn random_bytes() -> Option<[u8; TOKEN_BYTES]> {
        super::urandom()
    }

    /// `proc_pidinfo(PROC_PIDTBSDINFO)`: parent, image name, start time and
    /// controlling terminal in one call, without the `kinfo_proc` layout.
    fn info(pid: u64) -> Option<libc::proc_bsdinfo> {
        let pid = i32::try_from(pid).ok()?;
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
                size,
            )
        };
        (got == size).then_some(info)
    }

    fn describe(pid: u64) -> Option<ProcessInfo> {
        let info = info(pid)?;
        let name = unsafe { std::ffi::CStr::from_ptr(info.pbi_comm.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Some(ProcessInfo {
            pid,
            ppid: u64::from(info.pbi_ppid),
            name,
            start: info
                .pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec),
        })
    }

    pub fn ancestor_chain() -> Vec<ProcessInfo> {
        walk(u64::from(std::process::id()), describe)
    }

    pub fn ancestors_of(pid: u64) -> Vec<ProcessInfo> {
        walk(pid, describe)
    }

    pub fn capture_window(
        _chain: &[ProcessInfo],
        _anchor_pid: u64,
        _session: &str,
        _basename: &str,
    ) -> Option<crate::focus::WindowIdentity> {
        None
    }

    pub fn window_verifies(_window: &crate::focus::WindowIdentity, _session: &str) -> bool {
        false
    }

    pub fn raise_window(_handle: u64) -> bool {
        false
    }

    pub fn process_start(pid: u64) -> Option<u64> {
        describe(pid).map(|p| p.start)
    }

    /// Start times here are absolute, so no boot id is needed.
    pub fn boot_id() -> Option<String> {
        None
    }

    /// The controlling terminal, from the process's `e_tdev`. The device is
    /// mapped back to a path by its minor number and confirmed against the
    /// node's own `rdev`, so an unexpected major never yields a wrong tab.
    pub fn controlling_tty() -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let info = info(u64::from(std::process::id()))?;
        if info.e_tdev == u32::MAX {
            return None;
        }
        let minor = info.e_tdev & 0x00ff_ffff;
        let path = format!("/dev/ttys{minor:03}");
        let md = std::fs::metadata(&path).ok()?;
        (md.rdev() as u32 == info.e_tdev).then_some(path)
    }

    pub fn registered_protocol_command(_key: &str) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use super::{walk, ProcessInfo, TOKEN_BYTES};
    use crate::focus::{WindowCandidate, WindowIdentity, PSEUDO_CONSOLE_CLASS, WINDOW_CLASSES};

    use windows_sys::Win32::Foundation::{
        CloseHandle, BOOL, ERROR_SUCCESS, FILETIME, HANDLE, HWND, LPARAM,
    };
    use windows_sys::Win32::Security::Cryptography::ProcessPrng;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetAncestor, GetClassNameW, GetPropW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetPropW, GA_ROOTOWNER,
    };

    /// The source Rust's own standard library draws from. A zero return is a
    /// failure and yields no token.
    pub fn random_bytes() -> Option<[u8; TOKEN_BYTES]> {
        let mut buf = [0u8; TOKEN_BYTES];
        let ok = unsafe { ProcessPrng(buf.as_mut_ptr(), buf.len()) };
        (ok != 0).then_some(buf)
    }

    fn filetime_u64(t: FILETIME) -> u64 {
        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
    }

    /// The creation time from a limited-information handle, which a standard
    /// user can open on every process of their own.
    pub fn process_start(pid: u64) -> Option<u64> {
        let pid = u32::try_from(pid).ok()?;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut creation: FILETIME = std::mem::zeroed();
            let mut exit: FILETIME = std::mem::zeroed();
            let mut kernel: FILETIME = std::mem::zeroed();
            let mut user: FILETIME = std::mem::zeroed();
            let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
            CloseHandle(handle);
            (ok != 0).then(|| filetime_u64(creation))
        }
    }

    /// One Toolhelp snapshot, read into a pid → (ppid, image) map.
    fn snapshot() -> std::collections::HashMap<u64, (u64, String)> {
        let mut out = std::collections::HashMap::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap.is_null() || snap as isize == -1 {
                return out;
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
                    let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                    out.insert(
                        u64::from(entry.th32ProcessID),
                        (u64::from(entry.th32ParentProcessID), name),
                    );
                    if Process32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        out
    }

    /// The chain from the snapshot, with each entry's creation time. A parent
    /// pid that was recycled before the snapshot would point at an unrelated
    /// process; the creation-time comparison below catches the case where the
    /// "parent" started after the child.
    pub fn ancestor_chain() -> Vec<ProcessInfo> {
        let procs = snapshot();
        let chain = walk(u64::from(std::process::id()), |pid| {
            let (ppid, name) = procs.get(&pid)?;
            Some(ProcessInfo {
                pid,
                ppid: *ppid,
                name: name.clone(),
                start: process_start(pid).unwrap_or(0),
            })
        });
        let mut out: Vec<ProcessInfo> = Vec::with_capacity(chain.len());
        for info in chain {
            if let Some(child) = out.last() {
                if info.start != 0 && child.start != 0 && info.start > child.start {
                    break;
                }
            }
            out.push(info);
        }
        out
    }

    /// Creation times are absolute, so no boot id is needed.
    pub fn boot_id() -> Option<String> {
        None
    }

    pub fn controlling_tty() -> Option<String> {
        None
    }

    /// Nothing on Windows searches windows by ancestor; the record carries
    /// the handle itself.
    pub fn ancestors_of(pid: u64) -> Vec<ProcessInfo> {
        vec![ProcessInfo {
            pid,
            ppid: 0,
            name: String::new(),
            start: 0,
        }]
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn class_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    fn title_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    fn pid_of(hwnd: HWND) -> u64 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        u64::from(pid)
    }

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam as *mut Vec<HWND>);
        out.push(hwnd);
        1
    }

    /// Every top-level window, once; the callers filter by pid.
    fn top_level_windows() -> Vec<HWND> {
        let mut out: Vec<HWND> = Vec::new();
        unsafe { EnumWindows(Some(collect), &mut out as *mut Vec<HWND> as LPARAM) };
        out
    }

    fn visible_windows_of(all: &[HWND], pid: u64) -> Vec<WindowCandidate> {
        all.iter()
            .copied()
            .filter(|h| pid_of(*h) == pid && unsafe { IsWindowVisible(*h) } != 0)
            .map(|h| WindowCandidate {
                handle: h as usize as u64,
                class: class_of(h),
                title: title_of(h),
            })
            .collect()
    }

    /// KTD6 candidate A, without attaching to anything: the terminal window
    /// behind the console an ancestor is attached to.
    ///
    /// Claude Code spawns its children headless, so this process's own
    /// console has no window. The console windows of its ancestors are still
    /// enumerable, though: under a ConPTY host the `PseudoConsoleWindow` is
    /// attributed to the shell that owns the pty, and its root owner under
    /// Windows Terminal is the real hosting window; under the classic console
    /// the `ConsoleWindowClass` window belongs to a conhost whose parent is
    /// the console application. Both facts were measured in U1. Walking the
    /// chain nearest-first and asking `EnumWindows` costs no `FreeConsole`,
    /// which matters: a process that frees its console while another thread
    /// spawns children hands those children fresh consoles of their own.
    fn chain_console_window(
        chain: &[ProcessInfo],
        procs: &std::collections::HashMap<u64, (u64, String)>,
        all: &[HWND],
    ) -> Option<HWND> {
        let terminal_root = |h: HWND| -> Option<HWND> {
            let root = unsafe { GetAncestor(h, GA_ROOTOWNER) };
            let root = if root.is_null() { h } else { root };
            let class = class_of(root);
            (class != PSEUDO_CONSOLE_CLASS && WINDOW_CLASSES.contains(&class.as_str()))
                .then_some(root)
        };
        for ancestor in chain.iter().skip(1) {
            for h in all {
                let class = class_of(*h);
                if class == PSEUDO_CONSOLE_CLASS && pid_of(*h) == ancestor.pid {
                    if let Some(root) = terminal_root(*h) {
                        return Some(root);
                    }
                } else if class == "ConsoleWindowClass" {
                    // The window is conhost's; conhost's parent is the
                    // console application it serves.
                    let conhost_parent = procs.get(&pid_of(*h)).map(|(ppid, _)| *ppid);
                    if conhost_parent == Some(ancestor.pid) {
                        return Some(*h);
                    }
                }
            }
        }
        None
    }

    /// KTD6 candidate B: the first ancestor above this process that owns a
    /// visible top-level window, chosen through the pure selection rule.
    fn ancestor_window(
        chain: &[ProcessInfo],
        all: &[HWND],
        basename: &str,
    ) -> Option<(HWND, String)> {
        for ancestor in chain.iter().skip(1) {
            let windows = visible_windows_of(all, ancestor.pid);
            if windows.is_empty() {
                continue;
            }
            let is_vscode = ancestor.name.eq_ignore_ascii_case("Code.exe");
            let picked = crate::focus::select_window_candidate(&windows, is_vscode, basename)?;
            return Some((picked.handle as usize as HWND, ancestor.name.clone()));
        }
        None
    }

    /// Captures the terminal window: candidate A, then B, then the class
    /// rule, then the creation time of the owner, then the marker property.
    /// Any step that cannot be completed stores nothing (KTD9).
    pub fn capture_window(
        chain: &[ProcessInfo],
        anchor_pid: u64,
        session: &str,
        basename: &str,
    ) -> Option<WindowIdentity> {
        let _ = anchor_pid;
        let procs = snapshot();
        let all = top_level_windows();
        let (hwnd, owner_image) = match chain_console_window(chain, &procs, &all) {
            Some(h) => {
                let pid = pid_of(h);
                let image = procs
                    .get(&pid)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default();
                (h, image)
            }
            None => ancestor_window(chain, &all, basename)?,
        };
        let class = class_of(hwnd);
        if class == PSEUDO_CONSOLE_CLASS || !WINDOW_CLASSES.contains(&class.as_str()) {
            return None;
        }
        let owner_pid = pid_of(hwnd);
        let owner_start = process_start(owner_pid)?;
        let marker = wide(&crate::focus::window_marker(session));
        // The value is a small constant; only its presence is ever read.
        if unsafe { SetPropW(hwnd, marker.as_ptr(), 1usize as HANDLE) } == 0 {
            return None;
        }
        Some(WindowIdentity {
            handle: hwnd as usize as u64,
            owner_pid,
            owner_start,
            class: class.clone(),
            host: crate::focus::host_kind(&class, &owner_image).to_string(),
        })
    }

    /// Every particular must still match: the handle is a window, its class
    /// and owner are the recorded ones, the owner was created when the record
    /// says, and the capture-time marker is still on it. A handle recycled
    /// inside a surviving terminal keeps the first three, so the marker is
    /// what proves it is the captured window (KTD9).
    pub fn window_verifies(window: &WindowIdentity, session: &str) -> bool {
        let hwnd = window.handle as usize as HWND;
        if unsafe { IsWindow(hwnd) } == 0 {
            return false;
        }
        let class = class_of(hwnd);
        if class == PSEUDO_CONSOLE_CLASS || class != window.class {
            return false;
        }
        if pid_of(hwnd) != window.owner_pid {
            return false;
        }
        if process_start(window.owner_pid) != Some(window.owner_start) {
            return false;
        }
        let marker = wide(&crate::focus::window_marker(session));
        !unsafe { GetPropW(hwnd, marker.as_ptr()) }.is_null()
    }

    pub fn raise_window(_handle: u64) -> bool {
        false
    }

    /// Reads `<key>\shell\open\command`'s default value from HKCU.
    ///
    /// A registry read on the notify path, once per visual alert, never per
    /// tick. `RegGetValueW` with `RRF_RT_REG_SZ` refuses any other value type
    /// and returns a terminated string, so an expandable or binary value
    /// planted there reads as unregistered rather than as a command.
    pub fn registered_protocol_command(key: &str) -> Option<String> {
        let path = wide(&format!("{key}\\shell\\open\\command"));
        let mut len: u32 = 0;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if rc != ERROR_SUCCESS || len == 0 || len > 8192 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                &mut len,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::{ProcessInfo, TOKEN_BYTES};

    pub fn random_bytes() -> Option<[u8; TOKEN_BYTES]> {
        super::urandom()
    }

    pub fn ancestor_chain() -> Vec<ProcessInfo> {
        Vec::new()
    }

    pub fn process_start(_pid: u64) -> Option<u64> {
        None
    }

    pub fn boot_id() -> Option<String> {
        None
    }

    pub fn controlling_tty() -> Option<String> {
        None
    }

    pub fn ancestors_of(_pid: u64) -> Vec<ProcessInfo> {
        Vec::new()
    }

    pub fn registered_protocol_command(_key: &str) -> Option<String> {
        None
    }

    pub fn capture_window(
        _chain: &[ProcessInfo],
        _anchor_pid: u64,
        _session: &str,
        _basename: &str,
    ) -> Option<crate::focus::WindowIdentity> {
        None
    }

    pub fn window_verifies(_window: &crate::focus::WindowIdentity, _session: &str) -> bool {
        false
    }

    pub fn raise_window(_handle: u64) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Click path: probes and executors
// ---------------------------------------------------------------------------

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::cmd::focus::{DbusStyle, Probes, Step, TabFamily, TmuxClient, Tool, X11Tool};
use crate::cmd::notify::Platform;
use crate::focus::Record;

/// How long a terminal or desktop tool may take. These are local IPC calls
/// that answer in milliseconds; a stuck one degrades to a missed step.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// osascript may have to wake the target application and, the first time,
/// wait for the Automation consent prompt to be answered.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(8);

/// How often a running tool is checked against its deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Selects the Terminal.app tab whose tty is argv's first item and fronts its
/// window. Reads its input through `on run argv`, never by interpolation
/// (KTD10). Returns without touching anything when Terminal is not running,
/// so a stale record cannot launch it.
pub const TERMINAL_SELECT_TAB: &str = r#"on run argv
set target to item 1 of argv
if application "Terminal" is not running then return
tell application "Terminal"
repeat with w in windows
repeat with t in tabs of w
if tty of t is target then
set selected tab of w to t
set frontmost of w to true
return
end if
end repeat
end repeat
end tell
end run"#;

/// The iTerm2 equivalent: sessions carry the tty, and `select` on the
/// session, its tab and its window walks the selection up.
pub const ITERM2_SELECT_TAB: &str = r#"on run argv
set target to item 1 of argv
if application "iTerm2" is not running then return
tell application "iTerm2"
repeat with w in windows
repeat with t in tabs of w
repeat with s in sessions of t
if tty of s is target then
tell w to select
tell t to select
tell s to select
return
end if
end repeat
end repeat
end repeat
end tell
end run"#;

/// Ghostty: argv is the tty and the working directory, either possibly empty.
/// The tty is exact and wins; the working directory proves neither ownership
/// nor continuity, so it selects only when exactly one terminal matches
/// (KTD9). `focus` is Ghostty's own command and fronts the window.
pub const GHOSTTY_SELECT: &str = r#"on run argv
set targetTty to item 1 of argv
set targetCwd to item 2 of argv
if application "Ghostty" is not running then return
tell application "Ghostty"
if targetTty is not "" then
repeat with t in terminals
if tty of t is targetTty then
focus t
return
end if
end repeat
end if
if targetCwd is not "" then
set matched to {}
repeat with t in terminals
if working directory of t is targetCwd then set end of matched to t
end repeat
if (count of matched) is 1 then focus (item 1 of matched)
end if
end tell
end run"#;

/// Where tools are looked for before `PATH`, in order (KTD10).
///
/// The click runs with the login session's environment on macOS, whose PATH
/// has none of Homebrew, MacPorts, cargo or the app bundles; a tool that only
/// resolves through PATH would be found from a shell and missed from a click.
pub fn candidate_dirs(platform: Platform) -> Vec<PathBuf> {
    let home = crate::home_dir().unwrap_or_default();
    let dirs: Vec<PathBuf> = match platform {
        Platform::Macos => vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/local/bin"),
            home.join(".cargo").join("bin"),
            home.join(".local").join("bin"),
            PathBuf::from("/Applications/kitty.app/Contents/MacOS"),
            PathBuf::from("/Applications/WezTerm.app/Contents/MacOS"),
            PathBuf::from("/Applications/Ghostty.app/Contents/MacOS"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ],
        Platform::Linux => vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/bin"),
            home.join(".local").join("bin"),
            home.join(".cargo").join("bin"),
            PathBuf::from("/snap/bin"),
        ],
        // No Unix tool is ever planned on Windows; the helper spawns nothing.
        Platform::Windows => Vec::new(),
    };
    dirs
}

/// Resolves a tool to the absolute path it will be spawned by: the candidate
/// list first, `PATH` last.
pub fn resolve_tool(platform: Platform, tool: Tool) -> Option<PathBuf> {
    let name = tool.name();
    for dir in candidate_dirs(platform) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    crate::platform::notify::which(name).filter(|p| p.is_absolute())
}

/// Runs a tool by absolute path with constant verbs and record fields as
/// separate arguments, bounded end to end, and returns its stdout on success.
///
/// The shape mirrors `git::run_bounded`: a drain thread so a chatty child
/// cannot deadlock on a full pipe, a poll against the deadline, a kill when
/// it lapses. Every failure is `None`; the caller logs the step name only.
fn run_tool(program: &Path, args: &[String], timeout: Duration) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        // The helper is a GUI-subsystem process with no console; a spawned
        // console program would otherwise get a fresh window. Nothing is
        // planned on Windows today, and this keeps that true for the tool
        // runner itself.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let name = program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            crate::debug::log(move || format!("focus: cannot run {name}: {e}"));
            return None;
        }
    };

    let mut pipe = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = pipe.as_mut() {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
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
                    crate::debug::log(|| "focus: tool killed at its deadline".to_string());
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };
    let budget = deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(Duration::from_millis(250));
    let stdout = rx.recv_timeout(budget).ok()?;
    if !status?.success() {
        return None;
    }
    Some(stdout)
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The clients attached to the recorded tmux server, by tty and pid. Lines
/// that do not parse as a tty and a pid are dropped rather than trusted.
fn tmux_clients(tmux: &Path, socket: &str) -> Vec<TmuxClient> {
    let out = run_tool(
        tmux,
        &args(&[
            "-S",
            socket,
            "list-clients",
            "-F",
            "#{client_tty} #{client_pid}",
        ]),
        TOOL_TIMEOUT,
    );
    let Some(out) = out else {
        return Vec::new();
    };
    lines(&out)
        .iter()
        .filter_map(|line| {
            let (tty, pid) = line.split_once(' ')?;
            if !crate::focus::is_tty(tty) || !pid.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(TmuxClient {
                tty: tty.to_string(),
                pid: pid.parse().ok()?,
            })
        })
        .collect()
}

/// Visible top-level X11 windows of `pid`, through xdotool's search. xdotool
/// exits non-zero when nothing matches, which reads as no windows.
fn xdotool_windows(tool: &Path, pid: u64) -> Vec<u64> {
    run_tool(
        tool,
        &args(&["search", "--onlyvisible", "--pid", &pid.to_string()]),
        TOOL_TIMEOUT,
    )
    .map(|out| lines(&out).iter().filter_map(|l| l.parse().ok()).collect())
    .unwrap_or_default()
}

/// wmctrl lists every window with its pid once; the caller filters.
fn wmctrl_windows(tool: &Path) -> Vec<(u64, u64)> {
    run_tool(tool, &args(&["-lp"]), TOOL_TIMEOUT)
        .map(|out| {
            lines(&out)
                .iter()
                .filter_map(|l| {
                    let mut parts = l.split_whitespace();
                    let id = parts.next()?;
                    let _desktop = parts.next()?;
                    let pid: u64 = parts.next()?.parse().ok()?;
                    let id = u64::from_str_radix(id.trim_start_matches("0x"), 16).ok()?;
                    Some((pid, id))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// kdotool ids are opaque strings; only a conservative alphabet is kept.
fn kdotool_windows(tool: &Path, pid: u64) -> Vec<String> {
    run_tool(
        tool,
        &args(&["search", "--pid", &pid.to_string()]),
        TOOL_TIMEOUT,
    )
    .map(|out| {
        lines(&out)
            .into_iter()
            .filter(|l| {
                l.len() <= 64
                    && l.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'{' | b'}' | b'-'))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The windows of `pid` or, when it owns none, of its nearest ancestor that
/// does: a tmux client's shell owns no window, its terminal does.
fn windows_up_the_chain<T>(pid: u64, search: impl Fn(u64) -> Vec<T>) -> Vec<T> {
    for candidate in imp::ancestors_of(pid) {
        let found = search(candidate.pid);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

fn anchor_alive(anchor: &crate::focus::Anchor) -> bool {
    if anchor.pid == 0 {
        return false;
    }
    if imp::process_start(anchor.pid) != Some(anchor.start) {
        return false;
    }
    match &anchor.boot_id {
        Some(recorded) => imp::boot_id().as_deref() == Some(recorded.as_str()),
        None => true,
    }
}

/// Everything the planner needs to know about this machine now.
pub fn probe(platform: Platform, record: &Record) -> Probes {
    let id = &record.identity;
    let mut probes = Probes::default();
    for tool in Tool::ALL {
        if let Some(path) = resolve_tool(platform, tool) {
            probes.tools.insert(tool, path);
        }
    }
    probes.anchor_alive = anchor_alive(&record.anchor);
    probes.terminal_alive = match (id.terminal_pid, id.terminal_start) {
        (Some(pid), Some(start)) => imp::process_start(pid) == Some(start),
        _ => false,
    };
    probes.window_verifies = id
        .window
        .as_ref()
        .is_some_and(|w| imp::window_verifies(w, &record.session));
    probes.kitty_socket_present = id
        .kitty_socket
        .as_deref()
        .and_then(|s| s.strip_prefix("unix:"))
        .is_some_and(|p| Path::new(p).exists());
    probes.kde = std::env::var("XDG_CURRENT_DESKTOP")
        .map(|d| d.to_ascii_uppercase().contains("KDE"))
        .unwrap_or(false);

    if let (Some(t), Some(tmux)) = (&id.tmux, probes.tools.get(&Tool::Tmux)) {
        probes.tmux_clients = tmux_clients(tmux, &t.socket);
    }

    if platform == Platform::Linux {
        let mut pids: Vec<u64> = Vec::new();
        if id.tmux.is_some() {
            pids.extend(probes.tmux_clients.iter().map(|c| c.pid));
        } else {
            pids.extend(id.vscode_pid);
            if probes.terminal_alive {
                pids.extend(id.terminal_pid);
            }
        }
        if id.session_type.as_deref() == Some("wayland") {
            if let Some(kd) = probes.tools.get(&Tool::Kdotool).cloned() {
                for pid in pids {
                    let found = windows_up_the_chain(pid, |p| kdotool_windows(&kd, p));
                    probes.kde_windows.insert(pid, found);
                }
            }
        } else if let Some(xd) = probes.tools.get(&Tool::Xdotool).cloned() {
            for pid in pids {
                let found = windows_up_the_chain(pid, |p| xdotool_windows(&xd, p));
                probes.x11_windows.insert(pid, found);
            }
        } else if let Some(wm) = probes.tools.get(&Tool::Wmctrl).cloned() {
            let all = wmctrl_windows(&wm);
            for pid in pids {
                let found = windows_up_the_chain(pid, |p| {
                    all.iter()
                        .filter(|(owner, _)| *owner == p)
                        .map(|(_, id)| *id)
                        .collect()
                });
                probes.x11_windows.insert(pid, found);
            }
        }
    }
    probes
}

/// Carries out one step. `false` means it did not complete; the caller logs
/// the step name and nothing else.
pub fn execute(step: &Step) -> bool {
    match step {
        Step::RaiseWindow { handle } => imp::raise_window(*handle),
        Step::ActivateX11 {
            tool,
            via,
            window_id,
        } => {
            let id = window_id.to_string();
            let argv = match via {
                X11Tool::Xdotool => args(&["windowactivate", "--sync", &id]),
                X11Tool::Wmctrl => args(&["-i", "-a", &id]),
            };
            run_tool(tool, &argv, TOOL_TIMEOUT).is_some()
        }
        Step::ActivateKde { tool, window_id } => {
            run_tool(tool, &args(&["windowactivate", window_id]), TOOL_TIMEOUT).is_some()
        }
        Step::SelectTab { tool, family, tty } => {
            let script = match family {
                TabFamily::Terminal => TERMINAL_SELECT_TAB,
                TabFamily::ITerm2 => ITERM2_SELECT_TAB,
                TabFamily::Ghostty => GHOSTTY_SELECT,
            };
            let argv = match family {
                TabFamily::Ghostty => args(&["-e", script, tty, ""]),
                _ => args(&["-e", script, tty]),
            };
            run_tool(tool, &argv, SCRIPT_TIMEOUT).is_some()
        }
        Step::SelectGhostty { tool, tty, cwd } => run_tool(
            tool,
            &args(&["-e", GHOSTTY_SELECT, tty, cwd]),
            SCRIPT_TIMEOUT,
        )
        .is_some(),
        Step::KittenFocus {
            tool,
            socket,
            window_id,
        } => run_tool(
            tool,
            &args(&[
                "@",
                "--to",
                socket,
                "focus-window",
                "--match",
                &format!("id:{window_id}"),
            ]),
            TOOL_TIMEOUT,
        )
        .is_some(),
        Step::WeztermActivate { tool, pane } => run_tool(
            tool,
            &args(&["cli", "activate-pane", "--pane-id", &pane.to_string()]),
            TOOL_TIMEOUT,
        )
        .is_some(),
        Step::KonsoleSetSession {
            tool,
            style,
            service,
            window,
            session,
        } => {
            let session = session.to_string();
            let argv = match style {
                DbusStyle::Qdbus => args(&[
                    service,
                    window,
                    "org.kde.konsole.Window.setCurrentSession",
                    &session,
                ]),
                DbusStyle::Busctl => args(&[
                    "--user",
                    "call",
                    service,
                    window,
                    "org.kde.konsole.Window",
                    "setCurrentSession",
                    "i",
                    &session,
                ]),
            };
            run_tool(tool, &argv, TOOL_TIMEOUT).is_some()
        }
        Step::TmuxSwitchClient {
            tool,
            socket,
            client_tty,
            pane,
        } => run_tool(
            tool,
            &args(&["-S", socket, "switch-client", "-c", client_tty, "-t", pane]),
            TOOL_TIMEOUT,
        )
        .is_some(),
        Step::TmuxSelectWindow { tool, socket, pane } => run_tool(
            tool,
            &args(&["-S", socket, "select-window", "-t", pane]),
            TOOL_TIMEOUT,
        )
        .is_some(),
        Step::TmuxSelectPane { tool, socket, pane } => run_tool(
            tool,
            &args(&["-S", socket, "select-pane", "-t", pane]),
            TOOL_TIMEOUT,
        )
        .is_some(),
        Step::ScreenSelect {
            tool,
            session,
            window,
        } => run_tool(
            tool,
            &args(&["-S", session, "-X", "select", &window.to_string()]),
            TOOL_TIMEOUT,
        )
        .is_some(),
        Step::ZellijFocus {
            tool,
            session,
            pane,
        } => {
            let pane = pane.to_string();
            let argv = match session {
                Some(name) => args(&["--session", name, "action", "focus-pane-id", &pane]),
                None => args(&["action", "focus-pane-id", &pane]),
            };
            run_tool(tool, &argv, TOOL_TIMEOUT).is_some()
        }
    }
}
