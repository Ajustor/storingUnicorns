//! Business operations shared by the GUI and the TUI. Each function takes a
//! connection and explicit parameters and returns a typed result: no UI state.
pub mod query;
pub mod rows;
pub mod schema;
pub mod transfer;

#[cfg(test)]
#[allow(dead_code)] // helpers are used by the ops tests added in later tasks
pub(crate) mod test_support;
