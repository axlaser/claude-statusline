//! The focus record: the contract between the process that raises a toast and
//! the process that handles its click.
//!
//! A click arrives long after the toast was raised, in a process the OS
//! launched with none of the session's environment. Everything the click
//! handler needs to find the right terminal is therefore captured when the
//! alert fires and written to `statusline-focus-<session>.json` in the guarded
//! state directory, and only there. The toast carries a key,
//! `<session>.<token>`, that names the record and proves the click came from a
//! toast this user's session raised rather than from a web page invoking the
//! URI scheme with a guessed session id.
//!
//! The token is a cross-session nonce, not a secret: it is visible in the
//! session's own argv and to same-user processes, which are inside the boundary
//! already because they can edit `settings.json`. Its job is to stop web content
//! from raising the user's terminal by guessing. It never appears in a log
//! line, a toast, or rendered output.
//!
//! Everything here is cfg-free. The platform primitives it needs — random
//! bytes, the anchor process, the controlling tty, the Windows window — live in
//! `platform::focus` and are injected through [`Observation`] so the case table
//! can drive capture without a process tree.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::cmd::notify::Platform;
use crate::debug;
use crate::session::{sanitize_session_id, StateRoot};
use crate::state::{self, WriteOutcome};

/// The record format version. A record carrying any other version is refused,
/// and the next capture regenerates it.
pub const RECORD_VERSION: u64 = 1;

/// The token is 24 random bytes in the URL-safe alphabet: 32 characters.
pub const TOKEN_LEN: usize = 32;
pub const TOKEN_BYTES: usize = 24;

/// A record larger than this is refused before parsing.
pub const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// The URI scheme the Windows handler is registered under, and the prefix the
/// shell passes to the helper.
pub const SCHEME: &str = "claude-statusline";
pub const SCHEME_PREFIX: &str = "claude-statusline:";

/// The session part shares `sanitize_session_id`'s alphabet and length bound.
const MAX_SESSION: usize = 128;

/// The longest working directory a record may carry, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// The window classes a Windows record may name. Anything else is refused by
/// the loader and never stored by capture, so a handle is only ever raised
/// when it still belongs to a terminal host this tool knows.
pub const WINDOW_CLASSES: [&str; 7] = [
    // Windows Terminal, after the pseudo-console window is reparented.
    "CASCADIA_HOSTING_WINDOW_CLASS",
    // The classic console host.
    "ConsoleWindowClass",
    // VS Code and every other Electron host.
    "Chrome_WidgetWin_1",
    "Alacritty",
    "org.wezfurlong.wezterm",
    "mintty",
    // ConEmu and Cmder.
    "VirtualConsoleClass",
];

/// The window the pseudo-console creates for a ConPTY session. Never a target:
/// it is invisible under Windows Terminal and VS Code alike.
pub const PSEUDO_CONSOLE_CLASS: &str = "PseudoConsoleWindow";

/// The host kinds a Windows record may name.
pub const HOST_KINDS: [&str; 4] = ["windows-terminal", "console", "vscode", "other"];

fn is_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn all_key_bytes(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_key_byte)
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn within(s: &str, min: usize, max: usize, ok: impl Fn(u8) -> bool) -> bool {
    s.len() >= min && s.len() <= max && s.bytes().all(ok)
}

/// `^/dev/(ttys[0-9]{3}|pts/[0-9]+)$`.
pub fn is_tty(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("/dev/ttys") {
        return rest.len() == 3 && is_digits(rest);
    }
    if let Some(rest) = s.strip_prefix("/dev/pts/") {
        return is_digits(rest);
    }
    false
}

/// Absolute on either family, at most 4096 bytes, free of control bytes.
///
/// "Absolute" is spelled out rather than asked of `Path::is_absolute`, which
/// answers for the host: the case table loads macOS and Linux records on a
/// Windows machine, and a record never crosses machines in production.
pub fn is_abs_path(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_PATH_BYTES {
        return false;
    }
    if s.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return false;
    }
    let b = s.as_bytes();
    b[0] == b'/'
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
        || s.starts_with("\\\\")
}

