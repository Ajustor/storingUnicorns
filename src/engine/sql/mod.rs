//! SQL text utilities shared by the frontends.
pub mod completion;
pub mod format;
pub mod lexer;
#[allow(dead_code)] // used by the GUI (plan 3b)
pub mod paging;
// TODO(fk-navigation): used by the GUI task
#[allow(dead_code)]
pub mod relations;
pub mod statements;
