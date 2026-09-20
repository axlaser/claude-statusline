//! The silent-degradation entry layers, as two library calls.
//!
//! `main.rs` used to carry all five layers inline. The helper binary the click
//! path adds (`src/bin/focus.rs`) needs exactly the same layers, and two copies
//! of a contract this load-bearing would drift the first time one of them was
//! touched. So the layers live here: [`silence`] is what runs before anything
//! else, [`guarded`] is what wraps a subcommand, and the final exit stays in
//! each `main`, because `std::process::exit` is the one call a library must
//! not make on a caller's behalf.
//!
//! Nothing here branches on the platform. The one platform-specific step, the
//! fd 2 redirect, is reached through `platform::redirect_stderr_to_null`, so
//! this file does not join the confinement list.

use std::io::Write;

use crate::{debug, platform};

/// Layers 1 and 2: take fd 2 away, then silence the panic hook.
///
/// Call this first, before argv is read. The panic hook below cannot
/// intercept a stack overflow or an allocation failure — those are written by
/// the runtime straight to the descriptor, which is why the redirect comes
/// first and is not optional.
pub fn silence() {
    // Layer 1: take fd 2 away before any code can write to it.
    platform::redirect_stderr_to_null();

    // Layer 2: silence the default hook's multi-line panic message.
    //
    // Silent to the terminal always; silent to the debug log only when logging
    // is off. README and the release notes both tell users a `panic caught`
    // line is always worth reporting, and the catch in `guarded` can only name
    // the subcommand — the message and location live in the `PanicHookInfo`
    // the hook receives and nowhere else. Writing them to the log costs the
    // contract nothing: the log is a file, not fd 2.
    if debug::is_enabled() {
        std::panic::set_hook(Box::new(|info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "unknown location".to_string());
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            debug::log(move || format!("panic at {location}: {message}"));
        }));
    } else {
        std::panic::set_hook(Box::new(|_| {}));
    }
}

/// Layers 3 and 4: run `body` under `catch_unwind`, then flush stdout and
/// check the flush.
///
/// `name` is the subcommand, for the log line a caught panic leaves. The
/// caller performs layer 5, `std::process::exit(0)`, immediately after this
/// returns — nothing else may run between the flush and the exit.
pub fn guarded<F: FnOnce()>(name: &str, body: F) {
    // Layer 3: an unwinding panic anywhere below becomes a silent no-op.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    if result.is_err() {
        let name = name.to_string();
        debug::log(move || format!("panic caught in subcommand `{name}`"));
    }

    // Layer 4: flush before exiting. `std::process::exit` runs no destructors,
    // so a buffered writer dropped after this would silently discard the
    // render and still satisfy every exit-code and stderr assertion.
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if lock.flush().is_err() {
        debug::log(|| "stdout flush failed at exit".to_string());
    }
}
