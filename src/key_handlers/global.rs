use crossterm::event::{KeyCode, KeyModifiers};

use crate::services::{ActivePanel, AppState};

use super::KeyAction;

/// Handle global keyboard shortcuts that apply regardless of the active panel.
pub async fn handle_global_keys(
    state: &mut AppState,
    key_code: KeyCode,
    modifiers: KeyModifiers,
) -> KeyAction {
    match key_code {
        // Quit: Ctrl+Q always, or 'q' outside editor
        KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
            KeyAction::Quit
        }
        KeyCode::Char('q') if state.active_panel != ActivePanel::QueryEditor => {
            state.should_quit = true;
            KeyAction::Quit
        }

        // Tab navigation: Ctrl+Tab → next tab, Ctrl+Shift+Tab → prev tab
        KeyCode::Tab
            if state.active_panel == ActivePanel::QueryEditor
                && modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            state.query_tabs.prev_tab();
            KeyAction::Consumed
        }
        KeyCode::Tab
            if state.active_panel == ActivePanel::QueryEditor
                && modifiers.contains(KeyModifiers::CONTROL) =>
        {
            state.query_tabs.next_tab();
            KeyAction::Consumed
        }

        // Panel navigation: Tab / Shift+Tab
        KeyCode::Tab if !modifiers.contains(KeyModifiers::CONTROL) => {
            state.next_panel();
            KeyAction::Consumed
        }
        KeyCode::BackTab => {
            state.prev_panel();
            KeyAction::Consumed
        }

        // List navigation (Up/Down/k/j shared across non-editor panels)
        KeyCode::Up | KeyCode::Char('k') if state.active_panel != ActivePanel::QueryEditor => {
            state.select_prev();
            KeyAction::Consumed
        }
        KeyCode::Down | KeyCode::Char('j') if state.active_panel != ActivePanel::QueryEditor => {
            state.select_next();
            KeyAction::Consumed
        }

        // Panel resizing: Alt+=/Alt+-
        KeyCode::Char('+') | KeyCode::Char('=') if modifiers.contains(KeyModifiers::ALT) => {
            match state.active_panel {
                ActivePanel::Tables | ActivePanel::Connections => {
                    state.adjust_sidebar_width(5);
                }
                ActivePanel::QueryEditor => {
                    state.adjust_query_editor_height(5);
                }
                ActivePanel::Results => {
                    state.adjust_query_editor_height(-5);
                }
            }
            KeyAction::Consumed
        }
        KeyCode::Char('-') if modifiers.contains(KeyModifiers::ALT) => {
            match state.active_panel {
                ActivePanel::Tables | ActivePanel::Connections => {
                    state.adjust_sidebar_width(-5);
                }
                ActivePanel::QueryEditor => {
                    state.adjust_query_editor_height(-5);
                }
                ActivePanel::Results => {
                    state.adjust_query_editor_height(5);
                }
            }
            KeyAction::Consumed
        }

        // Execute query: F5
        KeyCode::F(5) => {
            crate::handle_execute_query(state).await;
            KeyAction::Consumed
        }

        // Export results: F6
        KeyCode::F(6) if !modifiers.contains(KeyModifiers::SHIFT) => {
            if state.query_result.is_some() {
                state.open_export_dialog();
            } else {
                state.set_status("No results to export. Execute a query first.");
            }
            KeyAction::Consumed
        }

        // Batch export: Shift+F6
        KeyCode::F(6) if modifiers.contains(KeyModifiers::SHIFT) => {
            if state.is_connected() && !state.schemas.is_empty() {
                state.open_batch_export_dialog();
            } else {
                state.set_status("Not connected or no tables. Connect to a database first.");
            }
            KeyAction::Consumed
        }

        // Import CSV: F7
        KeyCode::F(7) if !modifiers.contains(KeyModifiers::SHIFT) => {
            if state.is_connected() {
                state.open_import_dialog();
            } else {
                state.set_status("Not connected. Connect to a database first.");
            }
            KeyAction::Consumed
        }

        // Batch import: Shift+F7
        KeyCode::F(7) if modifiers.contains(KeyModifiers::SHIFT) => {
            if state.is_connected() && !state.schemas.is_empty() {
                state.open_batch_import_dialog();
            } else {
                state.set_status("Not connected or no tables. Connect to a database first.");
            }
            KeyAction::Consumed
        }

        // New tab: Ctrl+T
        KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.query_tabs.add_tab();
            state.active_panel = ActivePanel::QueryEditor;
            KeyAction::Consumed
        }

        // Switch to tab by number: Ctrl+1..9
        KeyCode::Char(c @ '1'..='9') if modifiers.contains(KeyModifiers::CONTROL) => {
            let tab_idx = (c as usize) - ('1' as usize);
            if tab_idx < state.query_tabs.tabs.len() {
                state.query_tabs.switch_to_tab(tab_idx);
                state.active_panel = ActivePanel::QueryEditor;
            }
            KeyAction::Consumed
        }

        // Save tabs: Ctrl+S
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.save_query_tabs();
            state.query_tabs.current_tab_mut().is_modified = false;
            state.set_status("Queries saved");
            KeyAction::Consumed
        }

        // New connection: Ctrl+N
        KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.open_new_connection_dialog();
            KeyAction::Consumed
        }

        // Refresh tables: Ctrl+R
        KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
            crate::handle_refresh_tables(state).await;
            KeyAction::Consumed
        }

        // Help
        KeyCode::Char('?') => {
            state.set_status(
                "Help: Tab=switch panels, Enter=connect/select, F5=execute, n=new, d=delete, q=quit",
            );
            KeyAction::Consumed
        }

        _ => KeyAction::NotHandled,
    }
}