fn is_bundle_id(s: &str) -> bool {
    within(s, 1, 255, |b| {
        b.is_ascii_alphanumeric() || b == b'.' || b == b'-'
    })
}

fn is_term_program(s: &str) -> bool {
    within(s, 1, 64, |b| {
        b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-'
    })
}

fn is_iterm_session(s: &str) -> bool {
    within(s, 1, 80, |b| {
        b.is_ascii_alphanumeric() || b == b':' || b == b'_' || b == b'-'
    })
}

fn is_display(s: &str) -> bool {
    within(s, 1, 64, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'/' | b'-')
    })
}

fn is_zellij_session(s: &str) -> bool {
    within(s, 1, 64, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
    })
}

fn is_session_type(s: &str) -> bool {
    s == "x11" || s == "wayland"
}

fn is_boot_id(s: &str) -> bool {
    within(s, 1, 64, |b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `^[$@%][0-9]+$` with the expected sigil: ids, never names.
fn is_tmux_id(s: &str, sigil: u8) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0] == sigil && is_digits(&s[1..])
}

/// `^[0-9]+\.[A-Za-z0-9_.-]+$`, the shape of `STY`.
fn is_screen_session(s: &str) -> bool {
    let Some((pid, name)) = s.split_once('.') else {
        return false;
    };
    is_digits(pid)
        && within(name, 1, 200, |b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
        })
}

fn is_kitty_socket(s: &str) -> bool {
    s.strip_prefix("unix:").is_some_and(is_abs_path)
}

fn is_konsole_service(s: &str) -> bool {
    s.strip_prefix("org.kde.konsole-").is_some_and(is_digits)
}

fn is_konsole_window(s: &str) -> bool {
    s.strip_prefix("/Windows/").is_some_and(is_digits)
}

fn is_window_class(s: &str) -> bool {
    WINDOW_CLASSES.contains(&s)
}

fn is_host_kind(s: &str) -> bool {
    HOST_KINDS.contains(&s)
}

/// The click key: `<session>.<token>`.
///
/// The only way to build a record path from a key is through this type, and
/// the only way to get one is [`Key::parse`], which applies R12: length first,
/// then the byte-exact grammar, with no trimming, decoding, case folding or
/// tolerance of extra separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    session: String,
    token: String,
}

impl Key {
    /// Parses the bare key.
    pub fn parse(arg: &str) -> Option<Key> {
        if arg.len() < 1 + 1 + TOKEN_LEN || arg.len() > MAX_SESSION + 1 + TOKEN_LEN {
            return None;
        }
        let (session, token) = arg.split_once('.')?;
        if session.is_empty() || session.len() > MAX_SESSION || !all_key_bytes(session) {
            return None;
        }
        if token.len() != TOKEN_LEN || !all_key_bytes(token) {
            return None;
        }
        Some(Key {
            session: session.to_string(),
            token: token.to_string(),
        })
    }

    /// Parses a click handler's argument: the bare key, or the key behind the
    /// exact lowercase scheme prefix the Windows shell passes.
    ///
    /// The prefix is matched byte for byte. `CLAUDE-STATUSLINE:`, `//` after
    /// the colon, a percent-encoded key and every other variation fail here.
    pub fn parse_argument(arg: &str) -> Option<Key> {
        if arg.len() > SCHEME_PREFIX.len() + MAX_SESSION + 1 + TOKEN_LEN {
            return None;
        }
        match arg.strip_prefix(SCHEME_PREFIX) {
            Some(rest) => Key::parse(rest),
            None => Key::parse(arg),
        }
    }

    /// Builds a key from parts that already passed their grammars.
    fn from_parts(session: &str, token: &str) -> Option<Key> {
        Key::parse(&format!("{session}.{token}"))
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// `<session>.<token>`, the form the toast carries.
    pub fn as_string(&self) -> String {
        format!("{}.{}", self.session, self.token)
    }

    /// `claude-statusline:<session>.<token>`, the Windows launch URI.
    pub fn uri(&self) -> String {
        format!("{SCHEME_PREFIX}{}", self.as_string())
    }

    /// The record this key names, built from the session part alone.
    pub fn record_path(&self, root: &Path) -> PathBuf {
        record_path(root, &self.session)
    }
}

/// Where a session's record lives. `safe_session` has already been through
/// `sanitize_session_id`.
pub fn record_path(root: &Path, safe_session: &str) -> PathBuf {
    root.join(format!("statusline-focus-{safe_session}.json"))
}

/// Encodes 24 random bytes as 32 URL-safe base64 characters, no padding.
pub fn encode_token(bytes: &[u8; TOKEN_BYTES]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(TOKEN_LEN);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
        }
    }
    out
}

