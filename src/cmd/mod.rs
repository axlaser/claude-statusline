//! Subcommand implementations, one module per runtime script replaced. Each
//! keeps its decisions in pure functions the case table can drive without a
//! process, behind one thin entry point the dispatcher calls.

pub mod focus;
pub mod git_refresh;
pub mod notify;
pub mod statusline;
pub mod subagent;
