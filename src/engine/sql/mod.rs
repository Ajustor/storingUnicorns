//! SQL text utilities shared by the frontends.
// TODO(context-completion): used by the GUI/TUI tasks
#[allow(dead_code)]
pub mod completion;
pub mod format;
pub mod lexer;
#[allow(dead_code)] // used by the GUI (plan 3b)
pub mod paging;
pub mod statements;