/// Constant-time equality over two strings of equal length. Hygiene, not a
/// control: the token is a nonce, and this only keeps the comparison from
/// being the one place its bytes influence timing.
fn tokens_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The process the click path checks for liveness: the nearest ancestor of
/// the capturing process that is not a shell, which is Claude Code itself.
///
/// The start time is what makes a pid meaningful after the process exits and
/// the number is reused; on Linux start times count from boot, so the boot id
/// travels with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Anchor {
    pub pid: u64,
    pub start: u64,
    pub boot_id: Option<String>,
}

/// What one process in the ancestor chain looks like to the platform layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u64,
    pub ppid: u64,
    /// The image name, without a directory.
    pub name: String,
    pub start: u64,
}

/// Shells that sit between Claude Code and the process it spawned, and
/// between the terminal and Claude Code. Compared against a lowercased image
/// name with any `.exe` removed.
const SHELLS: [&str; 13] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "tcsh",
    "csh",
    "nu",
    "pwsh",
    "powershell",
    "cmd",
    "busybox",
];

pub fn is_shell(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    let stem = lowered.strip_suffix(".exe").unwrap_or(&lowered);
    let stem = stem.trim_start_matches('-');
    SHELLS.contains(&stem)
}

/// Picks the anchor and, above it, the terminal process from an ancestor
/// chain (self first).
///
/// `CLAUDE_PID`, when Claude Code exports it and it appears in the chain,
/// names the anchor outright; otherwise the anchor is the first ancestor that
/// is not a shell. The terminal process is the first non-shell above the
/// anchor: the emulator that forked the user's shell, or nothing recognisable.
pub fn select_anchor(
    chain: &[ProcessInfo],
    claude_pid: Option<u64>,
) -> (Option<ProcessInfo>, Option<ProcessInfo>) {
    let anchor_index = match claude_pid {
        Some(pid) if chain.iter().skip(1).any(|p| p.pid == pid) => {
            chain.iter().position(|p| p.pid == pid)
        }
        _ => chain
            .iter()
            .skip(1)
            .position(|p| !is_shell(&p.name))
            .map(|i| i + 1),
    };
    let Some(anchor_index) = anchor_index else {
        return (None, None);
    };
    let terminal = chain
        .iter()
        .skip(anchor_index + 1)
        .find(|p| !is_shell(&p.name))
        .cloned();
    (chain.get(anchor_index).cloned(), terminal)
}

/// Konsole's D-Bus identity, as its environment exports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Konsole {
    pub service: String,
    pub window: String,
    pub session: u64,
}

/// A tmux pane, addressed by ids only. Names are user text and never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tmux {
    pub socket: String,
    pub pane: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub session: String,
    pub window: u64,
}

/// The Windows window a session was captured in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowIdentity {
    pub handle: u64,
    pub owner_pid: u64,
    pub owner_start: u64,
    pub class: String,
    pub host: String,
}

/// Everything the click handler may use to find the terminal, each field
/// optional and each validated against its grammar both when captured and when
/// loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub bundle_id: Option<String>,
    pub term_program: Option<String>,
    pub tty: Option<String>,
    pub iterm_session: Option<String>,
    pub ghostty_cwd: Option<String>,
    pub window_id: Option<u64>,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
    pub session_type: Option<String>,
    pub konsole: Option<Konsole>,
    pub terminal_pid: Option<u64>,
    pub terminal_start: Option<u64>,
    pub kitty_window_id: Option<u64>,
    pub kitty_socket: Option<String>,
    pub wezterm_pane: Option<u64>,
    pub tmux: Option<Tmux>,
    pub screen: Option<Screen>,
    pub zellij_pane: Option<u64>,
    /// zellij needs its session named to address a pane from outside; the
    /// name is user text, so it is held to a narrow alphabet.
    pub zellij_session: Option<String>,
    pub vscode_pid: Option<u64>,
    pub window: Option<WindowIdentity>,
}

