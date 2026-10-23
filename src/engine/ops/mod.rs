//! Business operations shared by the GUI and the TUI. Each function takes a
//! connection and explicit parameters and returns a typed result: no UI state.
pub mod query;
pub mod rows;
pub mod schema;
pub mod transfer;
pub mod tx;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod nosql_guards;
#[cfg(test)]
pub(crate) mod test_support;
