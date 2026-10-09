//! UI-agnostic core: configuration, database connectors, models and
//! operations shared by the GUI and the TUI.
pub mod config;
pub mod db;
pub mod models;
pub mod ops;
#[allow(dead_code)] // TODO(tls-presets): drop once the dialogs use it.
pub mod presets;
pub mod services;
pub mod sql;