impl Identity {
    /// True when the record was captured inside a multiplexer, which is the
    /// case where the tty and window id the process sees belong to the
    /// multiplexer rather than the terminal.
    pub fn multiplexed(&self) -> bool {
        self.tmux.is_some() || self.screen.is_some() || self.zellij_pane.is_some()
    }
}

/// What the platform layer observed at capture time. Injected so the case
/// table can drive capture with a chosen anchor, tty, window and randomness.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub anchor: Option<Anchor>,
    /// The terminal emulator above the anchor, Linux only: pid and start.
    pub terminal: Option<(u64, u64)>,
    /// The controlling terminal's device, macOS only.
    pub tty: Option<String>,
    /// The capturing process's working directory.
    pub cwd: String,
    /// The captured window, Windows only.
    pub window: Option<WindowIdentity>,
    /// Randomness for a new token, or `None` when the OS refused.
    pub random: Option<[u8; TOKEN_BYTES]>,
}

/// Parses a decimal environment value, refusing signs, spaces and anything
/// else `str::parse` would tolerate.
fn env_u64(v: &str) -> Option<u64> {
    if is_digits(v) {
        v.parse().ok()
    } else {
        None
    }
}

/// Builds the identity from the environment and the observation, dropping any
/// field that fails its grammar and naming it, so an unusual terminal loses one
/// step rather than the whole record.
pub fn identity_from_env(
    platform: Platform,
    var: &dyn Fn(&str) -> Option<String>,
    obs: &Observation,
) -> (Identity, Vec<&'static str>) {
    let mut id = Identity::default();
    let mut omitted: Vec<&'static str> = Vec::new();

    fn take_str(
        omitted: &mut Vec<&'static str>,
        name: &'static str,
        value: Option<String>,
        ok: fn(&str) -> bool,
    ) -> Option<String> {
        match value {
            Some(v) if ok(&v) => Some(v),
            Some(_) => {
                omitted.push(name);
                None
            }
            None => None,
        }
    }

    fn take_u64(
        omitted: &mut Vec<&'static str>,
        name: &'static str,
        value: Option<String>,
    ) -> Option<u64> {
        match value {
            Some(v) => match env_u64(&v) {
                Some(n) => Some(n),
                None => {
                    omitted.push(name);
                    None
                }
            },
            None => None,
        }
    }

    // The multiplexer identity first: when one is present, the tty, window id
    // and terminal process the capturing process sees are the multiplexer's,
    // and storing them would select the wrong tab on click.
    if let Some(tmux) = var("TMUX") {
        let socket = tmux.split(',').next().unwrap_or("").to_string();
        let pane = var("TMUX_PANE").unwrap_or_default();
        if is_abs_path(&socket) && is_tmux_id(&pane, b'%') {
            id.tmux = Some(Tmux { socket, pane });
        } else {
            omitted.push("tmux");
        }
    }
    if let Some(sty) = var("STY") {
        let window = var("WINDOW").and_then(|w| env_u64(&w));
        match window {
            Some(w) if is_screen_session(&sty) => {
                id.screen = Some(Screen {
                    session: sty,
                    window: w,
                })
            }
            _ => omitted.push("screen"),
        }
    }
    id.zellij_pane = take_u64(&mut omitted, "zellij_pane", var("ZELLIJ_PANE_ID"));
    if id.zellij_pane.is_some() {
        id.zellij_session = take_str(
            &mut omitted,
            "zellij_session",
            var("ZELLIJ_SESSION_NAME"),
            is_zellij_session,
        );
    }

    id.bundle_id = take_str(
        &mut omitted,
        "bundle_id",
        var("__CFBundleIdentifier"),
        is_bundle_id,
    );
    id.term_program = take_str(
        &mut omitted,
        "term_program",
        var("TERM_PROGRAM"),
        is_term_program,
    );
    id.vscode_pid = take_u64(&mut omitted, "vscode_pid", var("VSCODE_PID"));

    if !id.multiplexed() {
        if platform == Platform::Macos {
            id.tty = take_str(&mut omitted, "tty", obs.tty.clone(), is_tty);
            id.iterm_session = take_str(
                &mut omitted,
                "iterm_session",
                var("ITERM_SESSION_ID"),
                is_iterm_session,
            );
            if id.term_program.as_deref() == Some("ghostty") {
                id.ghostty_cwd = take_str(
                    &mut omitted,
                    "ghostty_cwd",
                    Some(obs.cwd.clone()),
                    is_abs_path,
                );
            }
        }
        if platform == Platform::Linux {
            id.window_id = take_u64(&mut omitted, "window_id", var("WINDOWID"));
            id.display = take_str(&mut omitted, "display", var("DISPLAY"), is_display);
            id.wayland_display = take_str(
                &mut omitted,
                "wayland_display",
                var("WAYLAND_DISPLAY"),
                is_display,
            );
            id.session_type = take_str(
                &mut omitted,
                "session_type",
                var("XDG_SESSION_TYPE"),
                is_session_type,
            );
            if let Some(service) = var("KONSOLE_DBUS_SERVICE") {
                let window = var("KONSOLE_DBUS_WINDOW").unwrap_or_default();
                let session = var("KONSOLE_DBUS_SESSION")
                    .and_then(|s| s.strip_prefix("/Sessions/").and_then(env_u64));
                match session {
                    Some(s) if is_konsole_service(&service) && is_konsole_window(&window) => {
                        id.konsole = Some(Konsole {
                            service,
                            window,
                            session: s,
                        })
                    }
                    _ => omitted.push("konsole"),
                }
            }
            if let Some((pid, start)) = obs.terminal {
                id.terminal_pid = Some(pid);
                id.terminal_start = Some(start);
            }
        }
        id.kitty_window_id = take_u64(&mut omitted, "kitty_window_id", var("KITTY_WINDOW_ID"));
        id.kitty_socket = take_str(
            &mut omitted,
            "kitty_socket",
            var("KITTY_LISTEN_ON"),
            is_kitty_socket,
        );
        id.wezterm_pane = take_u64(&mut omitted, "wezterm_pane", var("WEZTERM_PANE"));
    }

    if platform == Platform::Windows {
        match &obs.window {
            Some(w) if is_window_class(&w.class) && is_host_kind(&w.host) => {
                id.window = Some(w.clone())
            }
            Some(_) => omitted.push("window"),
            None => {}
        }
    }

    (id, omitted)
}

