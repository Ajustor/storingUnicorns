mod connections;
mod editor;
mod filter;
mod global;
mod results;
mod tables;

pub use connections::handle_connections_keys;
pub use editor::handle_editor_keys;
pub use filter::handle_filter_keys;
pub use global::handle_global_keys;
pub use results::handle_results_keys;
pub use tables::handle_tables_keys;

/// Result of a key handler indicating what action to take.
#[derive(Debug, PartialEq)]
pub enum KeyAction {
    /// Key was consumed, no further processing needed.
    Consumed,
    /// Key was not handled; try the next handler.
    NotHandled,
    /// Application should quit.
    Quit,
}
