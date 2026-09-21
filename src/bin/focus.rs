//! `claude-statusline-focus`: the Windows click helper.
//!
//! The shell launches it with the toast's launch URI on click. It is a
//! GUI-subsystem program so the launch never allocates a console: a
//! console-subsystem image started by the shell gets a console or Windows
//! Terminal window, and the click must land in the terminal the user already
//! has. Elsewhere the attribute is ignored and the binary behaves as the
//! `focus` subcommand. There is no logic here: the entry layers are
//! the two calls `main.rs` makes, and `cmd::focus::run` treats its one
//! OS-string argument as hostile.
#![windows_subsystem = "windows"]

use claude_statusline::{cmd, entry};

fn main() {
    // Layers 1 and 2: fd 2 gone and the panic hook silent before argv is read.
    entry::silence();

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // Layers 3 and 4. Nothing here writes to stdout, but the layer stays so
    // the helper's contract is the binary's contract, verbatim.
    entry::guarded("focus", || cmd::focus::run(&args));

    // Layer 5.
    std::process::exit(0);
}
