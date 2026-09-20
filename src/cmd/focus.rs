//! `focus` — the click handler: resolve a key, plan the steps, run them.
//!
//! Usage: `claude-statusline focus <session>.<token>`, or the same key behind
//! the `claude-statusline:` scheme prefix, which is how the Windows shell
//! passes it to the helper executable. Three callers share [`run`]: the
//! subcommand (macOS, through terminal-notifier's `-execute`), the `notify`
//! arm on Linux once notify-send reports the click, and the helper binary on
//! Windows.
//!
//! Like `notify`, the unit is a pure [`plan`] over data — a [`Record`] and a
//! [`Probes`] — and an impure executor in `platform::focus`. Every step
//! carries typed arguments and the absolute path of the tool that runs it,
//! never a command string, so the case table can assert exactly what a record
//! turns into without a terminal in sight (KTD8, KTD10).
//!
//! The order of gates is the whole security story (R12): argument count, then
//! UTF-8, then the byte-exact key grammar, then the trusted read of a path
//! built from the session part alone, then the record's own grammar. Nothing
//! is spawned before all of them pass, and nothing derived from the argument
//! is ever spawned.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::cmd::notify::Platform;
use crate::focus::{Key, Record, MAX_RECORD_BYTES};
use crate::{debug, platform, session, state};

/// The external tools a step may run, each resolved to an absolute path by
/// the probe or not planned at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    Osascript,
    Kitten,
    Wezterm,
    Tmux,
    Screen,
    Zellij,
    Xdotool,
    Wmctrl,
    Kdotool,
    Qdbus6,
    QdbusQt6,
    QdbusQt5,
    Qdbus,
    Busctl,
}

impl Tool {
    pub const ALL: [Tool; 14] = [
        Tool::Osascript,
        Tool::Kitten,
        Tool::Wezterm,
        Tool::Tmux,
        Tool::Screen,
        Tool::Zellij,
        Tool::Xdotool,
        Tool::Wmctrl,
        Tool::Kdotool,
        Tool::Qdbus6,
        Tool::QdbusQt6,
        Tool::QdbusQt5,
        Tool::Qdbus,
        Tool::Busctl,
    ];

    /// The program name, as it sits on disk.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Osascript => "osascript",
            Tool::Kitten => "kitten",
            Tool::Wezterm => "wezterm",
            Tool::Tmux => "tmux",
            Tool::Screen => "screen",
            Tool::Zellij => "zellij",
            Tool::Xdotool => "xdotool",
            Tool::Wmctrl => "wmctrl",
            Tool::Kdotool => "kdotool",
            Tool::Qdbus6 => "qdbus6",
            Tool::QdbusQt6 => "qdbus-qt6",
            Tool::QdbusQt5 => "qdbus-qt5",
            Tool::Qdbus => "qdbus",
            Tool::Busctl => "busctl",
        }
    }
}

/// The Konsole D-Bus callers, in preference order. Arch and Fedora do not
/// ship the bare `qdbus` name, and `busctl` is everywhere systemd is.
pub const KONSOLE_CALLERS: [Tool; 5] = [
    Tool::Qdbus6,
    Tool::QdbusQt6,
    Tool::QdbusQt5,
    Tool::Qdbus,
    Tool::Busctl,
];

/// Which macOS terminal a tab-selection script addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabFamily {
    Terminal,
    ITerm2,
    Ghostty,
}

