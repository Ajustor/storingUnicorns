use crossterm::event::{KeyCode, KeyModifiers};

use crate::models::DatabaseType;
use crate::services::{ActivePanel, AppState};

use super::KeyAction;

/// Handle keyboard events for the Tables panel.
pub async fn handle_tables_keys(
    state: &mut AppState,
    key_code: KeyCode,
    modifiers: KeyModifiers,
) -> KeyAction {
    match key_code {
        // Select table or toggle schema — generate SELECT query
        KeyCode::Enter => {
            if state.selected_table == 0 {
                state.toggle_schema();
            } else if let Some(table_name) = state.get_selected_table_full_name() {
                // Pre-fetch columns for autocompletion (async, cached)
                let _ = crate::fetch_table_columns(state, &table_name).await;
                state.current_table_context = Some(table_name.clone());

                let query = if let Some(ref config) = state.current_connection_config {
                    match config.db_type {
                        DatabaseType::SQLServer | DatabaseType::Azure => {
                            format!("SELECT TOP 100 * FROM {};", table_name)
                        }
                        _ => {
                            format!("SELECT * FROM {} LIMIT 100;", table_name)
                        }
                    }
                } else {
                    format!("SELECT * FROM {} LIMIT 100;", table_name)
                };
                state.set_query(query);
                state.active_panel = ActivePanel::QueryEditor;
            }
            KeyAction::Consumed
        }

        // Toggle schema expansion with Space
        KeyCode::Char(' ') => {
            if state.selected_table == 0 {
                state.toggle_schema();
            }
            KeyAction::Consumed
        }

        // Open schema modification dialog
        KeyCode::Char('s') => {
            if state.selected_table > 0 && state.is_connected() {
                state.open_schema_dialog();
            } else {
                state.set_status("Select a table first (not a schema)");
            }
            KeyAction::Consumed
        }

        // Truncate selected table
        KeyCode::Char('t') if !modifiers.contains(KeyModifiers::CONTROL) => {
            if state.selected_table > 0 && state.is_connected() {
                state.open_truncate_confirm();
            } else {
                state.set_status("Select a table first");
            }
            KeyAction::Consumed
        }

        // Batch truncate
        KeyCode::Char('T') if modifiers.contains(KeyModifiers::SHIFT) => {
            if state.is_connected() && !state.schemas.is_empty() {
                state.open_batch_truncate_dialog();
            } else {
                state.set_status("Not connected or no tables.");
            }
            KeyAction::Consumed
        }

        // Activate filter mode
        KeyCode::Char('/') => {
            state.tables_filter_active = true;
            state.tables_filter_cursor = state.tables_filter.len();
            KeyAction::Consumed
        }

        // Clear filter with Escape (when filter exists but not active)
        KeyCode::Esc if !state.tables_filter.is_empty() => {
            state.tables_filter.clear();
            state.tables_filter_cursor = 0;
            state.tables_scroll = 0;
            KeyAction::Consumed
        }

        _ => KeyAction::NotHandled,
    }
}
