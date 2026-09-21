//! `notify` — the notification hub the hooks and the status line both call.
//!
//! Usage: `claude-statusline notify <event> [value]`, with the permission
//! event's hook payload on stdin.
//!
//! Split into a pure [`plan`] and an impure executor
//! (`crate::platform::notify`): `plan` decides which helper runs, with which
//! arguments, in which order, so the case table asserts the invoked command
//! without spawning anything. `plan` takes the platform as a parameter rather
//! than reading `cfg!`, so one host can assert all three platforms — that is
//! what makes the captured macOS and Linux fixtures checkable from Windows.

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

/// Linux: the toast's click is observed by waiting on notify-send.
/// The action flag implies `--wait`, which GNOME does not reliably bound, so
/// the executor owns the deadline: a first stdout line of `default` is the
/// click, and the child is terminated when the deadline lapses. The key
/// travels here, not in argv, so `as_records` and the fixtures never see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickWait {
    pub key: String,
    pub deadline_secs: u64,
}

/// How long a Linux toast stays clickable, and so the notify process's
/// lifetime per visual alert. Revisit only with real-desktop evidence.
pub const LINUX_CLICK_WAIT_SECS: u64 = 120;

/// The notify-send action: the body click, reported as `default` on stdout.
pub const LINUX_CLICK_ACTION: &str = "default=Focus";

/// One thing the notifier does.
///
/// Windows sound is not a `Spawn`: the shipped handler plays it in-process
/// through `System.Media.SoundPlayer`, and a spawned player would be slower
/// and a visible behaviour change — which is also why Windows has no captured
/// sound fixture: an in-process call leaves nothing for a `PATH` shim to
/// record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run an external helper. `stdin` is fed to the child; `background` means
    /// the notifier does not wait for it; `click` means the executor waits for
    /// the toast's click instead and hands the key back.
    Spawn {
        program: String,
        args: Vec<String>,
        stdin: Option<String>,
        background: bool,
        click: Option<ClickWait>,
    },
    /// Windows: play a `.wav` in-process, blocking, as `PlaySync` did.
    PlayWav(PathBuf),
    /// Windows: the system asterisk, when no `.wav` resolved.
    Beep,
}

/// Everything about the machine that changes what gets invoked.
///
/// Injected rather than probed so a case can pin it: a fixture captured on a
/// runner that had `paplay` is not reproducible on a host that does not, and
/// probing at assert time would make the test depend on installed packages.
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
    /// macOS: the launching application's bundle identifier, when inherited.
    pub bundle_id: Option<String>,
    /// `TERM_PROGRAM`, the fallback when no bundle identifier was
    /// exported.
    pub term_program: Option<String>,
    /// Windows: the `claude-statusline:` handler is registered and names the
    /// helper beside this binary; only then is a protocol toast safe.
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

    /// The application terminal-notifier activates on click: the inherited
    /// identifier first, then the known terminals by `TERM_PROGRAM`.
    pub fn activation_bundle(&self) -> Option<String> {
        if let Some(id) = &self.bundle_id {
            return Some(id.clone());
        }
        let program = self.term_program.as_deref()?;
        bundle_for_term_program(program).map(str::to_string)
    }
}

/// The bundle identifiers of the terminals that export `TERM_PROGRAM`.
/// Warp's is the one shipped by Warp itself; nothing here is guessed.
pub fn bundle_for_term_program(program: &str) -> Option<&'static str> {
    Some(match program {
        "Apple_Terminal" => "com.apple.Terminal",
        "iTerm.app" => "com.googlecode.iterm2",
        "ghostty" => "com.mitchellh.ghostty",
        "vscode" => "com.microsoft.VSCode",
        "WezTerm" => "com.github.wez.wezterm",
        "kitty" => "net.kovidgoyal.kitty",
        "Alacritty" => "org.alacritty",
        "WarpTerminal" => "dev.warp.Warp-Stable",
        _ => return None,
    })
}

/// The `-execute` value: the binary's absolute path in single quotes, `focus`,
/// and the key. terminal-notifier hands it to `/bin/sh -c`, which
/// expands nothing inside single quotes, so a path that contains a single
/// quote, a control byte, or is not absolute yields no command at all rather
/// than a differently quoted one.
pub fn execute_command(binary: &Path, key: &Key) -> Option<String> {
    let path = binary.to_str()?;
    if !crate::focus::is_abs_path(path) || path.contains('\'') {
        return None;
    }
    Some(format!("'{path}' focus {}", key.as_string()))
}