impl TabFamily {
    /// The bundle identifiers each family answers to.
    pub fn from_bundle_id(bundle_id: &str) -> Option<TabFamily> {
        match bundle_id {
            "com.apple.Terminal" => Some(TabFamily::Terminal),
            "com.googlecode.iterm2" => Some(TabFamily::ITerm2),
            "com.mitchellh.ghostty" => Some(TabFamily::Ghostty),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X11Tool {
    Xdotool,
    Wmctrl,
}

/// The two argument shapes a Konsole call takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbusStyle {
    /// `qdbus <service> <path> <interface.method> <args>`; every `qdbus*`.
    Qdbus,
    /// `busctl --user call <service> <path> <interface> <method> <sig> <args>`.
    Busctl,
}

/// One thing the click handler does. Each variant carries typed arguments
/// and the tool's absolute path; verbs and flags are constants in the
/// executor (KTD10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Windows: raise the recorded window through the foreground recipe, with
    /// the taskbar flash as its own fallback.
    RaiseWindow {
        handle: u64,
    },
    /// Linux X11: activate a window id.
    ActivateX11 {
        tool: PathBuf,
        via: X11Tool,
        window_id: u64,
    },
    /// KDE on Wayland: activate a kdotool window id.
    ActivateKde {
        tool: PathBuf,
        window_id: String,
    },
    /// Terminal.app or iTerm2: select the tab whose tty matches.
    SelectTab {
        tool: PathBuf,
        family: TabFamily,
        tty: String,
    },
    /// Ghostty: focus the terminal whose tty matches, else the one terminal
    /// whose working directory matches. Either may be empty.
    SelectGhostty {
        tool: PathBuf,
        tty: String,
        cwd: String,
    },
    KittenFocus {
        tool: PathBuf,
        socket: String,
        window_id: u64,
    },
    WeztermActivate {
        tool: PathBuf,
        pane: u64,
    },
    KonsoleSetSession {
        tool: PathBuf,
        style: DbusStyle,
        service: String,
        window: String,
        session: u64,
    },
    TmuxSwitchClient {
        tool: PathBuf,
        socket: String,
        client_tty: String,
        pane: String,
    },
    TmuxSelectWindow {
        tool: PathBuf,
        socket: String,
        pane: String,
    },
    TmuxSelectPane {
        tool: PathBuf,
        socket: String,
        pane: String,
    },
    ScreenSelect {
        tool: PathBuf,
        session: String,
        window: u64,
    },
    ZellijFocus {
        tool: PathBuf,
        session: Option<String>,
        pane: u64,
    },
}

impl Step {
    /// A short name for the log: no arguments, so no record field and never
    /// the token.
    pub fn name(&self) -> &'static str {
        match self {
            Step::RaiseWindow { .. } => "raise-window",
            Step::ActivateX11 { .. } => "activate-x11",
            Step::ActivateKde { .. } => "activate-kde",
            Step::SelectTab { .. } => "select-tab",
            Step::SelectGhostty { .. } => "select-ghostty",
            Step::KittenFocus { .. } => "kitten-focus",
            Step::WeztermActivate { .. } => "wezterm-activate",
            Step::KonsoleSetSession { .. } => "konsole-set-session",
            Step::TmuxSwitchClient { .. } => "tmux-switch-client",
            Step::TmuxSelectWindow { .. } => "tmux-select-window",
            Step::TmuxSelectPane { .. } => "tmux-select-pane",
            Step::ScreenSelect { .. } => "screen-select",
            Step::ZellijFocus { .. } => "zellij-focus",
        }
    }
}

/// An attached tmux client: the terminal it sits in and the process behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxClient {
    pub tty: String,
    pub pid: u64,
}

/// Everything about the machine at click time that changes what is planned.
///
/// Injected like `notify::Env`: a case pins it, and the production probe in
/// `platform::focus` fills it from the real machine.
#[derive(Debug, Clone, Default)]
pub struct Probes {
    /// The tools that resolved, each to the absolute path it will be run by.
    pub tools: BTreeMap<Tool, PathBuf>,
    /// The anchor process is alive with the recorded start time (and boot id).
    pub anchor_alive: bool,
    /// The recorded terminal process is alive with its recorded start time.
    pub terminal_alive: bool,
    /// Windows: the recorded window still verifies in every particular.
    pub window_verifies: bool,
    /// The kitty remote-control socket the record names exists.
    pub kitty_socket_present: bool,
    /// The click-time desktop is KDE, which is where kdotool works.
    pub kde: bool,
    /// The clients attached to the recorded tmux session.
    pub tmux_clients: Vec<TmuxClient>,
    /// Visible top-level X11 windows found for each searched pid, or its
    /// nearest window-owning ancestor.
    pub x11_windows: BTreeMap<u64, Vec<u64>>,
    /// The same search through kdotool, whose ids are strings.
    pub kde_windows: BTreeMap<u64, Vec<String>>,
}