/// One session's focus record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub session: String,
    pub token: String,
    pub captured_at: u64,
    pub anchor: Anchor,
    pub cwd: String,
    pub debug: bool,
    pub identity: Identity,
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        map.insert(key.to_string(), v);
    }
}

fn num(v: u64) -> Value {
    Value::from(v)
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

impl Record {
    /// Serialises with a pinned field order, version first.
    pub fn to_json(&self) -> String {
        let mut root = Map::new();
        root.insert("version".into(), num(RECORD_VERSION));
        root.insert("session".into(), text(&self.session));
        root.insert("token".into(), text(&self.token));
        root.insert("captured_at".into(), num(self.captured_at));
        let mut anchor = Map::new();
        anchor.insert("pid".into(), num(self.anchor.pid));
        anchor.insert("start".into(), num(self.anchor.start));
        put(
            &mut anchor,
            "boot_id",
            self.anchor.boot_id.as_deref().map(text),
        );
        root.insert("anchor".into(), Value::Object(anchor));
        root.insert("cwd".into(), text(&self.cwd));
        root.insert("debug".into(), Value::Bool(self.debug));

        let id = &self.identity;
        let mut identity = Map::new();
        put(
            &mut identity,
            "bundle_id",
            id.bundle_id.as_deref().map(text),
        );
        put(
            &mut identity,
            "term_program",
            id.term_program.as_deref().map(text),
        );
        put(&mut identity, "tty", id.tty.as_deref().map(text));
        put(
            &mut identity,
            "iterm_session",
            id.iterm_session.as_deref().map(text),
        );
        put(
            &mut identity,
            "ghostty_cwd",
            id.ghostty_cwd.as_deref().map(text),
        );
        put(&mut identity, "window_id", id.window_id.map(num));
        put(&mut identity, "display", id.display.as_deref().map(text));
        put(
            &mut identity,
            "wayland_display",
            id.wayland_display.as_deref().map(text),
        );
        put(
            &mut identity,
            "session_type",
            id.session_type.as_deref().map(text),
        );
        if let Some(k) = &id.konsole {
            let mut m = Map::new();
            m.insert("service".into(), text(&k.service));
            m.insert("window".into(), text(&k.window));
            m.insert("session".into(), num(k.session));
            identity.insert("konsole".into(), Value::Object(m));
        }
        put(&mut identity, "terminal_pid", id.terminal_pid.map(num));
        put(&mut identity, "terminal_start", id.terminal_start.map(num));
        put(
            &mut identity,
            "kitty_window_id",
            id.kitty_window_id.map(num),
        );
        put(
            &mut identity,
            "kitty_socket",
            id.kitty_socket.as_deref().map(text),
        );
        put(&mut identity, "wezterm_pane", id.wezterm_pane.map(num));
        if let Some(t) = &id.tmux {
            let mut m = Map::new();
            m.insert("socket".into(), text(&t.socket));
            m.insert("pane".into(), text(&t.pane));
            identity.insert("tmux".into(), Value::Object(m));
        }
        if let Some(s) = &id.screen {
            let mut m = Map::new();
            m.insert("session".into(), text(&s.session));
            m.insert("window".into(), num(s.window));
            identity.insert("screen".into(), Value::Object(m));
        }
        put(&mut identity, "zellij_pane", id.zellij_pane.map(num));
        put(
            &mut identity,
            "zellij_session",
            id.zellij_session.as_deref().map(text),
        );
        put(&mut identity, "vscode_pid", id.vscode_pid.map(num));
        if let Some(w) = &id.window {
            let mut m = Map::new();
            m.insert("handle".into(), num(w.handle));
            m.insert("owner_pid".into(), num(w.owner_pid));
            m.insert("owner_start".into(), num(w.owner_start));
            m.insert("class".into(), text(&w.class));
            m.insert("host".into(), text(&w.host));
            identity.insert("window".into(), Value::Object(m));
        }
        root.insert("identity".into(), Value::Object(identity));
        Value::Object(root).to_string()
    }

    /// The whole loader gate: size, version, session, token, then every field
    /// against its grammar. One failure and there is no record.
    ///
    /// `expected_token` is compared in constant time when given; capture passes
    /// `None` because it is reading the record to reuse whatever token is
    /// there.
    pub fn load(bytes: &[u8], session: &str, expected_token: Option<&str>) -> Option<Record> {
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return None;
        }
        let root: Value = serde_json::from_slice(bytes).ok()?;
        let root = root.as_object()?;
        if root.get("version")?.as_u64()? != RECORD_VERSION {
            return None;
        }
        let stored_session = root.get("session")?.as_str()?;
        if stored_session != session || !all_key_bytes(session) || session.len() > MAX_SESSION {
            return None;
        }
        let token = root.get("token")?.as_str()?;
        if token.len() != TOKEN_LEN || !all_key_bytes(token) {
            return None;
        }
        if let Some(expected) = expected_token {
            if !tokens_equal(token, expected) {
                return None;
            }
        }
        let captured_at = root.get("captured_at")?.as_u64()?;
        let anchor = root.get("anchor")?.as_object()?;
        let anchor = Anchor {
            pid: anchor.get("pid")?.as_u64()?,
            start: anchor.get("start")?.as_u64()?,
            boot_id: match anchor.get("boot_id") {
                None => None,
                Some(v) => Some(v.as_str().filter(|s| is_boot_id(s))?.to_string()),
            },
        };
        let cwd = root.get("cwd")?.as_str()?;
        if !is_abs_path(cwd) {
            return None;
        }
        let debug = root.get("debug")?.as_bool()?;
        let identity = load_identity(root.get("identity")?.as_object()?)?;
        Some(Record {
            session: stored_session.to_string(),
            token: token.to_string(),
            captured_at,
            anchor,
            cwd: cwd.to_string(),
            debug,
            identity,
        })
    }

    /// The key this record answers to.
    pub fn key(&self) -> Option<Key> {
        Key::from_parts(&self.session, &self.token)
    }
}

