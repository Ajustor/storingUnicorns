use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table},
    Frame,
};

use crate::services::{ActivePanel, AppState, ConnectionField, DialogMode};

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
    let is_active = state.active_panel == ActivePanel::Connections && !state.is_dialog_open();

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
    let is_active = state.active_panel == ActivePanel::Tables && !state.is_dialog_open();

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
    let is_active = state.active_panel == ActivePanel::QueryEditor && !state.is_dialog_open();

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
    let is_active = state.active_panel == ActivePanel::Results && !state.is_dialog_open();

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
        let widths: Vec<Constraint> = (0..col_count)
            .map(|_| Constraint::Percentage((100 / col_count) as u16))
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

pub fn render_help_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let help_items = if state.is_dialog_open() {
        vec![
            ("Tab", "Next field"),
            ("Enter", "Save"),
            ("Esc", "Cancel"),
            ("←/→", "Cycle type"),
        ]
    } else {
        match state.active_panel {
            ActivePanel::Connections => vec![
                ("Enter", "Connect"),
                ("n", "New"),
                ("d", "Delete"),
                ("Tab", "Next panel"),
                ("?", "Help"),
                ("q", "Quit"),
            ],
            ActivePanel::Tables => vec![
                ("Enter", "Select"),
                ("Ctrl+R", "Refresh"),
                ("Tab", "Next panel"),
                ("?", "Help"),
                ("q", "Quit"),
            ],
            ActivePanel::QueryEditor => vec![
                ("F5", "Execute"),
                ("Tab", "Next panel"),
                ("Ctrl+Q", "Quit"),
            ],
            ActivePanel::Results => vec![
                ("↑/↓", "Navigate"),
                ("Tab", "Next panel"),
                ("?", "Help"),
                ("q", "Quit"),
            ],
        }
    };

    let spans: Vec<Span> = help_items
        .iter()
        .flat_map(|(key, desc)| {
            vec![
                Span::styled(
                    format!(" {} ", key),
                    Style::default().bg(Color::DarkGray).fg(Color::White),
                ),
                Span::styled(
                    format!("{} ", desc),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw(" "),
            ]
        })
        .collect();

    let help_line = Line::from(spans);
    let paragraph = Paragraph::new(help_line);

    frame.render_widget(paragraph, area);
}

pub fn render_new_connection_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::NewConnection {
        return;
    }

    let area = centered_rect(60, 70, frame.area());
    
    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" New Connection ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name
            Constraint::Length(3), // DB Type
            Constraint::Length(3), // Host
            Constraint::Length(3), // Port
            Constraint::Length(3), // Username
            Constraint::Length(3), // Password
            Constraint::Length(3), // Database
            Constraint::Min(1),    // Spacer
        ])
        .split(inner);

    let nc = &state.new_connection;

    // Helper to render a field
    let render_field = |frame: &mut Frame, area: Rect, label: &str, value: &str, field: ConnectionField, is_password: bool| {
        let is_active = nc.active_field == field;
        let display_value = if is_password && !value.is_empty() {
            "*".repeat(value.len())
        } else {
            value.to_string()
        };

        let style = if is_active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };

        let content = format!("{}: {}", label, display_value);
        let paragraph = Paragraph::new(content)
            .style(style)
            .block(Block::default().borders(Borders::BOTTOM).border_style(style));

        frame.render_widget(paragraph, area);

        // Show cursor for active text field
        if is_active && field != ConnectionField::DbType {
            let cursor_x = area.x + label.len() as u16 + 2 + nc.cursor_position as u16;
            let cursor_y = area.y;
            frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), cursor_y));
        }
    };

    render_field(frame, chunks[0], "Name", &nc.name, ConnectionField::Name, false);
    
    // DB Type - special handling with cycle indicator
    let db_type_active = nc.active_field == ConnectionField::DbType;
    let db_style = if db_type_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };
    let db_hint = if db_type_active { " (←/→ to change)" } else { "" };
    let db_content = format!("Type: {}{}", nc.db_type, db_hint);
    let db_paragraph = Paragraph::new(db_content)
        .style(db_style)
        .block(Block::default().borders(Borders::BOTTOM).border_style(db_style));
    frame.render_widget(db_paragraph, chunks[1]);

    render_field(frame, chunks[2], "Host", &nc.host, ConnectionField::Host, false);
    render_field(frame, chunks[3], "Port", &nc.port, ConnectionField::Port, false);
    render_field(frame, chunks[4], "Username", &nc.username, ConnectionField::Username, false);
    render_field(frame, chunks[5], "Password", &nc.password, ConnectionField::Password, true);
    render_field(frame, chunks[6], "Database", &nc.database, ConnectionField::Database, false);
}

/// Helper to create a centered popup area
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
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
