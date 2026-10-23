use crossterm::event::{KeyCode, KeyModifiers};

use crate::engine::models::DatabaseType;
use crate::tui::{nosql, ActivePanel, AppState};

use super::KeyAction;

/// Handle keyboard events for the Tables panel.
pub async fn handle_tables_keys(
    state: &mut AppState,
    key_code: KeyCode,
    modifiers: KeyModifiers,
) -> KeyAction {
    match key_code {
        // The cursor on a line not drawn (a hidden schema's): nothing to act on.
        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('h')
            if !state.schemas.is_empty() && !state.table_selection_visible() =>
        {
            KeyAction::Consumed
        }

        // MongoDB / Redis: their own keys (no SQL in the editor).
        _ if nosql::engine(state).is_some() => {
            match handle_nosql_tables_keys(state, key_code, modifiers).await {
                KeyAction::NotHandled => handle_common_keys(state, key_code),
                handled => handled,
            }
        }

        // Select table or toggle schema — generate SELECT query
        KeyCode::Enter => {
            if state.selected_table == 0 {
                state.toggle_schema();
            } else if let Some(table_name) = state.get_selected_table_full_name() {
                // Load its columns for the completion (in the background).
                if let Some(table) = state.selected_table_ref() {
                    crate::tui::columns::request(state, &table);
                }

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

        _ => handle_common_keys(state, key_code),
    }
}

/// Keys of the Tables panel shared by every engine: hidden schemas and the
/// name filter.
fn handle_common_keys(state: &mut AppState, key_code: KeyCode) -> KeyAction {
    match key_code {
        // Hide the schema under the cursor (or show it again)
        KeyCode::Char('h') => {
            state.toggle_selected_schema_hidden();
            KeyAction::Consumed
        }

        // Show / hide the hidden schemas (greyed)
        KeyCode::Char('H') => {
            state.toggle_show_hidden_schemas();
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

/// Keys of the Tables panel of a MongoDB / Redis connection: Enter opens a
/// collection / a key (folds a database / a key group), `s` the indexes,
/// `t` empties (confirmed), `n` a new Redis key, `/` the Redis SCAN
/// pattern; `T` and the SQL schema dialog are not offered.
async fn handle_nosql_tables_keys(
    state: &mut AppState,
    key_code: KeyCode,
    modifiers: KeyModifiers,
) -> KeyAction {
    let redis = nosql::is_redis(state);
    match key_code {
        KeyCode::Enter => nosql::tables_enter(state).await,
        // A database unfolds; on Redis, a key group too.
        KeyCode::Char(' ')
            if state.selected_table == 0
                || matches!(
                    nosql::selected_key_row(state),
                    Some((_, nosql::KeyRow::Group { .. }))
                ) =>
        {
            nosql::tables_enter(state).await
        }
        KeyCode::Char(' ') => {}
        KeyCode::Char('s') => nosql::tables_schema_key(state).await,
        KeyCode::Char('t') if !modifiers.contains(KeyModifiers::CONTROL) => {
            if state.is_connected() {
                nosql::tables_truncate_key(state);
            }
        }
        KeyCode::Char('T') => nosql::sql_only(state, "Vidage par lot"),
        KeyCode::Char('n') if redis => nosql::tables_new_key(state),
        KeyCode::Char('/') if redis => nosql::tables_pattern(state),
        _ => return KeyAction::NotHandled,
    }
    KeyAction::Consumed
}
