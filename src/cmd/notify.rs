//! `notify` — the notification hub the hooks and the status line both call.
//!
//! Usage: `claude-statusline notify <event> [value]`, with the permission
//! event's hook payload on stdin.
//!
//! The whole unit is split into a pure [`plan`] and an impure executor
//! (`crate::platform::notify`). Everything interesting — which helper runs,
//! with which arguments, in which order — is decided by `plan`, so the
//! observable this component is tested on — the command and arguments it
//! invokes — can be asserted
//! from a table without spawning anything or raising a toast on the developer's
//! desktop.
//!
//! `plan` takes the platform as a parameter rather than reading `cfg!`, so one
//! host can assert all three platforms' behaviour. That is what makes the
//! captured macOS and Linux fixtures checkable from a Windows machine.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::NotifyConfig;
use crate::focus::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Linux,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Platform::Macos
        }
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Platform::Linux
        }
    }
}

/// One thing the notifier does.
///
/// Windows sound is not a `Spawn`: the shipped handler plays it in-process
/// through `System.Media.SoundPlayer`, and spawning a player instead would be
/// both slower and a visible behaviour change. That is also why Windows has no
/// captured sound fixture — an in-process call leaves nothing for a `PATH` shim
/// to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run an external helper. `stdin` is fed to the child; `background` means
    /// the notifier does not wait for it.
    Spawn {
        program: String,
        args: Vec<String>,
        stdin: Option<String>,
        background: bool,
    },
    /// Windows: play a `.wav` in-process, blocking, as `PlaySync` did.
    PlayWav(PathBuf),
    /// Windows: the system asterisk, when no `.wav` resolved.
    Beep,
}

/// Everything about the machine that changes what gets invoked.
///
/// Injected rather than probed so a case can pin it. Which helpers exist
/// and which sound files are present are as much an input to this component as
/// the payload is — a fixture captured on a runner that had `paplay` is not
/// reproducible on a host that does not, and probing at assert time would make
/// the test pass or fail on the developer's installed packages.
#[derive(Debug, Default, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub cwd: PathBuf,
    /// Windows' `%SystemRoot%`.
    pub system_root: PathBuf,
    /// Programs resolvable on `PATH`.
    pub programs: BTreeSet<String>,
    /// Files that exist — sound assets and the notification icon.
    pub files: BTreeSet<PathBuf>,
    /// This binary's absolute path, for the macOS `-execute` action.
    pub binary: PathBuf,
    /// The launching application's bundle identifier on macOS, when the
    /// inherited environment names one.
    pub bundle_id: Option<String>,
    /// Windows: the `claude-statusline:` handler is registered and names the
    /// helper beside this binary, which is what makes a protocol toast safe
    /// to raise (KTD4).
    pub handler_registered: bool,
}

impl Env {
    fn has(&self, program: &str) -> bool {
        self.programs.contains(program)
    }

