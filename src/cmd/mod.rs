//! Subcommand implementations.
//!
//! One module per runtime script the migration replaces. Each keeps its
//! decision logic in pure functions so the case table can drive it without a
//! process, and exposes one thin entry point that the dispatcher calls.

pub mod focus;
pub mod git_refresh;
pub mod notify;
pub mod statusline;
pub mod subagent;
