use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

use crate::services::AppState;

use super::widgets::{
    render_connections_panel, render_query_editor, render_results_panel, render_status_bar,
    render_tables_panel,
};

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
pub fn render_ui(frame: &mut Frame, state: &AppState) {
    let size = frame.area();

    // Main vertical split: content + status bar
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
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
    render_connections_panel(frame, sidebar_chunks[0], state);
    render_tables_panel(frame, sidebar_chunks[1], state);
    render_query_editor(frame, right_chunks[0], state);
    render_results_panel(frame, right_chunks[1], state);
    render_status_bar(frame, main_chunks[1], state);
}

/// Helper to create a centered popup area
#[allow(dead_code)]
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
