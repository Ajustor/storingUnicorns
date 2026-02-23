use crossterm::event::KeyCode;

use crate::services::AppState;

use super::KeyAction;

/// Handle keyboard events when the tables filter is active.
pub fn handle_filter_keys(state: &mut AppState, key_code: KeyCode) -> KeyAction {
    if state.tables_filter_active {
        match key_code {
            KeyCode::Esc => {
                state.tables_filter_active = false;
            }
            KeyCode::Enter => {
                state.tables_filter_active = false;
            }
            KeyCode::Left => {
                if state.tables_filter_cursor > 0 {
                    state.tables_filter_cursor = crate::prev_char_boundary(
                        &state.tables_filter,
                        state.tables_filter_cursor,
                    );
                }
            }
            KeyCode::Right => {
                if state.tables_filter_cursor < state.tables_filter.len() {
                    state.tables_filter_cursor = crate::next_char_boundary(
                        &state.tables_filter,
                        state.tables_filter_cursor,
                    );
                }
            }
            KeyCode::Home => {
                state.tables_filter_cursor = 0;
            }
            KeyCode::End => {
                state.tables_filter_cursor = state.tables_filter.len();
            }
            KeyCode::Char(c) => {
                state.tables_filter.insert(state.tables_filter_cursor, c);
                state.tables_filter_cursor += c.len_utf8();
                state.tables_scroll = 0;
                state.selected_table = 0;
            }
            KeyCode::Backspace => {
                if state.tables_filter_cursor > 0 {
                    let new_cursor = crate::prev_char_boundary(
                        &state.tables_filter,
                        state.tables_filter_cursor,
                    );
                    state.tables_filter.remove(new_cursor);
                    state.tables_filter_cursor = new_cursor;
                }
                state.tables_scroll = 0;
            }
            KeyCode::Delete => {
                if state.tables_filter_cursor < state.tables_filter.len() {
                    state.tables_filter.remove(state.tables_filter_cursor);
                }
                state.tables_scroll = 0;
            }
            _ => {}
        }
        return KeyAction::Consumed;
    }

    if state.results_filter_active {
        match key_code {
            KeyCode::Esc => {
                state.results_filter_active = false;
            }
            KeyCode::Enter => {
                state.results_filter_active = false;
            }
            KeyCode::Left => {
                if state.results_filter_cursor > 0 {
                    state.results_filter_cursor = crate::prev_char_boundary(
                        &state.results_filter,
                        state.results_filter_cursor,
                    );
                }
            }
            KeyCode::Right => {
                if state.results_filter_cursor < state.results_filter.len() {
                    state.results_filter_cursor = crate::next_char_boundary(
                        &state.results_filter,
                        state.results_filter_cursor,
                    );
                }
            }
            KeyCode::Home => {
                state.results_filter_cursor = 0;
            }
            KeyCode::End => {
                state.results_filter_cursor = state.results_filter.len();
            }
            KeyCode::Char(c) => {
                state.results_filter.insert(state.results_filter_cursor, c);
                state.results_filter_cursor += c.len_utf8();
                state.results_scroll = 0;
            }
            KeyCode::Backspace => {
                if state.results_filter_cursor > 0 {
                    let new_cursor = crate::prev_char_boundary(
                        &state.results_filter,
                        state.results_filter_cursor,
                    );
                    state.results_filter.remove(new_cursor);
                    state.results_filter_cursor = new_cursor;
                }
                state.results_scroll = 0;
            }
            KeyCode::Delete => {
                if state.results_filter_cursor < state.results_filter.len() {
                    state.results_filter.remove(state.results_filter_cursor);
                }
                state.results_scroll = 0;
            }
            _ => {}
        }
        return KeyAction::Consumed;
    }

    KeyAction::NotHandled
}
