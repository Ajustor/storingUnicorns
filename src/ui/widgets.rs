// This module re-exports all panel rendering functions from the new `panels/` directory.
// Each panel now lives in its own file for independent management.
//
// Panel files:
//   panels/connections.rs   - Connections panel
//   panels/tables.rs        - Tables panel
//   panels/query_editor.rs  - Query editor panel
//   panels/results.rs       - Results panel
//   panels/status_bar.rs    - Status bar & help bar

pub use super::panels::render_connections_panel;
pub use super::panels::render_help_bar;
pub use super::panels::render_query_editor;
pub use super::panels::render_results_panel;
pub use super::panels::render_status_bar;
pub use super::panels::render_tables_panel;

// Re-export draw_cursor for use by modals
pub use super::panels::common::draw_cursor;

