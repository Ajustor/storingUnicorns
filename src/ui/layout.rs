use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::services::AppState;

use super::clickable::ClickableRegistry;
use super::modals::{render_edit_row_dialog, render_new_connection_dialog, render_schema_dialog};
use super::widgets::{
    render_connections_panel, render_help_bar, render_query_editor, render_results_panel,
    render_status_bar, render_tables_panel,
};

/// Panel types for click detection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelType {
    Connections,
    Tables,
    QueryEditor,
    Results,
}

/// Main UI layout - DataGrip-like interface
/// ┌─────────────┬─────────────────────────────────┐
/// │ Connections │  Query Editor                   │
/// │             │                                 │
/// ├─────────────┤                                 │
/// │ Tables      │                                 │
/// │             ├─────────────────────────────────┤
/// │             │  Results                        │
/// │             │                                 │
/// └─────────────┴─────────────────────────────────┘
/// │ Status Bar                                    │
/// └───────────────────────────────────────────────┘
/// │ Help Bar                                      │
/// └───────────────────────────────────────────────┘
pub fn render_ui(frame: &mut Frame, state: &AppState, clickable_registry: &ClickableRegistry) {
    // Clear previous clickable areas
    clickable_registry.clear();

    let size = frame.area();

    // Main vertical split: content + status bar + help bar
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),   // Content
            Constraint::Length(1), // Status bar
            Constraint::Length(1), // Help bar
        ])
        .split(size);

    // Content area: left sidebar + right main area
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(50)])
        .split(main_chunks[0]);

    // Left sidebar: connections + tables
    let sidebar_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(content_chunks[0]);

    // Right side: query editor + results
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(content_chunks[1]);

    // Render all panels
    render_connections_panel(frame, sidebar_chunks[0], state, clickable_registry);
    render_tables_panel(frame, sidebar_chunks[1], state, clickable_registry);
    render_query_editor(frame, right_chunks[0], state, clickable_registry);
    render_results_panel(frame, right_chunks[1], state, clickable_registry);
    render_status_bar(frame, main_chunks[1], state);
    render_help_bar(frame, main_chunks[2], state);

    // Render dialog overlay if active
    render_new_connection_dialog(frame, state);
    render_edit_row_dialog(frame, state);
    render_schema_dialog(frame, state);
}
