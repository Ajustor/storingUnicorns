use crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::AppState;

use super::KeyAction;

/// Handle keyboard events for the QueryEditor panel.
pub async fn handle_editor_keys(
    state: &mut AppState,
    key_code: KeyCode,
    modifiers: KeyModifiers,
) -> KeyAction {
    match key_code {
        // Execute current query at cursor: Ctrl+Enter (Ctrl+E, see fallbacks)
        KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) => {
            crate::tui::handle_execute_current_query(state).await;
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

        // Autocompletion: Dismiss with Escape, also while it waits for
        // columns with nothing shown (Escape then keeps its usual effect)
        KeyCode::Esc if state.show_completion || state.completion_wanted => {
            let shown = state.show_completion;
            state.hide_completion();
            if shown {
                KeyAction::Consumed
            } else {
                KeyAction::NotHandled
            }
        }

        // Autocompletion: Trigger with Ctrl+Space (Ctrl+K, see fallbacks)
        KeyCode::Char(' ') if modifiers.contains(KeyModifiers::CONTROL) => {
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
                    let prev = crate::tui::prev_char_boundary(&query, pos);
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
        KeyCode::Left if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion => {
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            if !state.has_selection() {
                state.start_selection();
            }
            let new_pos = crate::tui::prev_char_boundary(&query, pos);
            state.set_cursor_position(new_pos);
            state.extend_selection(new_pos);
            KeyAction::Consumed
        }

        // Shift+Right: extend selection right
        KeyCode::Right if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion => {
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            if !state.has_selection() {
                state.start_selection();
            }
            let new_pos = crate::tui::next_char_boundary(&query, pos);
            state.set_cursor_position(new_pos);
            state.extend_selection(new_pos);
            KeyAction::Consumed
        }

        // Shift+Up: extend selection up
        KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion => {
            if !state.has_selection() {
                state.start_selection();
            }
            crate::tui::move_cursor_up(state);
            state.extend_selection(state.cursor_position());
            KeyAction::Consumed
        }

        // Shift+Down: extend selection down
        KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) && !state.show_completion => {
            if !state.has_selection() {
                state.start_selection();
            }
            crate::tui::move_cursor_down(state);
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
            state.set_cursor_position(crate::tui::prev_char_boundary(&query, pos));
            KeyAction::Consumed
        }

        // Normal Right
        KeyCode::Right if !state.show_completion => {
            state.clear_selection();
            let pos = state.cursor_position();
            let query = state.query_input().to_string();
            state.set_cursor_position(crate::tui::next_char_boundary(&query, pos));
            KeyAction::Consumed
        }

        // Normal Up
        KeyCode::Up if !state.show_completion => {
            state.clear_selection();
            crate::tui::move_cursor_up(state);
            KeyAction::Consumed
        }

        // Normal Down
        KeyCode::Down if !state.show_completion => {
            state.clear_selection();
            crate::tui::move_cursor_down(state);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::engine::config::AppConfig;
    use crate::engine::models::{ConnectionConfig, DatabaseType, SchemaInfo};
    use crate::engine::ops::test_support::sqlite_mem;
    use crate::tui::ActivePanel;

    /// Ctrl+Space after `SELECT u.` while the columns of `users` load:
    /// nothing shown yet.
    async fn waiting_for_columns() -> AppState {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.current_connection_config = Some(ConnectionConfig {
            db_type: DatabaseType::SQLite,
            ..Default::default()
        });
        state.schemas = vec![SchemaInfo {
            name: "main".into(),
            tables: vec!["users".into()],
            expanded: true,
        }];
        state.connection = Some(Arc::new(
            sqlite_mem(&["CREATE TABLE users (id INTEGER PRIMARY KEY)"]).await,
        ));
        state.active_panel = ActivePanel::QueryEditor;
        state.set_query("SELECT u. FROM users u".into());
        state.set_cursor_position("SELECT u.".len());
        handle_editor_keys(&mut state, KeyCode::Char(' '), KeyModifiers::CONTROL).await;
        assert!(!state.show_completion && state.completion_wanted);
        state
    }

    /// Poll until the columns arrived.
    async fn settle(state: &mut AppState) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.completion_columns.is_loading() {
            assert!(Instant::now() < deadline, "columns never arrived");
            crate::tui::columns::poll(state).await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Ctrl+K (completion) and Ctrl+E (run the statement) reach the editor
    /// like Ctrl+Space and Ctrl+Enter, which macOS terminals may not send.
    #[tokio::test]
    async fn mac_terminal_fallbacks_reach_the_editor() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.active_panel = ActivePanel::QueryEditor;
        state.set_query("SEL".into());
        state.set_cursor_position(3);
        let (code, mods) = super::super::resolve_fallback(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
            state.active_panel,
        );
        handle_editor_keys(&mut state, code, mods).await;
        assert!(state.show_completion, "Ctrl+K completes");
        assert_eq!(state.query_input(), "SEL", "nothing typed");
        state.hide_completion();
        // Not connected: Ctrl+E tries to run, nothing is typed.
        let (code, mods) = super::super::resolve_fallback(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
            state.active_panel,
        );
        let action = handle_editor_keys(&mut state, code, mods).await;
        assert_eq!(action, KeyAction::Consumed);
        assert_eq!(state.query_input(), "SEL");
        assert!(
            state.status_message.contains("Not connected"),
            "{}",
            state.status_message
        );
    }

    #[tokio::test]
    async fn arriving_columns_open_the_completion_asked_for() {
        let mut state = waiting_for_columns().await;
        settle(&mut state).await;
        assert!(state.show_completion);
        assert_eq!(state.completion_suggestions[0].label, "id");
    }

    #[tokio::test]
    async fn escape_on_an_empty_popup_keeps_arriving_columns_hidden() {
        let mut state = waiting_for_columns().await;
        let action = handle_editor_keys(&mut state, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(matches!(action, KeyAction::NotHandled));
        assert!(!state.completion_wanted);
        settle(&mut state).await;
        assert!(!state.show_completion);
        assert!(state.completion_suggestions.is_empty());
    }

    #[tokio::test]
    async fn arriving_columns_stay_hidden_once_the_cursor_moved() {
        let mut state = waiting_for_columns().await;
        handle_editor_keys(&mut state, KeyCode::Right, KeyModifiers::NONE).await;
        settle(&mut state).await;
        assert!(!state.show_completion && !state.completion_wanted);
        // Enter is a newline again.
        handle_editor_keys(&mut state, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(state.query_input(), "SELECT u. \nFROM users u");
    }

    #[tokio::test]
    async fn arriving_columns_stay_hidden_after_typing() {
        let mut state = waiting_for_columns().await;
        handle_editor_keys(&mut state, KeyCode::Char('x'), KeyModifiers::NONE).await;
        settle(&mut state).await;
        assert!(!state.show_completion && !state.completion_wanted);
    }

    #[tokio::test]
    async fn arriving_columns_stay_hidden_in_another_panel() {
        let mut state = waiting_for_columns().await;
        state.active_panel = ActivePanel::Results;
        settle(&mut state).await;
        assert!(!state.show_completion && !state.completion_wanted);
        // Back in the editor: still nothing popped up.
        state.active_panel = ActivePanel::QueryEditor;
        crate::tui::columns::poll(&mut state).await;
        assert!(!state.show_completion);
    }
}
