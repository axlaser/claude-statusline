//! Executing what `cmd::notify` planned, and probing the machine it planned
//! against.
//!
//! Everything here is impure. The decisions all live in `cmd::notify::plan`;
//! this file only carries them out and reports the environment the planner
//! reads. Keeping the split sharp is what lets the case table assert
//! what this component invokes without spawning a single process.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cmd::notify::{Action, Env, Platform, LINUX_CLICK_ACTION};
use crate::debug;

/// Probes this machine for the facts the planner needs.
///
/// Only the specific helpers and assets the planner can ask about are resolved,
/// rather than everything on `PATH`: this runs on a notification, and walking
/// `PATH` for its own sake would be work with no output.
pub fn probe_env() -> Env {
    let home = crate::home_dir().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));

    let platform = Platform::current();
    let mut programs = BTreeSet::new();
    for candidate in helpers(platform) {
        if which(candidate).is_some() {
            programs.insert(candidate.to_string());
        }
    }

    let mut files = BTreeSet::new();
    let icon = home.join(".claude").join("claude-icon.png");
    if icon.is_file() {
        files.insert(icon);
    }
    for asset in sound_assets(platform, &system_root) {
        if asset.is_file() {
            files.insert(asset);
        }
    }

    let binary = std::env::current_exe().unwrap_or_default();
    let handler_registered = platform == Platform::Windows && {
        let helper = crate::cmd::notify::helper_beside(&binary);
        crate::platform::focus::registered_protocol_command()
            .is_some_and(|c| crate::cmd::notify::handler_command_matches(&c, &helper))
            && helper.is_file()
    };
    Env {
        home,
        cwd,
        system_root,
        programs,
        files,
        binary,
        // The bundle identifier the launching app hands every child; empty
        // outside macOS and under an app that does not set it.
        bundle_id: std::env::var("__CFBundleIdentifier")
            .ok()
            .filter(|v| !v.is_empty()),
        term_program: std::env::var("TERM_PROGRAM").ok().filter(|v| !v.is_empty()),
        handler_registered,
    }
}

/// The helpers each platform's planner can name.
fn helpers(platform: Platform) -> &'static [&'static str] {
    match platform {
        Platform::Macos => &["terminal-notifier"],
        Platform::Linux => &["notify-send", "paplay", "ffplay", "ogg123"],
        // The Windows toast is reached by absolute path, never through `PATH`.
        Platform::Windows => &[],
    }
}

/// Every sound file any event could resolve to, so one probe covers them all.
fn sound_assets(platform: Platform, system_root: &Path) -> Vec<PathBuf> {
    match platform {
        // macOS never checks: it hands the path to `afplay` and tolerates
        // failure, exactly as the script did.
        Platform::Macos => Vec::new(),
        Platform::Linux => ["bell.oga", "complete.oga", "dialog-warning.oga"]
            .iter()
            .map(|f| PathBuf::from(format!("/usr/share/sounds/freedesktop/stereo/{f}")))
            .collect(),
        Platform::Windows => [
            "Windows Exclamation.wav",
            "chimes.wav",
            "Windows Battery Low.wav",
            "Windows Battery Critical.wav",
        ]
        .iter()
        .map(|f| system_root.join("Media").join(f))
        .collect(),
    }
}