/// `Some(None)` for an absent key, `Some(Some(v))` for a valid one, `None` for
/// a present-but-invalid one.
fn opt_str(
    map: &Map<String, Value>,
    key: &str,
    ok: impl Fn(&str) -> bool,
) -> Option<Option<String>> {
    match map.get(key) {
        None => Some(None),
        Some(v) => {
            let s = v.as_str()?;
            if ok(s) {
                Some(Some(s.to_string()))
            } else {
                None
            }
        }
    }
}

fn opt_u64(map: &Map<String, Value>, key: &str) -> Option<Option<u64>> {
    match map.get(key) {
        None => Some(None),
        Some(v) => Some(Some(v.as_u64()?)),
    }
}

fn load_identity(map: &Map<String, Value>) -> Option<Identity> {
    let mut id = Identity {
        bundle_id: opt_str(map, "bundle_id", is_bundle_id)?,
        term_program: opt_str(map, "term_program", is_term_program)?,
        tty: opt_str(map, "tty", is_tty)?,
        iterm_session: opt_str(map, "iterm_session", is_iterm_session)?,
        ghostty_cwd: opt_str(map, "ghostty_cwd", is_abs_path)?,
        window_id: opt_u64(map, "window_id")?,
        display: opt_str(map, "display", is_display)?,
        wayland_display: opt_str(map, "wayland_display", is_display)?,
        session_type: opt_str(map, "session_type", is_session_type)?,
        konsole: None,
        terminal_pid: opt_u64(map, "terminal_pid")?,
        terminal_start: opt_u64(map, "terminal_start")?,
        kitty_window_id: opt_u64(map, "kitty_window_id")?,
        kitty_socket: opt_str(map, "kitty_socket", is_kitty_socket)?,
        wezterm_pane: opt_u64(map, "wezterm_pane")?,
        tmux: None,
        screen: None,
        zellij_pane: opt_u64(map, "zellij_pane")?,
        zellij_session: opt_str(map, "zellij_session", is_zellij_session)?,
        vscode_pid: opt_u64(map, "vscode_pid")?,
        window: None,
    };
    if let Some(k) = map.get("konsole") {
        let k = k.as_object()?;
        let service = k.get("service")?.as_str()?;
        let window = k.get("window")?.as_str()?;
        if !is_konsole_service(service) || !is_konsole_window(window) {
            return None;
        }
        id.konsole = Some(Konsole {
            service: service.to_string(),
            window: window.to_string(),
            session: k.get("session")?.as_u64()?,
        });
    }
    if let Some(t) = map.get("tmux") {
        let t = t.as_object()?;
        let socket = t.get("socket")?.as_str()?;
        let pane = t.get("pane")?.as_str()?;
        if !is_abs_path(socket) || !is_tmux_id(pane, b'%') {
            return None;
        }
        id.tmux = Some(Tmux {
            socket: socket.to_string(),
            pane: pane.to_string(),
        });
    }
    if let Some(s) = map.get("screen") {
        let s = s.as_object()?;
        let session = s.get("session")?.as_str()?;
        if !is_screen_session(session) {
            return None;
        }
        id.screen = Some(Screen {
            session: session.to_string(),
            window: s.get("window")?.as_u64()?,
        });
    }
    if let Some(w) = map.get("window") {
        let w = w.as_object()?;
        let class = w.get("class")?.as_str()?;
        let host = w.get("host")?.as_str()?;
        if !is_window_class(class) || !is_host_kind(host) {
            return None;
        }
        id.window = Some(WindowIdentity {
            handle: w.get("handle")?.as_u64()?,
            owner_pid: w.get("owner_pid")?.as_u64()?,
            owner_start: w.get("owner_start")?.as_u64()?,
            class: class.to_string(),
            host: host.to_string(),
        });
    }
    Some(id)
}