/// Joins with the **target** platform's separator, not the host's: `plan` is
/// asserted for all three platforms from one machine, and `PathBuf::join`
/// would render a macOS icon path with backslashes on Windows where the
/// fixture spells it with slashes. Every path the planner emits or compares
/// against `Env::files` goes through here, so the two agree.
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
/// attacker-influenceable (`tool_input.command` on a permission event), so it
/// crosses the interpreter boundary on **stdin**, as data, never as script
/// text; `notify_argv_never_carries_the_message` asserts that. No double
/// quotes here, so the Windows command-line encoding cannot alter what
/// PowerShell parses.
///
/// Verified on Windows 11 / PowerShell 5.1 on 2026-07-27: six hostile messages
/// (metacharacters, an embedded newline, `%PATH%`, doubled and escaped quotes,
/// backslashes) arrived byte-identical, exit 0, empty stderr. That does **not**
/// cover BurntToast rendering the string; only a live Windows session can.
pub const WINDOWS_TOAST_SCRIPT: &str = concat!(
    "$ErrorActionPreference='SilentlyContinue';",
    "$p=[Console]::In.ReadToEnd()|ConvertFrom-Json;",
    "if(-not $p.message){exit 0};",
    "if(-not (Get-Module -ListAvailable -Name BurntToast)){exit 0};",
    "Import-Module BurntToast;",
    "$hasIcon=[bool]($p.icon -and (Test-Path -LiteralPath $p.icon));",
    "$done=$false;",
    // The click transport: protocol activation with the stdin JSON's
    // launch URI. Never `-AppId` — the default identity keeps the toast
    // rendering as today. Any failure falls through to the cmdlet below.
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

/// Builds the message text for an event. Only the permission case reads the
/// payload; an unparseable one degrades to the bare prompt rather than
/// dropping the notification — the user still needs to know something waits.
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
    // Characters, not bytes: bash counts characters under a UTF-8 locale, and
    // a half-sliced multi-byte character renders as a replacement glyph.
    let detail: String = detail.chars().take(DETAIL_LIMIT).collect();
    format!("{tool}: {detail}")
}

/// Drops the working-directory prefix from a file path. Windows compares
/// case-insensitively and joins with a backslash; the bash scripts compare
/// exactly and join with a slash. Preserved as-is: a path that differs only in
/// case is the same file on Windows and two files on Linux.
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
        // Case folding can change a char's UTF-8 length (U+212A KELVIN SIGN
        // lowercases to a one-byte `k`), so the matched region of `detail` is
        // not necessarily `prefix.len()` bytes; slicing there can panic
        // mid-char, and `catch_unwind` turns that into a dropped notification.
        // Walk `detail` accumulating folded widths to find the real end.
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
        // The other platforms compare and slice the same bytes.
        _ => detail[prefix.len()..].to_string(),
    }
}

/// Whether this event must stay silent because the session is not actually
/// finished. Only `stop` is gated: its "Finished working" text is a claim
/// about the whole turn, and a turn that ends with a subagent still running is
/// resumed the moment that agent hands back — so one request alerts twice and
/// the first one is false. Claude Code's `Stop` payload carries
/// `background_tasks` for exactly this, documented as distinguishing "session
/// is done" from "session is paused waiting for background work to wake it",
/// and empty when nothing is in flight.
///
/// Absent, null or unparseable reads as nothing running and stays audible: a
/// missing field must not silence the alert, because a lost notification is
/// the failure the user cannot see. `session_crons` is deliberately not read —
/// a scheduled wake-up is a later turn, not unfinished work in this one.
pub fn silenced_by_background_work(event: &str, stdin: &str) -> bool {
    if event != "stop" {
        return false;
    }
    serde_json::from_str::<Value>(stdin)
        .ok()
        .as_ref()
        .and_then(|payload| payload.get("background_tasks"))
        .and_then(Value::as_array)
        .is_some_and(|tasks| !tasks.is_empty())
}

