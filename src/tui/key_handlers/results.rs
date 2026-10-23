use crossterm::event::KeyCode;

use crate::tui::{nosql, AppState};

use super::KeyAction;

/// Handle keyboard events for the Results panel.
pub async fn handle_results_keys(state: &mut AppState, key_code: KeyCode) -> KeyAction {
    // MongoDB / Redis: rows edited through their `Source`, JSON view.
    if nosql::engine(state).is_some() {
        match key_code {
            KeyCode::Enter => nosql::open_row_editor(state, false),
            KeyCode::Char('a') => nosql::open_row_editor(state, true),
            KeyCode::Delete | KeyCode::Char('d') => nosql::ask_delete_row(state),
            KeyCode::Char('v') => nosql::open_json_view(state),
            KeyCode::Char('x') => nosql::transfer::open_results(state),
            _ => return handle_common_keys(state, key_code),
        }
        return KeyAction::Consumed;
    }
    match key_code {
        // Edit selected row
        KeyCode::Enter => {
            state.open_edit_row_dialog();
            KeyAction::Consumed
        }

        // Add new row
        KeyCode::Char('a') => {
            if state.query_result.is_some() {
                state.open_add_row_dialog();
            } else {
                state.set_status("Execute a query first to add rows");
            }
            KeyAction::Consumed
        }

        // Delete selected row
        KeyCode::Delete | KeyCode::Char('d') => {
            if state.query_result.is_some() {
                state.open_delete_row_confirm();
            }
            KeyAction::Consumed
        }

        // Export shortcut
        KeyCode::Char('x') => {
            if state.query_result.is_some() {
                state.open_export_dialog();
            }
            KeyAction::Consumed
        }

        _ => handle_common_keys(state, key_code),
    }
}

/// Keys of the Results panel shared by every engine: scrolling and filter.
fn handle_common_keys(state: &mut AppState, key_code: KeyCode) -> KeyAction {
    match key_code {
        // Horizontal scroll
        KeyCode::Left => {
            state.results_scroll_x = state.results_scroll_x.saturating_sub(1);
            KeyAction::Consumed
        }
        KeyCode::Right => {
            if let Some(ref result) = state.query_result {
                if state.results_scroll_x < result.columns.len().saturating_sub(1) {
                    state.results_scroll_x += 1;
                }
            }
            KeyAction::Consumed
        }
        KeyCode::Home => {
            state.results_scroll_x = 0;
            KeyAction::Consumed
        }
        KeyCode::End => {
            if let Some(ref result) = state.query_result {
                state.results_scroll_x = result.columns.len().saturating_sub(1);
            }
            KeyAction::Consumed
        }

        // Activate results filter
        KeyCode::Char('/') => {
            if state.query_result.is_some() {
                state.results_filter_active = true;
                state.results_filter_cursor = state.results_filter.len();
            }
            KeyAction::Consumed
        }

        // Clear filter with Escape (when filter exists but not active)
        KeyCode::Esc if !state.results_filter.is_empty() => {
            state.results_filter.clear();
            state.results_filter_cursor = 0;
            state.results_scroll = 0;
            KeyAction::Consumed
        }

        _ => KeyAction::NotHandled,
    }
}
