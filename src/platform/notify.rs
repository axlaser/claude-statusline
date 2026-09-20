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

use crate::cmd::notify::{Action, Env, Platform};
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

    Env {
        home,
        cwd,
        system_root,
        programs,
        files,
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

/// Carries out one planned action, swallowing every failure.
///
/// A notification helper that is missing, broken, or refuses to start is not an
/// error the user should see — the scripts redirect all of it to `/dev/null`
/// today, and the silent-degradation contract requires the same here.
pub fn execute(action: &Action) {
    match action {
        Action::Spawn {
            program,
            args,
            stdin,
            background,
        } => spawn(program, args, stdin.as_deref(), *background),
        Action::PlayWav(path) => play_wav(path),
        Action::Beep => beep(),
    }
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
