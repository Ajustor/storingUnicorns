use crossterm::event::{KeyCode, KeyModifiers};

use crate::services::AppState;

use super::KeyAction;

/// Handle keyboard events for the QueryEditor panel.
pub async fn handle_editor_keys(
    state: &mut AppState,
    key_code: KeyCode,
    modifiers: KeyModifiers,
) -> KeyAction {
    match key_code {
        // Execute current query at cursor: Ctrl+Enter
        KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) => {
            crate::handle_execute_current_query(state).await;
            KeyAction::Consumed
        }

        // Autocompletion: Apply with Enter
        KeyCode::Enter if state.show_completion => {
            state.apply_completion();
            KeyAction::Consumed
        }

        // Autocompletion: Navigate Up
        KeyCode::Up if state.show_completion => {
            state.completion_prev();
            KeyAction::Consumed
        }

        // Autocompletion: Navigate Down
        KeyCode::Down if state.show_completion => {
            state.completion_next();
            KeyAction::Consumed
        }

        // Autocompletion: Dismiss with Escape
        KeyCode::Esc if state.show_completion => {
            state.hide_completion();
            KeyAction::Consumed
        }

        // Autocompletion: Trigger with Ctrl+Space
        KeyCode::Char(' ') if modifiers.contains(KeyModifiers::CONTROL) => {
            crate::update_completions_from_context(state).await;
            state.update_completions();
            KeyAction::Consumed
        }

        // Newline (only if completion not shown)
        KeyCode::Enter if !state.show_completion => {
            let pos = state.cursor_position();
            state.query_input_mut().insert(pos, '\n');
            state.set_cursor_position(pos + '\n'.len_utf8());
            state.query_tabs.current_tab_mut().is_modified = true;
            state.hide_completion();
            KeyAction::Consumed
        }

        // Character input (exclude Ctrl combinations, but allow AltGr = Ctrl+Alt)
        KeyCode::Char(c)
            if !modifiers.contains(KeyModifiers::CONTROL)
                || modifiers.contains(KeyModifiers::ALT) =>
        {
            if state.has_selection() {
                state.delete_selection();
            }
            let pos = state.cursor_position();
            state.query_input_mut().insert(pos, c);
            state.set_cursor_position(pos + c.len_utf8());
            state.query_tabs.current_tab_mut().is_modified = true;
            if state.show_completion {
                state.update_completions();
            }
            KeyAction::Consumed
        }

        // Backspace
        KeyCode::Backspace => {
            if state.has_selection() {
                state.delete_selection();
                state.query_tabs.current_tab_mut().is_modified = true;
            } else {
                let pos = state.cursor_position();
                if pos > 0 {
                    let query = state.query_input().to_string();
                    let prev = crate::prev_char_boundary(&query, pos);
                    state.set_cursor_position(prev);
                    state.query_input_mut().remove(prev);
                    state.query_tabs.current_tab_mut().is_modified = true;
                }
            }
            state.hide_completion();
            KeyAction::Consumed
        }

        // Delete
        KeyCode::Delete => {
            if state.has_selection() {
                state.delete_selection();
                state.query_tabs.current_tab_mut().is_modified = true;
            } else {
                let pos = state.cursor_position();
                if pos < state.query_input().len() {
                    state.query_input_mut().remove(pos);
                    state.query_tabs.current_tab_mut().is_modified = true;
                }
            }
            state.hide_completion();
            KeyAction::Consumed
        }

        // Shift+Left: extend selection left
        KeyCode::Left
            if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion =>
        {
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            if !state.has_selection() {
                state.start_selection();
            }
            let new_pos = crate::prev_char_boundary(&query, pos);
            state.set_cursor_position(new_pos);
            state.extend_selection(new_pos);
            KeyAction::Consumed
        }

        // Shift+Right: extend selection right
        KeyCode::Right
            if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion =>
        {
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            if !state.has_selection() {
                state.start_selection();
            }
            let new_pos = crate::next_char_boundary(&query, pos);
            state.set_cursor_position(new_pos);
            state.extend_selection(new_pos);
            KeyAction::Consumed
        }

        // Shift+Up: extend selection up
        KeyCode::Up
            if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion =>
        {
            if !state.has_selection() {
                state.start_selection();
            }
            crate::move_cursor_up(state);
            state.extend_selection(state.cursor_position());
            KeyAction::Consumed
        }

        // Shift+Down: extend selection down
        KeyCode::Down
            if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion =>
        {
            if !state.has_selection() {
                state.start_selection();
            }
            crate::move_cursor_down(state);
            state.extend_selection(state.cursor_position());
            KeyAction::Consumed
        }

        // Shift+Home: select to start of line
        KeyCode::Home if modifiers.contains(KeyModifiers::SHIFT) => {
            if !state.has_selection() {
                state.start_selection();
            }
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            let before_cursor = &query[..pos];
            if let Some(line_start) = before_cursor.rfind('\n') {
                state.set_cursor_position(line_start + 1);
            } else {
                state.set_cursor_position(0);
            }
            state.extend_selection(state.cursor_position());
            KeyAction::Consumed
        }

        // Shift+End: select to end of line
        KeyCode::End if modifiers.contains(KeyModifiers::SHIFT) => {
            if !state.has_selection() {
                state.start_selection();
            }
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            let after_cursor = &query[pos..];
            if let Some(line_end) = after_cursor.find('\n') {
                state.set_cursor_position(pos + line_end);
            } else {
                state.set_cursor_position(query.len());
            }
            state.extend_selection(state.cursor_position());
            KeyAction::Consumed
        }

        // Select all: Ctrl+A
        KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.select_all();
            KeyAction::Consumed
        }

        // Copy: Ctrl+C
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(selected_text) = state.get_selected_text() {
                state.set_status(format!("Copied {} characters", selected_text.len()));
            }
            KeyAction::Consumed
        }

        // Cut: Ctrl+X
        KeyCode::Char('x') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(selected_text) = state.delete_selection() {
                state.query_tabs.current_tab_mut().is_modified = true;
                state.set_status(format!("Cut {} characters", selected_text.len()));
            }
            KeyAction::Consumed
        }

        // Clear selection with Escape
        KeyCode::Esc if state.has_selection() => {
            state.clear_selection();
            KeyAction::Consumed
        }

        // Normal Left (clear selection on move)
        KeyCode::Left if !state.show_completion => {
            state.clear_selection();
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            state.set_cursor_position(crate::prev_char_boundary(&query, pos));
            KeyAction::Consumed
        }

        // Normal Right
        KeyCode::Right if !state.show_completion => {
            state.clear_selection();
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            state.set_cursor_position(crate::next_char_boundary(&query, pos));
            KeyAction::Consumed
        }

        // Normal Up
        KeyCode::Up if !state.show_completion => {
            state.clear_selection();
            crate::move_cursor_up(state);
            KeyAction::Consumed
        }

        // Normal Down
        KeyCode::Down if !state.show_completion => {
            state.clear_selection();
            crate::move_cursor_down(state);
            KeyAction::Consumed
        }

        // Home: start of current line
        KeyCode::Home => {
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            let before_cursor = &query[..pos];
            if let Some(line_start) = before_cursor.rfind('\n') {
                state.set_cursor_position(line_start + 1);
            } else {
                state.set_cursor_position(0);
            }
            KeyAction::Consumed
        }

        // End: end of current line
        KeyCode::End => {
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            let after_cursor = &query[pos..];
            if let Some(line_end) = after_cursor.find('\n') {
                state.set_cursor_position(pos + line_end);
            } else {
                state.set_cursor_position(query.len());
            }
            KeyAction::Consumed
        }

        // Close tab: Ctrl+W
        KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
            if !state.query_tabs.close_current_tab() {
                state.set_status("Cannot close the last tab");
            }
            KeyAction::Consumed
        }

        _ => KeyAction::NotHandled,
    }
}
