use crossterm::event::KeyCode;

use crate::engine::db::entra_interactive;
use crate::tui::AppState;

use super::KeyAction;

/// Handle keyboard events for the Connections panel.
pub async fn handle_connections_keys(state: &mut AppState, key_code: KeyCode) -> KeyAction {
    let on_folder = state.selected_folder.is_some();
    match key_code {
        // Fold / unfold the folder under the cursor
        KeyCode::Enter | KeyCode::Char(' ') if on_folder => {
            state.toggle_selected_folder();
            KeyAction::Consumed
        }
        KeyCode::Left => {
            state.connections_left();
            KeyAction::Consumed
        }
        KeyCode::Right => {
            state.connections_right();
            KeyAction::Consumed
        }

        // New folder (under the cursor's folder)
        KeyCode::Char('f') => {
            state.open_new_folder_prompt();
            KeyAction::Consumed
        }

        // Move the connection to a folder (a folder: rename it)
        KeyCode::Char('m') => {
            state.open_move_prompt();
            KeyAction::Consumed
        }

        // Rename the folder
        KeyCode::Char('r') | KeyCode::Char('e') if on_folder => {
            state.open_rename_folder_prompt();
            KeyAction::Consumed
        }

        // Delete the folder (its content moves up one level)
        KeyCode::Char('d') if on_folder => {
            state.delete_selected_folder();
            KeyAction::Consumed
        }

        // Connect to selected database
        KeyCode::Enter => {
            // The event loop opens it in the background (`connect::start`).
            KeyAction::NotHandled
        }

        // New connection dialog
        KeyCode::Char('n') => {
            state.open_new_connection_dialog();
            KeyAction::Consumed
        }

        // Edit connection dialog
        KeyCode::Char('e') => {
            if !state.config.connections.is_empty() {
                state.open_edit_connection_dialog(state.selected_connection);
            }
            KeyAction::Consumed
        }

        // Delete connection
        KeyCode::Char('d') => {
            if !state.config.connections.is_empty() {
                let name = state.config.connections[state.selected_connection]
                    .name
                    .clone();
                if state
                    .pending_connect
                    .as_ref()
                    .is_some_and(|p| p.config.name == name)
                {
                    crate::tui::connect::cancel(state);
                }
                let removed = state.config.connections.remove(state.selected_connection);
                // Unless another connection (another base of the server) uses it.
                entra_interactive::forget_unless_used(&removed, &state.config.connections);
                if state.selected_connection >= state.config.connections.len()
                    && state.selected_connection > 0
                {
                    state.selected_connection -= 1;
                }
                // The cursor stays on a visible line.
                let rows = state.connection_rows();
                let pos = state.connection_cursor(&rows);
                if let Some(row) = rows.get(pos).cloned() {
                    state.select_connection_row(&row);
                }
                state.set_status(format!("Deleted connection: {}", name));
            }
            KeyAction::Consumed
        }

        _ => KeyAction::NotHandled,
    }
}
