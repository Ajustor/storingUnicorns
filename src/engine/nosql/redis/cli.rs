//! Redis console syntax (plan 13, « Syntaxe Redis »): redis-cli tokenizing,
//! safety classification and reply shaping. Types set by Task 1, bodies by
//! Task 3.
#![allow(unused_variables)] // stubs (plan 13, Task 3)

use crate::engine::models::QueryResult;

/// What to do with a command before sending it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Safety {
    Safe,
    /// Destructive: confirm first (French reason).
    Confirm(String),
    /// Never sent from the console (blocking, subscriptions…).
    Refuse(String),
    /// `KEYS`: run as an iterative `SCAN MATCH`.
    RewriteScan,
}

/// The arguments of one command line (redis-cli quoting rules).
pub fn tokenize(line: &str) -> Result<Vec<Vec<u8>>, String> {
    Err(super::super::not_implemented(super::ENGINE).to_string())
}

pub fn classify(argv: &[Vec<u8>]) -> Safety {
    Safety::Safe
}

/// The reply of `argv` as a grid.
pub fn reply_to_result(argv: &[Vec<u8>], reply: &::redis::Value) -> QueryResult {
    QueryResult::default()
}