/// Why a capture produced no key, or how it produced one.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The record is on disk with a fresh token.
    Written,
    /// The record is on disk and the existing token was kept.
    Reused,
    /// The session id sanitised to nothing.
    NoSession,
    /// The state root is the flat fallback: capture is skipped there (R19).
    Unguarded,
    /// The OS produced no randomness, so there is no token (KTD13).
    NoRandomness,
    /// The guarded write did not land.
    NotWritten(WriteOutcome),
}

#[derive(Debug)]
pub struct Capture {
    pub key: Option<Key>,
    pub outcome: CaptureOutcome,
    /// Identity fields dropped for failing their grammar, by name.
    pub omitted: Vec<&'static str>,
}

/// The pure core of capture: everything about the machine arrives through
/// `var` and `obs`, so the case table can pin it.
pub fn capture_with(
    root: &StateRoot,
    session_id: &str,
    debug: bool,
    platform: Platform,
    var: &dyn Fn(&str) -> Option<String>,
    obs: &Observation,
    now: u64,
) -> Capture {
    let safe = sanitize_session_id(session_id);
    if safe.is_empty() {
        return Capture {
            key: None,
            outcome: CaptureOutcome::NoSession,
            omitted: Vec::new(),
        };
    }
    // R19: the flat temp root is shared and unverified. A record there could
    // be planted by another local user, so none is written and the toast goes
    // out without click handling.
    if !root.is_guarded() {
        return Capture {
            key: None,
            outcome: CaptureOutcome::Unguarded,
            omitted: Vec::new(),
        };
    }
    let path = record_path(root, &safe);

    // Reuse the token while an existing record reads as trusted and passes
    // the grammar, so every toast a session raises keeps working. A trusted
    // record with a bad token or an unknown version is regenerated.
    let existing = state::read_trusted(&path).and_then(|b| Record::load(&b, &safe, None));
    let (token, reused) = match existing {
        Some(r) => (r.token, true),
        None => match obs.random {
            Some(bytes) => (encode_token(&bytes), false),
            None => {
                return Capture {
                    key: None,
                    outcome: CaptureOutcome::NoRandomness,
                    omitted: Vec::new(),
                }
            }
        },
    };

    let (identity, omitted) = identity_from_env(platform, var, obs);
    let record = Record {
        session: safe.clone(),
        token,
        captured_at: now,
        anchor: obs.anchor.clone().unwrap_or_default(),
        cwd: if is_abs_path(&obs.cwd) {
            obs.cwd.clone()
        } else {
            "/".to_string()
        },
        debug,
        identity,
    };

    match state::write_guarded_under(root, &path, record.to_json().as_bytes()) {
        WriteOutcome::Written => Capture {
            key: record.key(),
            outcome: if reused {
                CaptureOutcome::Reused
            } else {
                CaptureOutcome::Written
            },
            omitted,
        },
        other => Capture {
            key: None,
            outcome: CaptureOutcome::NotWritten(other),
            omitted,
        },
    }
}

/// Production capture: observes the machine, then runs the pure core, and
/// logs the outcome without ever logging the token.
pub fn capture(root: &StateRoot, session_id: &str, debug: bool) -> Option<Key> {
    let obs = crate::platform::focus::observe();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let capture = capture_with(
        root,
        session_id,
        debug,
        Platform::current(),
        &var,
        &obs,
        now,
    );
    if crate::debug::is_enabled() {
        let safe = sanitize_session_id(session_id);
        let outcome = format!("{:?}", capture.outcome);
        let omitted = capture.omitted.join(",");
        debug::log(move || {
            if omitted.is_empty() {
                format!("focus: capture for {safe}: {outcome}")
            } else {
                format!("focus: capture for {safe}: {outcome}, omitted [{omitted}]")
            }
        });
    }
    capture.key
}

/// The session id a hook payload names, sanitised. `None` when the payload is
/// not an object or the id sanitises to nothing.
pub fn session_from_payload(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let raw = value.get("session_id")?.as_str()?;
    let safe = sanitize_session_id(raw);
    (!safe.is_empty()).then_some(safe)
}
