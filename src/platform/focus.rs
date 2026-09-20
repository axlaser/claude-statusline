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
pub fn observe() -> Observation {
    let chain = imp::ancestor_chain();
    let claude_pid = std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let (anchor, terminal) = focus::select_anchor(&chain, claude_pid);
    Observation {
        anchor: anchor.map(|p| Anchor {
            pid: p.pid,
            start: p.start,
            boot_id: imp::boot_id(),
        }),
        terminal: terminal.map(|p| (p.pid, p.start)),
        tty: imp::controlling_tty(),
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        window: None,
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
}

#[cfg(windows)]
mod imp {
    use super::{walk, ProcessInfo, TOKEN_BYTES};

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::Security::Cryptography::ProcessPrng;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
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
}
