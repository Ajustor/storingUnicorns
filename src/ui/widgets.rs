use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table},
    Frame,
};

use crate::services::{ActivePanel, AppState};

fn panel_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn highlight_style() -> Style {
    Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

pub fn render_connections_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_active = state.active_panel == ActivePanel::Connections;

    let items: Vec<ListItem> = state
        .config
        .connections
        .iter()
        .enumerate()
        .map(|(i, conn)| {
            let style = if i == state.selected_connection && is_active {
                highlight_style()
            } else {
                Style::default()
            };

            let connected_marker = if state.current_connection_config.as_ref().map(|c| &c.name) == Some(&conn.name) {
                "● "
            } else {
                "  "
            };

            ListItem::new(format!("{}{} ({})", connected_marker, conn.name, conn.db_type)).style(style)
        })
        .collect();

    let title = if state.config.connections.is_empty() {
        " Connections (none) "
    } else {
        " Connections "
    };

    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(panel_style(is_active)),
    );

    frame.render_widget(list, area);
}

pub fn render_tables_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_active = state.active_panel == ActivePanel::Tables;

    let items: Vec<ListItem> = state
        .tables
        .iter()
        .enumerate()
        .map(|(i, table)| {
            let style = if i == state.selected_table && is_active {
                highlight_style()
            } else {
                Style::default()
            };
            ListItem::new(format!("  {}", table)).style(style)
        })
        .collect();

    let title = if state.is_connected() {
        " Tables "
    } else {
        " Tables (not connected) "
    };

    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(panel_style(is_active)),
    );

    frame.render_widget(list, area);
}

pub fn render_query_editor(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_active = state.active_panel == ActivePanel::QueryEditor;

    let input_text = if state.query_input.is_empty() && !is_active {
        "-- Enter SQL query here..."
    } else {
        &state.query_input
    };

    let paragraph = Paragraph::new(input_text)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .title(" Query Editor [F5 to execute] ")
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        );

    frame.render_widget(paragraph, area);

    // Show cursor when editing
    if is_active {
        let cursor_x = area.x + 1 + state.cursor_position as u16;
        let cursor_y = area.y + 1;
        frame.set_cursor_position((cursor_x.min(area.x + area.width - 2), cursor_y));
    }
}

pub fn render_results_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_active = state.active_panel == ActivePanel::Results;

    if let Some(ref result) = state.query_result {
        // Build header
        let header_cells: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
        let header = Row::new(header_cells)
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .height(1);

        // Build rows
        let rows: Vec<Row> = result
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let style = if i == state.selected_row && is_active {
                    highlight_style()
                } else {
                    Style::default()
                };
                Row::new(row.clone()).style(style)
            })
            .collect();

        // Calculate column widths (equal distribution for now)
        let col_count = result.columns.len().max(1);
        let widths: Vec<ratatui::layout::Constraint> = (0..col_count)
            .map(|_| ratatui::layout::Constraint::Percentage((100 / col_count) as u16))
            .collect();

        let title = format!(
            " Results ({} rows, {}ms) ",
            result.rows.len(),
            result.execution_time_ms
        );

        let table = Table::new(rows, widths)
            .header(header)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(panel_style(is_active)),
            );

        frame.render_widget(table, area);
    } else {
        let paragraph = Paragraph::new("No results yet. Execute a query with F5.")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .title(" Results ")
                    .borders(Borders::ALL)
                    .border_style(panel_style(is_active)),
            );

        frame.render_widget(paragraph, area);
    }
}

pub fn render_status_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let connection_info = if let Some(ref config) = state.current_connection_config {
        format!("Connected: {} ({}) | ", config.name, config.db_type)
    } else {
        String::from("Disconnected | ")
    };

    let status = Line::from(vec![
        Span::styled(connection_info, Style::default().fg(Color::Green)),
        Span::styled(&state.status_message, Style::default().fg(Color::White)),
    ]);

    let paragraph = Paragraph::new(status).style(Style::default().bg(Color::DarkGray));

    frame.render_widget(paragraph, area);
}