    fn file_exists(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    fn icon(&self, platform: Platform) -> Option<PathBuf> {
        let icon = join(platform, &self.home, &[".claude", "claude-icon.png"]);
        self.file_exists(&icon).then_some(icon)
    }
}

/// Joins path components with the separator the **target** platform uses, not
/// the host's.
///
/// `plan` is asserted for all three platforms from whichever machine runs the
/// tests, and `PathBuf::join` would render a macOS icon path with backslashes
/// on a Windows host — where the fixture, captured on a real macOS runner,
/// spells it with slashes. Every path the planner *emits* goes through here;
/// paths it merely compares against `Env::files` do too, so the two agree.
fn join(platform: Platform, base: &Path, parts: &[&str]) -> PathBuf {
    let sep = match platform {
        Platform::Windows => '\\',
        _ => '/',
    };
    let base = base.to_string_lossy();
    let mut out = base.trim_end_matches(['/', '\\']).to_string();
    for part in parts {
        out.push(sep);
        out.push_str(part);
    }
    PathBuf::from(out)
}

/// The title every notification carries.
pub const TITLE: &str = "Claude Code";

/// Longest tool detail rendered into a permission message, in characters.
const DETAIL_LIMIT: usize = 80;

/// The PowerShell that raises the Windows toast.
///
/// **This body is a compile-time constant and must stay one.** The message is
/// attacker-influenceable — on a permission event it is `tool_input.command`,
/// which is whatever the model was about to run — so it crosses the interpreter
/// boundary on **stdin**, as data, and never as script text.
/// `notify_argv_never_carries_the_message` asserts exactly that.
///
/// It contains no double quotes, so the Windows command-line encoding that
/// `std::process::Command` applies cannot alter what PowerShell parses.
///
/// Verified on Windows 11 / PowerShell 5.1 on 2026-07-27 by running this exact
/// invocation shape with a body that echoes back what it parsed: six hostile
/// messages — shell metacharacters, an embedded newline, `%PATH%`, doubled and
/// escaped quotes, backslashes — all arrived byte-identical, with exit 0 and
/// empty stderr. That covers every step where the text could be evaluated or
/// mangled. It does **not** cover BurntToast rendering the string, which is a
/// visual fact, and only a live session on Windows can confirm it.
pub const WINDOWS_TOAST_SCRIPT: &str = concat!(
    "$ErrorActionPreference='SilentlyContinue';",
    "$p=[Console]::In.ReadToEnd()|ConvertFrom-Json;",
    "if(-not $p.message){exit 0};",
    "if(-not (Get-Module -ListAvailable -Name BurntToast)){exit 0};",
    "Import-Module BurntToast;",
    "$hasIcon=[bool]($p.icon -and (Test-Path -LiteralPath $p.icon));",
    "$done=$false;",
    // The click transport (KTD4): protocol activation with the launch URI the
    // stdin JSON carries. Never `-AppId` — the default identity is what keeps
    // the toast rendering as it does today. Any failure falls through to the
    // cmdlet below, so a BurntToast without these cmdlets still toasts.
    "if($p.launch){",
    "try{",
    "$ErrorActionPreference='Stop';",
    "$t=@((New-BTText -Text $p.title),(New-BTText -Text $p.message));",
    "if($hasIcon){$b=New-BTBinding -Children $t -AppLogoOverride (New-BTImage -Source $p.icon -AppLogoOverride)}",
    "else{$b=New-BTBinding -Children $t};",
    "$v=New-BTVisual -BindingGeneric $b;",
    "$a=New-BTAudio -Silent;",
    "$c=New-BTContent -Visual $v -Audio $a -ActivationType Protocol -Launch $p.launch;",
    "Submit-BTNotification -Content $c;",
    "$done=$true",
    "}catch{$done=$false};",
    "$ErrorActionPreference='SilentlyContinue'",
    "};",
    "if(-not $done){",
    "if($hasIcon){",
    "New-BurntToastNotification -Text $p.title,$p.message -AppLogo $p.icon -Silent",
    "}else{",
    "New-BurntToastNotification -Text $p.title,$p.message -Silent",
    "}",
    "}",
);

/// The sound each event plays, per platform. Kept verbatim from the scripts.
fn sound_file(platform: Platform, event: &str) -> Option<&'static str> {
    let name = match (platform, event) {
        (Platform::Macos, "permission" | "compaction_start") => "Tink.aiff",
        (Platform::Macos, "stop" | "compaction_done") => "Glass.aiff",
        (Platform::Macos, "rate_limit" | "context_high") => "Sosumi.aiff",

        (Platform::Linux, "permission" | "compaction_start") => "bell.oga",
        (Platform::Linux, "stop" | "compaction_done") => "complete.oga",
        (Platform::Linux, "rate_limit" | "context_high") => "dialog-warning.oga",

        (Platform::Windows, "permission") => "Windows Exclamation.wav",
        (Platform::Windows, "stop" | "compaction_done") => "chimes.wav",
        (Platform::Windows, "compaction_start") => "Windows Battery Low.wav",
        (Platform::Windows, "rate_limit" | "context_high") => "Windows Battery Critical.wav",

        _ => return None,
    };
    Some(name)
}