/// Decides everything this invocation will do, without doing any of it.
///
/// Order per platform is not cosmetic: the bash scripts background the sound
/// and then raise the visual, while the Windows handler raises the toast first
/// and plays its sound synchronously so the toast is not delayed behind audio.
///
/// `key` is its own parameter rather than probed into `Env`: it is the
/// product of a write that just happened, not a fact about the machine. With
/// no key the plan is exactly today's, which keeps the captured fixtures.
pub fn plan(
    platform: Platform,
    event: &str,
    value: &str,
    stdin: &str,
    cfg: &NotifyConfig,
    env: &Env,
    key: Option<&Key>,
) -> Vec<Action> {
    if event.is_empty() || silenced_by_background_work(event, stdin) {
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
                actions.extend(unix_visual(platform, &msg, env, key));
            }
        }
    }
    actions
}

fn unix_sound(platform: Platform, event: &str, env: &Env) -> Option<Action> {
    let file = sound_file(platform, event)?;
    match platform {
        // The macOS script never checks that the file or `afplay` exists;
        // probing first would change the observable, so this tolerates the
        // spawn failing instead.
        Platform::Macos => Some(Action::Spawn {
            program: "afplay".to_string(),
            args: vec![format!("/System/Library/Sounds/{file}")],
            stdin: None,
            background: true,
            click: None,
        }),
        // Linux does check: the asset's package is often absent, and any of
        // three players might be installed.
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
                click: None,
            })
        }
        Platform::Windows => None,
    }
}

fn unix_visual(platform: Platform, msg: &str, env: &Env, key: Option<&Key>) -> Option<Action> {
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
            // The click actions, run by terminal-notifier on click in
            // this order: activate the app, then execute the focus command.
            // Only with a key, so the captured fixtures keep today's argv.
            if let Some(key) = key {
                if let Some(bundle) = env.activation_bundle() {
                    args.push("-activate".to_string());
                    args.push(bundle);
                }
                if let Some(command) = execute_command(&env.binary, key) {
                    args.push("-execute".to_string());
                    args.push(command);
                }
            }
            Some(Action::Spawn {
                program: "terminal-notifier".to_string(),
                args,
                stdin: None,
                background: false,
                click: None,
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
            // The click action, only with a key. The flag implies
            // `--wait`; the executor bounds it and hands the click back.
            let click = key.map(|key| {
                args.push("-A".to_string());
                args.push(LINUX_CLICK_ACTION.to_string());
                ClickWait {
                    key: key.as_string(),
                    deadline_secs: LINUX_CLICK_WAIT_SECS,
                }
            });
            Some(Action::Spawn {
                program: "notify-send".to_string(),
                args,
                stdin: None,
                background: false,
                click,
            })
        }
        Platform::Windows => None,
    }
}

/// The Windows toast, as a `powershell.exe` invocation.
///
/// Load-bearing: the interpreter is addressed by **absolute path** under
/// `%SystemRoot%`, never by `PATH` or the current directory, so a
/// `powershell.exe` dropped in the working directory must never be what raises
/// the notification; `-NoProfile`, so a profile cannot change what the script
/// means; and the message travels on **stdin as JSON**, never inside the
/// `-Command` body, so a hostile message is harmless by construction rather
/// than by escaping: the text is never parsed as code.
///
/// Unlike the shipped handler this does not first probe for BurntToast: the
/// script checks itself, saving a second interpreter launch.
///
/// The click key travels the same way, as `launch` in the stdin JSON, and only
/// when the handler is registered: an unregistered machine gets today's
/// toast and today's click behaviour. The launch attribute is XML on the
/// other side; the key alphabet's exclusion of `&<>"'` keeps it inert there.
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
        click: None,
    }
}

/// The helper executable's name and where it sits: beside the binary.
pub const FOCUS_HELPER: &str = "claude-statusline-focus.exe";

pub fn helper_beside(binary: &Path) -> PathBuf {
    // Cut at the last separator of either kind rather than `Path::parent`,
    // which on a Unix host treats a whole Windows path as one file name; the
    // case table asserts this from Linux.
    let text = binary.to_string_lossy();
    let dir = match text.rfind(['\\', '/']) {
        Some(i) => &text[..i],
        None => "",
    };
    join(Platform::Windows, Path::new(dir), &[FOCUS_HELPER])
}

/// The open command the registration writes: the helper quoted, then `"%1"`.
pub fn protocol_command(helper: &Path) -> String {
    format!("\"{}\" \"%1\"", helper.to_string_lossy())
}

/// Whether a registered open command names exactly the helper beside this
/// binary. Case folds because the registry and the installer may spell
/// the drive differently; no other difference is tolerated.
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
