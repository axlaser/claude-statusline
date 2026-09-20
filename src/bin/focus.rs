//! `claude-statusline-focus`: the Windows click helper.
//!
//! The shell launches this executable with the toast's launch URI when the
//! user clicks a notification (KTD4). It is a GUI-subsystem program so that
//! the launch never allocates a console — a console-subsystem image started
//! by the shell gets a console window, or a whole Windows Terminal window when
//! that is the default terminal, and the point of the click is to land in the
//! terminal the user already has (R10). The attribute is ignored elsewhere,
//! and the binary builds and behaves as the `focus` subcommand on every
//! platform (KTD14).
//!
//! There is no logic here. The entry layers are the same two calls `main.rs`
//! makes, and everything else is `cmd::focus::run`, which accepts exactly one
//! OS-string argument and treats it as hostile (R12).
#![windows_subsystem = "windows"]

use claude_statusline::{cmd, entry};

fn main() {
    // Layers 1 and 2: fd 2 is gone and the panic hook is silent before argv
    // is read.
    entry::silence();

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // Layers 3 and 4: a panic becomes a silent no-op and stdout is flushed
    // with the result checked. Nothing here writes to stdout, but the layer
    // stays so the helper's contract is the binary's contract, verbatim.
    entry::guarded("focus", || cmd::focus::run(&args));

    // Layer 5.
    std::process::exit(0);
}
