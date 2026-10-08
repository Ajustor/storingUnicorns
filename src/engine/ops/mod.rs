//! Business operations shared by the GUI and the TUI. Each function takes a
//! connection and explicit parameters and returns a typed result: no UI state.
pub mod query;
pub mod rows;
pub mod schema;
pub mod transfer;

#[cfg(test)]
pub(crate) mod test_support;