/// The Linux players, in the order the script tries them.
const LINUX_PLAYERS: [&str; 3] = ["paplay", "ffplay", "ogg123"];

/// Builds the message text for an event.
///
/// The permission case is the only one that reads the payload: it names the
/// tool and, for the tools whose argument is worth seeing, its command or file
/// path. An unparseable payload degrades to the bare prompt rather than
/// dropping the notification — the user still needs to know something is
/// waiting.
pub fn message(platform: Platform, event: &str, value: &str, stdin: &str, cwd: &Path) -> String {
    match event {
        "permission" => permission_message(platform, stdin, cwd),
        "stop" => "Finished working".to_string(),
        "compaction_start" => "Compacting context...".to_string(),
        "compaction_done" => "Context compacted".to_string(),
        "rate_limit" => format!("Rate limit at {value}%"),
        "context_high" => format!("Context window at {value}%"),
        _ => String::new(),
    }
}

fn permission_message(platform: Platform, stdin: &str, cwd: &Path) -> String {
    const WAITING: &str = "Waiting for permission";

    let Ok(payload) = serde_json::from_str::<Value>(stdin) else {
        return WAITING.to_string();
    };
    let Some(tool) = payload.get("tool_name").and_then(Value::as_str) else {
        return WAITING.to_string();
    };
    if tool.is_empty() {
        return WAITING.to_string();
    }

    let detail = match tool {
        "Bash" => payload
            .get("tool_input")
            .and_then(|i| i.get("command"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        "Edit" | "Write" | "Read" => payload
            .get("tool_input")
            .and_then(|i| i.get("file_path"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        _ => "",
    };
    if detail.is_empty() {
        return tool.to_string();
    }

    let mut detail = detail.to_string();
    if matches!(tool, "Edit" | "Write" | "Read") {
        detail = strip_cwd(platform, &detail, cwd);
    }
    // Characters, not bytes: bash counts characters here under a UTF-8 locale,
    // and slicing a multi-byte character in half would render a replacement
    // glyph in the notification.
    let detail: String = detail.chars().take(DETAIL_LIMIT).collect();
    format!("{tool}: {detail}")
}

/// Drops the working-directory prefix from a file path, so a notification shows
/// `src/main.rs` rather than the user's whole home directory.
///
/// Windows compares case-insensitively and joins with a backslash; the bash
/// scripts compare exactly and join with a slash. Preserved as-is: a path that
/// differs only in case is the same file on Windows and two different files on
/// Linux, so unifying this would be wrong on one of them.
fn strip_cwd(platform: Platform, detail: &str, cwd: &Path) -> String {
    let cwd = cwd.to_string_lossy();
    let (cwd, sep) = match platform {
        Platform::Windows => (cwd.trim_end_matches('\\').to_string(), '\\'),
        _ => (cwd.to_string(), '/'),
    };
    if cwd.is_empty() {
        return detail.to_string();
    }
    let prefix = format!("{cwd}{sep}");

    let matches = match platform {
        Platform::Windows => detail.to_lowercase().starts_with(&prefix.to_lowercase()),
        _ => detail.starts_with(&prefix),
    };
    if !matches {
        return detail.to_string();
    }
    match platform {
        // Case folding can change a char's UTF-8 length — U+212A KELVIN SIGN
        // lowercases to a one-byte ASCII `k` — so the region of `detail` that
        // matched is not necessarily `prefix.len()` bytes long. Slicing at
        // `prefix.len()` can land mid-char and panic, which the entry-point
        // `catch_unwind` swallows into a dropped notification. Walk `detail`
        // accumulating folded widths to find where the match actually ends.
        Platform::Windows => {
            let want = prefix.to_lowercase().len();
            let mut folded = 0usize;
            let mut end = 0usize;
            for (offset, ch) in detail.char_indices() {
                if folded >= want {
                    break;
                }
                folded += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
                end = offset + ch.len_utf8();
            }
            detail[end..].to_string()
        }
        // The other platforms compare and slice the same bytes, so the
        // prefix length is the matched length by construction.
        _ => detail[prefix.len()..].to_string(),
    }
}

/// Decides everything this invocation will do, without doing any of it.
///
/// Order is preserved per platform and is not cosmetic: the bash scripts
/// background the sound and then raise the visual, while the Windows handler
/// raises the toast first and plays its sound synchronously afterwards so the
/// toast is not delayed behind the audio.
///
/// `key` is the click key the alert captured, passed as its own parameter
/// rather than probed into `Env` (KTD16): it is the product of a write that
/// just happened, not a fact about the machine. With no key the plan is
/// exactly today's, which is what keeps the captured fixtures untouched.
pub fn plan(
    platform: Platform,
    event: &str,
    value: &str,
    stdin: &str,
    cfg: &NotifyConfig,
    env: &Env,
    key: Option<&Key>,
) -> Vec<Action> {
    if event.is_empty() {
        return Vec::new();
    }
    let flags = cfg.event(event);
    let msg = message(platform, event, value, stdin, &env.cwd);

    let mut actions = Vec::new();
    match platform {
        Platform::Windows => {
            if flags.visual && !msg.is_empty() {
                actions.push(windows_toast(&msg, env, key));
            }
            if flags.sound {
                actions.push(windows_sound(event, env));
            }
        }
        _ => {
            if flags.sound {
                actions.extend(unix_sound(platform, event, env));
            }
            if flags.visual && !msg.is_empty() {
                actions.extend(unix_visual(platform, &msg, env));
            }
        }
    }
    actions
}

fn unix_sound(platform: Platform, event: &str, env: &Env) -> Option<Action> {
    let file = sound_file(platform, event)?;
    match platform {
        // The macOS script never checks that the file or `afplay` exists — it
        // runs the command and lets a missing one fail into /dev/null. Probing
        // first would change the observable on any machine where the check
        // fails, so this does the same and tolerates the spawn failing.
        Platform::Macos => Some(Action::Spawn {
            program: "afplay".to_string(),
            args: vec![format!("/System/Library/Sounds/{file}")],
            stdin: None,
            background: true,
        }),
        // Linux does check: the asset ships in a package that is often absent,
        // and three different players might be installed.
        Platform::Linux => {
            let path = PathBuf::from(format!("/usr/share/sounds/freedesktop/stereo/{file}"));
            if !env.file_exists(&path) {
                return None;
            }
            let player = LINUX_PLAYERS.iter().find(|p| env.has(p))?;
            let args = match *player {
                "ffplay" => vec![
                    "-nodisp".to_string(),
                    "-autoexit".to_string(),
                    "-loglevel".to_string(),
                    "quiet".to_string(),
                    path.to_string_lossy().into_owned(),
                ],
                "ogg123" => vec!["-q".to_string(), path.to_string_lossy().into_owned()],
                _ => vec![path.to_string_lossy().into_owned()],
            };
            Some(Action::Spawn {
                program: (*player).to_string(),
                args,
                stdin: None,
                background: true,
            })
        }
        Platform::Windows => None,
    }
}

fn unix_visual(platform: Platform, msg: &str, env: &Env) -> Option<Action> {
    let icon = env.icon(platform);
    match platform {
        Platform::Macos => {
            if !env.has("terminal-notifier") {
                return None;
            }
            let mut args = vec![
                "-title".to_string(),
                TITLE.to_string(),
                "-message".to_string(),
                msg.to_string(),
            ];
            if let Some(icon) = icon {
                let icon = icon.to_string_lossy().into_owned();
                args.push("-appIcon".to_string());
                args.push(icon.clone());
                args.push("-contentImage".to_string());
                args.push(icon);
            }
            Some(Action::Spawn {
                program: "terminal-notifier".to_string(),
                args,
                stdin: None,
                background: false,
            })
        }
        Platform::Linux => {
            if !env.has("notify-send") {
                return None;
            }
            let mut args = vec![
                TITLE.to_string(),
                msg.to_string(),
                "--urgency=normal".to_string(),
            ];
            if let Some(icon) = icon {
                args.push(format!("--icon={}", icon.to_string_lossy()));
            }
            Some(Action::Spawn {
                program: "notify-send".to_string(),
                args,
                stdin: None,
                background: false,
            })
        }
        Platform::Windows => None,
    }
}

/// The Windows toast, as a `powershell.exe` invocation.
///
/// Three things here are load-bearing:
///
/// - The interpreter is addressed by **absolute path** under `%SystemRoot%`,
///   never by `PATH` or the current directory. A `powershell.exe` dropped in
///   the working directory must never be what raises the notification.
/// - `-NoProfile`, so a user profile cannot change what the script means.
/// - The message travels on **stdin as JSON**, never inside the `-Command`
///   body. This is what keeps a hostile message harmless by construction rather than by
///   escaping: no quoting rule has to be right, because the text is never
///   parsed as code.
///
/// Unlike the shipped handler this does not first probe for BurntToast — that
/// check now lives inside the script, which costs nothing and saves a second
/// interpreter launch just to answer a question the script can answer itself.
///
/// The click key travels the same way as the message, inside the stdin JSON as
/// `launch`, and only when the handler is registered (AE6): an unregistered
/// machine gets today's toast, and a click on it keeps today's behaviour (R9).
/// The launch attribute is XML on the other side; the key alphabet's exclusion
/// of `&<>"'` is what keeps it inert there.
fn windows_toast(msg: &str, env: &Env, key: Option<&Key>) -> Action {
    let launch = match key {
        Some(key) if env.handler_registered => Some(key.uri()),
        _ => None,
    };
    let payload = serde_json::json!({
        "title": TITLE,
        "message": msg,
        "icon": env.icon(Platform::Windows).map(|p| p.to_string_lossy().into_owned()),
        "launch": launch,
    });
    Action::Spawn {
        program: powershell_path(&env.system_root)
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            WINDOWS_TOAST_SCRIPT.to_string(),
        ],
        stdin: Some(payload.to_string()),
        background: false,
    }
}

/// The helper executable's name and where it sits: beside the binary.
pub const FOCUS_HELPER: &str = "claude-statusline-focus.exe";

pub fn helper_beside(binary: &Path) -> PathBuf {
    let dir = binary.parent().unwrap_or(binary);
    join(Platform::Windows, dir, &[FOCUS_HELPER])
}

/// The open command the registration writes: the helper quoted, then `"%1"`.
pub fn protocol_command(helper: &Path) -> String {
    format!("\"{}\" \"%1\"", helper.to_string_lossy())
}

/// Whether a registered open command names exactly the helper beside this
/// binary (KTD5). The comparison folds case because the path came through the
/// registry and the installer may have spelled the drive differently; it does
/// not tolerate any other difference.
pub fn handler_command_matches(command: &str, helper: &Path) -> bool {
    command.eq_ignore_ascii_case(&protocol_command(helper))
}

/// Windows PowerShell 5.1's fixed location.
pub fn powershell_path(system_root: &Path) -> PathBuf {
    join(
        Platform::Windows,
        system_root,
        &["System32", "WindowsPowerShell", "v1.0", "powershell.exe"],
    )
}

fn windows_sound(event: &str, env: &Env) -> Action {
    match sound_file(Platform::Windows, event) {
        Some(file) => {
            let path = join(Platform::Windows, &env.system_root, &["Media", file]);
            if env.file_exists(&path) {
                Action::PlayWav(path)
            } else {
                Action::Beep
            }
        }
        // An event with no sound of its own still beeps, which is what the
        // shipped handler's `elseif ($soundEnabled)` branch does.
        None => Action::Beep,
    }
}