impl Probes {
    fn tool(&self, tool: Tool) -> Option<PathBuf> {
        self.tools.get(&tool).cloned()
    }

    fn first_tool(&self, order: &[Tool]) -> Option<(Tool, PathBuf)> {
        order
            .iter()
            .find_map(|t| self.tools.get(t).map(|p| (*t, p.clone())))
    }
}

/// The steps to run, in order, and what was decided against, for the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Why a step was not planned. Carries identity names and counts, never
    /// the token.
    pub notes: Vec<String>,
}

/// Turns a record and the probes into an ordered step list, without doing
/// any of it.
///
/// The liveness rule (KTD9) sits here so it is testable: an anchor that is
/// alive with the same start time gets the full list; one that is gone gets
/// the window raise alone, and only while the window still verifies; on macOS
/// the app activation is terminal-notifier's own handling of the click and
/// needs no step. A non-unique identifier — a pid search, a Ghostty working
/// directory — selects only when exactly one candidate matches.
pub fn plan(platform: Platform, record: &Record, probes: &Probes) -> Plan {
    let id = &record.identity;
    let mut plan = Plan::default();

    if !probes.anchor_alive {
        plan.notes
            .push("anchor process is gone: raise only, no selection".to_string());
        raise_windows_window(id, probes, &mut plan);
        return plan;
    }

    match platform {
        Platform::Windows => raise_windows_window(id, probes, &mut plan),
        Platform::Linux => raise_linux(id, probes, &mut plan),
        // Activation is the transport's job on macOS (KTD2).
        Platform::Macos => {}
    }

    if let Some(t) = &id.tmux {
        match probes.tool(Tool::Tmux) {
            Some(tool) => {
                if probes.tmux_clients.is_empty() {
                    plan.notes.push("tmux: no attached client".to_string());
                }
                for client in &probes.tmux_clients {
                    plan.steps.push(Step::TmuxSwitchClient {
                        tool: tool.clone(),
                        socket: t.socket.clone(),
                        client_tty: client.tty.clone(),
                        pane: t.pane.clone(),
                    });
                }
                plan.steps.push(Step::TmuxSelectWindow {
                    tool: tool.clone(),
                    socket: t.socket.clone(),
                    pane: t.pane.clone(),
                });
                plan.steps.push(Step::TmuxSelectPane {
                    tool,
                    socket: t.socket.clone(),
                    pane: t.pane.clone(),
                });
            }
            None => plan.notes.push("tmux: tool not found".to_string()),
        }
    }
    if let Some(s) = &id.screen {
        match probes.tool(Tool::Screen) {
            Some(tool) => plan.steps.push(Step::ScreenSelect {
                tool,
                session: s.session.clone(),
                window: s.window,
            }),
            None => plan.notes.push("screen: tool not found".to_string()),
        }
    }
    if let Some(pane) = id.zellij_pane {
        match probes.tool(Tool::Zellij) {
            Some(tool) => plan.steps.push(Step::ZellijFocus {
                tool,
                session: id.zellij_session.clone(),
                pane,
            }),
            None => plan.notes.push("zellij: tool not found".to_string()),
        }
    }

    if platform == Platform::Macos {
        select_macos_tab(record, probes, &mut plan);
    }

    if let (Some(window_id), Some(socket)) = (id.kitty_window_id, &id.kitty_socket) {
        if !probes.kitty_socket_present {
            plan.notes
                .push("kitty: remote control socket absent".to_string());
        } else {
            match probes.tool(Tool::Kitten) {
                Some(tool) => plan.steps.push(Step::KittenFocus {
                    tool,
                    socket: socket.clone(),
                    window_id,
                }),
                None => plan.notes.push("kitty: kitten not found".to_string()),
            }
        }
    }
    if let Some(pane) = id.wezterm_pane {
        match probes.tool(Tool::Wezterm) {
            Some(tool) => plan.steps.push(Step::WeztermActivate { tool, pane }),
            None => plan.notes.push("wezterm: tool not found".to_string()),
        }
    }
    if let Some(k) = &id.konsole {
        match probes.first_tool(&KONSOLE_CALLERS) {
            Some((which, tool)) => plan.steps.push(Step::KonsoleSetSession {
                tool,
                style: if which == Tool::Busctl {
                    DbusStyle::Busctl
                } else {
                    DbusStyle::Qdbus
                },
                service: k.service.clone(),
                window: k.window.clone(),
                session: k.session,
            }),
            None => plan
                .notes
                .push("konsole: no D-Bus caller found".to_string()),
        }
    }

    plan
}

