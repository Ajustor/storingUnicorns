use crossterm::event::KeyCode;

use crate::services::AppState;

use super::KeyAction;

/// Handle keyboard events for the Connections panel.
pub async fn handle_connections_keys(state: &mut AppState, key_code: KeyCode) -> KeyAction {
    match key_code {
        // Connect to selected database
        KeyCode::Enter => {
            // handle_connect needs the terminal, so we signal back and the caller handles it
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
                state.config.connections.remove(state.selected_connection);
                if state.selected_connection >= state.config.connections.len()
                    && state.selected_connection > 0
                {
                    state.selected_connection -= 1;
                }
                state.set_status(format!("Deleted connection: {}", name));
            }
            KeyAction::Consumed
        }

        _ => KeyAction::NotHandled,
    }
}