/// Resolves a program on `PATH`, honouring `PATHEXT` on Windows.
pub(crate) fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat", "com"] {
                let candidate = dir.join(format!("{program}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Carries out one planned action, swallowing every failure, and returns the
/// click key when the action waited for a click and got one.
///
/// A notification helper that is missing, broken, or refuses to start is not an
/// error the user should see — the scripts redirect all of it to `/dev/null`
/// today, and the silent-degradation contract requires the same here. The
/// returned key goes back to the caller (`main.rs`), which runs the focus
/// handler; this module never calls up into focus orchestration (KTD3).
pub fn execute(action: &Action) -> Option<String> {
    match action {
        Action::Spawn {
            program,
            args,
            click: Some(click),
            ..
        } => wait_for_click(
            program,
            args,
            &click.key,
            std::time::Duration::from_secs(click.deadline_secs),
        ),
        Action::Spawn {
            program,
            args,
            stdin,
            background,
            click: None,
        } => {
            spawn(program, args, stdin.as_deref(), *background);
            None
        }
        Action::PlayWav(path) => {
            play_wav(path);
            None
        }
        Action::Beep => {
            beep();
            None
        }
    }
}

/// A child's run, bounded: its status if it exited, what it wrote, how long
/// it took, and whether the deadline killed it.
struct ClickRun {
    status: Option<std::process::ExitStatus>,
    stdout: Vec<u8>,
    elapsed: std::time::Duration,
    timed_out: bool,
}

/// Runs notify-send with stdout piped and returns when it exits or the
/// deadline lapses, whichever is first. The same drain-thread shape as the
/// bounded runners elsewhere: a child that fills the pipe must not block, and
/// a kill must not wait on a reader that never finishes.
fn run_for_click(
    program: &str,
    args: &[String],
    deadline: std::time::Duration,
) -> Option<ClickRun> {
    let started = std::time::Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let program = program.to_string();
            debug::log(move || format!("notify: cannot spawn {program}: {e}"));
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
    let end = started + deadline;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if std::time::Instant::now() >= end {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };
    // A child that exited has closed its end of the pipe, so the drain
    // finishes as soon as the thread is scheduled — but on a loaded machine
    // that can be later than a few hundred milliseconds, and a click read
    // as a dismissal is the one outcome that must not depend on load. A
    // killed child gets the short budget: something it spawned may still
    // hold the pipe, and there is nothing left to read from it anyway.
    let drain = if timed_out {
        std::time::Duration::from_millis(250)
    } else {
        std::time::Duration::from_secs(2)
    };
    let stdout = rx.recv_timeout(drain).unwrap_or_default();
    Some(ClickRun {
        status,
        stdout,
        elapsed: started.elapsed(),
        timed_out,
    })
}

/// Raises the toast and waits for its click (KTD3, AE2).
///
/// A first line of `default` on stdout is the click, and the key comes back.
/// A non-zero exit within the first second with nothing on stdout is an old
/// libnotify rejecting the action flag: the toast is re-raised once without
/// it, the one place the executor's argv diverges from the plan, and that
/// second run is not waited on for a click because it cannot report one. A
/// lapsed deadline terminates notify-send and yields no click.
pub fn wait_for_click(
    program: &str,
    args: &[String],
    key: &str,
    deadline: std::time::Duration,
) -> Option<String> {
    let run = run_for_click(program, args, deadline)?;
    if run.timed_out {
        debug::log(|| "notify: click window closed, notify-send terminated".to_string());
        return None;
    }
    let rejected = run.status.is_some_and(|s| !s.success())
        && run.elapsed < std::time::Duration::from_secs(1)
        && run.stdout.iter().all(u8::is_ascii_whitespace);
    if rejected {
        debug::log(|| "notify: the action flag was rejected, re-raising without it".to_string());
        let without: Vec<String> = strip_click_action(args);
        spawn(program, &without, None, false);
        return None;
    }
    let first = String::from_utf8_lossy(&run.stdout);
    let first = first.lines().next().unwrap_or("").trim();
    if first == "default" {
        return Some(key.to_string());
    }
    None
}

/// The argv without the `-A default=Focus` pair.
pub fn strip_click_action(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "-A" {
            skip_next = true;
            continue;
        }
        if arg == LINUX_CLICK_ACTION {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

fn spawn(program: &str, args: &[String], stdin: Option<&str>, background: bool) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    #[cfg(windows)]
    {
        // The third spawn site, and the one that needs this most: the toast is
        // launched through the console-subsystem interpreter, and its parent is
        // the detached `notify` child that `notify_state::spawn` deliberately
        // created with no console of its own. There is nothing to inherit, so
        // without this a window flashes on every visible notification.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let program = program.to_string();
            debug::log(move || format!("notify: cannot spawn {program}: {e}"));
            return;
        }
    };

    if let Some(payload) = stdin {
        // A write failure here is routine, not exceptional: the child may have
        // exited already — BurntToast absent, for instance — and a broken pipe
        // must not become a visible failure.
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(payload.as_bytes());
        }
        // Dropped explicitly: the child blocks on `ReadToEnd` until the pipe
        // closes, so holding it open would hang the notifier.
    }

    if background {
        // Deliberately not awaited. The sound outlives this process, which is
        // what the scripts' `&` achieves — the notification must not wait for
        // audio to finish playing.
        return;
    }
    let _ = child.wait();
}

#[cfg(windows)]
fn play_wav(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_NODEFAULT, SND_SYNC};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // Synchronous, matching the handler's `PlaySync()`. An async play would be
    // cut off the instant this short-lived process exits.
    unsafe {
        PlaySoundW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_SYNC | SND_NODEFAULT,
        );
    }
}

#[cfg(not(windows))]
fn play_wav(path: &Path) {
    let path = path.display().to_string();
    debug::log(move || format!("notify: PlayWav is Windows-only, ignoring {path}"));
}

#[cfg(windows)]
fn beep() {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_SYNC};

    // `SystemSounds::Asterisk.Play()` in the shipped handler, reached without a
    // CLR: the alias resolves to whatever the user has configured for that
    // system event, which is what makes it *their* asterisk and not a wav this
    // tool picked.
    let alias: Vec<u16> = "SystemAsterisk".encode_utf16().chain(Some(0)).collect();
    unsafe {
        PlaySoundW(alias.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_SYNC);
    }
}

#[cfg(not(windows))]
fn beep() {
    debug::log(|| "notify: Beep is Windows-only, ignoring".to_string());
}