fn raise_windows_window(id: &crate::focus::Identity, probes: &Probes, plan: &mut Plan) {
    if let Some(w) = &id.window {
        if probes.window_verifies {
            plan.steps.push(Step::RaiseWindow { handle: w.handle });
        } else {
            plan.notes
                .push("window: no longer verifies, nothing raised".to_string());
        }
    }
}

fn raise_linux(id: &crate::focus::Identity, probes: &Probes, plan: &mut Plan) {
    // The pids whose windows are searched: the recorded terminal process
    // while it still verifies, or each attached tmux client (KTD9).
    let candidates: Vec<u64> = if id.tmux.is_some() {
        probes.tmux_clients.iter().map(|c| c.pid).collect()
    } else {
        // VS Code's window belongs to the pid it exports, not to the pty host
        // above the shell; the terminal process covers everything else.
        let mut pids: Vec<u64> = id.vscode_pid.into_iter().collect();
        match (id.terminal_pid, probes.terminal_alive) {
            (Some(pid), true) => pids.push(pid),
            (Some(_), false) => plan
                .notes
                .push("terminal process: start time changed, no pid search".to_string()),
            (None, _) => {}
        }
        pids
    };

    if id.session_type.as_deref() == Some("wayland") {
        if !probes.kde {
            plan.notes
                .push("wayland: the compositor accepts no outside activation".to_string());
            return;
        }
        let Some(tool) = probes.tool(Tool::Kdotool) else {
            plan.notes
                .push("kde wayland: kdotool not found".to_string());
            return;
        };
        for pid in candidates {
            match probes.kde_windows.get(&pid) {
                Some(ids) if ids.len() == 1 => plan.steps.push(Step::ActivateKde {
                    tool: tool.clone(),
                    window_id: ids[0].clone(),
                }),
                Some(ids) => plan.notes.push(format!(
                    "kde: pid {pid} owns {} windows, none raised",
                    ids.len()
                )),
                None => plan.notes.push(format!("kde: no window for pid {pid}")),
            }
        }
        return;
    }

    let Some((tool, via)) = probes
        .tool(Tool::Xdotool)
        .map(|p| (p, X11Tool::Xdotool))
        .or_else(|| probes.tool(Tool::Wmctrl).map(|p| (p, X11Tool::Wmctrl)))
    else {
        plan.notes
            .push("x11: neither xdotool nor wmctrl found".to_string());
        return;
    };
    if let Some(window_id) = id.window_id {
        plan.steps.push(Step::ActivateX11 {
            tool,
            via,
            window_id,
        });
        return;
    }
    for pid in candidates {
        match probes.x11_windows.get(&pid) {
            Some(ids) if ids.len() == 1 => plan.steps.push(Step::ActivateX11 {
                tool: tool.clone(),
                via,
                window_id: ids[0],
            }),
            Some(ids) => plan.notes.push(format!(
                "x11: pid {pid} owns {} visible windows, none raised",
                ids.len()
            )),
            None => plan.notes.push(format!("x11: no window for pid {pid}")),
        }
    }
}

fn select_macos_tab(record: &Record, probes: &Probes, plan: &mut Plan) {
    let id = &record.identity;
    let Some(family) = id.bundle_id.as_deref().and_then(TabFamily::from_bundle_id) else {
        if id.bundle_id.is_some() {
            plan.notes
                .push("macos: terminal has no tab hook, activation only".to_string());
        }
        return;
    };
    let Some(tool) = probes.tool(Tool::Osascript) else {
        plan.notes.push("macos: osascript not found".to_string());
        return;
    };
    // Inside tmux the recorded tty is the pane's pty; the tabs are found
    // through the attached clients' ttys instead (KTD9).
    let ttys: Vec<String> = if id.tmux.is_some() {
        probes.tmux_clients.iter().map(|c| c.tty.clone()).collect()
    } else {
        id.tty.iter().cloned().collect()
    };
    match family {
        TabFamily::Ghostty => {
            if ttys.is_empty() {
                match &id.ghostty_cwd {
                    Some(cwd) => plan.steps.push(Step::SelectGhostty {
                        tool: tool.clone(),
                        tty: String::new(),
                        cwd: cwd.clone(),
                    }),
                    None => plan
                        .notes
                        .push("ghostty: no tty and no working directory".to_string()),
                }
            }
            for tty in ttys {
                plan.steps.push(Step::SelectGhostty {
                    tool: tool.clone(),
                    tty,
                    cwd: id.ghostty_cwd.clone().unwrap_or_default(),
                });
            }
        }
        family => {
            if ttys.is_empty() {
                plan.notes
                    .push("macos: no tty recorded, no tab selected".to_string());
            }
            for tty in ttys {
                plan.steps.push(Step::SelectTab {
                    tool: tool.clone(),
                    family,
                    tty,
                });
            }
        }
    }
}

/// The click handler. Accepts exactly one argument, resolves it through every
/// gate, and exits quietly whatever happens (R11, R12).
///
/// Log lines about a rejected argument carry its byte length and the reason,
/// never any of its bytes, so a page that invokes the scheme repeatedly
/// cannot write into the debug log. Lines about a record carry the session
/// part and the outcome, never the token (R20).
pub fn run(args: &[OsString]) {
    if args.len() != 1 {
        let n = args.len();
        debug::log(move || format!("focus: expected one argument, got {n}"));
        return;
    }
    let Some(arg) = args[0].to_str() else {
        let n = args[0].len();
        debug::log(move || format!("focus: argument is not UTF-8 ({n} bytes), rejected"));
        return;
    };
    let Some(key) = Key::parse_argument(arg) else {
        let n = arg.len();
        debug::log(move || format!("focus: argument failed the key grammar ({n} bytes)"));
        return;
    };
    let session = key.session().to_string();
    let root = session::state_dir();
    let path = key.record_path(&root);

    // Size before reading, so an oversize file is never even loaded.
    match std::fs::symlink_metadata(&path) {
        Ok(md) if md.len() > MAX_RECORD_BYTES => {
            debug::log(move || format!("focus: record for {session} is oversize, rejected"));
            return;
        }
        Ok(_) => {}
        Err(_) => {
            debug::log(move || format!("focus: no record for {session}"));
            return;
        }
    }
    let Some(bytes) = state::read_trusted(&path) else {
        debug::log(move || format!("focus: record for {session} is not trusted"));
        return;
    };
    let Some(record) = Record::load(&bytes, key.session(), Some(key.token())) else {
        debug::log(move || format!("focus: record for {session} rejected"));
        return;
    };
    // The OS launched this process without the session's environment, so the
    // session's own debug flag decides whether the rest is logged (KTD12).
    if record.debug {
        debug::enable();
    }

    let platform = Platform::current();
    let probes = platform::focus::probe(platform, &record);
    let plan = plan(platform, &record, &probes);
    if debug::is_enabled() {
        let names: Vec<&str> = plan.steps.iter().map(Step::name).collect();
        let summary = format!(
            "focus: {session}: anchor_alive={} steps=[{}]",
            probes.anchor_alive,
            names.join(",")
        );
        debug::log(move || summary);
        for note in &plan.notes {
            let line = format!("focus: {session}: {note}");
            debug::log(move || line);
        }
    }
    for step in &plan.steps {
        let ok = platform::focus::execute(step);
        if debug::is_enabled() {
            let line = format!(
                "focus: {session}: {} {}",
                step.name(),
                if ok { "ok" } else { "failed" }
            );
            debug::log(move || line);
        }
    }
}
