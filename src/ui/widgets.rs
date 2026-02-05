use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    },
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

            let connected_marker =
                if state.current_connection_config.as_ref().map(|c| &c.name) == Some(&conn.name) {
                    "● "
                } else {
                    "  "
                };

            ListItem::new(format!(
                "{}{} ({})",
                connected_marker, conn.name, conn.db_type
            ))
            .style(style)
        })
        .collect();

    let title = if state.config.connections.is_empty() {
        " Connections (none) "
    } else {
        " Connections "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        )
        .highlight_style(highlight_style());

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_connection));

    frame.render_stateful_widget(list, area, &mut list_state);
}

pub fn render_tables_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_active = state.active_panel == ActivePanel::Tables && !state.is_dialog_open();

    let mut items: Vec<ListItem> = Vec::new();

    for (schema_idx, schema) in state.schemas.iter().enumerate() {
        // Schema header
        let is_schema_selected =
            schema_idx == state.selected_schema && state.selected_table == 0 && is_active;
        let expand_icon = if schema.expanded { "▼" } else { "▶" };
        let schema_style = if is_schema_selected {
            highlight_style()
        } else {
            Style::default().fg(Color::Yellow)
        };
        items.push(
            ListItem::new(format!(
                "{} {} ({})",
                expand_icon,
                schema.name,
                schema.tables.len()
            ))
            .style(schema_style),
        );

        // Tables under this schema (if expanded)
        if schema.expanded {
            for (table_idx, table) in schema.tables.iter().enumerate() {
                let is_table_selected = schema_idx == state.selected_schema
                    && table_idx + 1 == state.selected_table
                    && is_active;
                let table_style = if is_table_selected {
                    highlight_style()
                } else {
                    Style::default()
                };
                items.push(ListItem::new(format!("    {}", table)).style(table_style));
            }
        }
    }

    // Fallback to flat table list if no schemas
    if state.schemas.is_empty() && !state.tables.is_empty() {
        items = state
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
    }

    let title = if state.is_connected() {
        " Tables "
    } else {
        " Tables (not connected) "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        )
        .highlight_style(highlight_style());

    // Calculate selected index in flat list
    let mut selected_idx = 0;
    for (schema_idx, schema) in state.schemas.iter().enumerate() {
        if schema_idx == state.selected_schema {
            selected_idx += state.selected_table;
            break;
        }
        selected_idx += 1; // schema header
        if schema.expanded {
            selected_idx += schema.tables.len();
        }
    }

    let mut list_state = ListState::default();
    list_state.select(Some(selected_idx));

    frame.render_stateful_widget(list, area, &mut list_state);
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
        .wrap(ratatui::widgets::Wrap { trim: false })
        .block(
            Block::default()
                .title(" Query Editor [F5 to execute] ")
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        );

    frame.render_widget(paragraph, area);

    // Show cursor when editing (calculate position with wrapping)
    if is_active {
        let inner_width = area.width.saturating_sub(2) as usize; // Account for borders
        if inner_width > 0 {
            let cursor_line = state.cursor_position / inner_width;
            let cursor_col = state.cursor_position % inner_width;
            let cursor_x = area.x + 1 + cursor_col as u16;
            let cursor_y = area.y + 1 + cursor_line as u16;
            // Only show cursor if it's within the visible area
            if cursor_y < area.y + area.height - 1 {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }
}

pub fn render_results_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_active = state.active_panel == ActivePanel::Results && !state.is_dialog_open();

    // Show connection error if any
    if let Some(ref error) = state.connection_error {
        let error_text = format!("❌ {}", error);
        let paragraph = Paragraph::new(error_text)
            .style(Style::default().fg(Color::Red))
            .block(
                Block::default()
                    .title(" Results - Error ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            );
        frame.render_widget(paragraph, area);
        return;
    }

    // Show connecting loader
    if state.is_connecting {
        let paragraph = Paragraph::new("⏳ Connecting to database...")
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .title(" Results ")
                    .borders(Borders::ALL)
                    .border_style(panel_style(is_active)),
            );
        frame.render_widget(paragraph, area);
        return;
    }

    if let Some(ref result) = state.query_result {
        // Calculate column widths: min(30, max_content_length)
        let col_widths: Vec<u16> = result
            .columns
            .iter()
            .enumerate()
            .map(|(col_idx, col)| {
                // Start with column name length
                let mut max_len = col.name.len();
                // Check all rows for max content length
                for row in &result.rows {
                    if let Some(cell) = row.get(col_idx) {
                        max_len = max_len.max(cell.len());
                    }
                }
                // Min between 30 and max_len, with minimum of 5
                (max_len.min(30).max(5)) as u16
            })
            .collect();

        // Apply horizontal scroll by skipping columns
        let col_offset = state.results_scroll_x;
        let visible_col_start = col_offset.min(result.columns.len().saturating_sub(1));

        // Build header with column offset
        let header_cells: Vec<&str> = result
            .columns
            .iter()
            .skip(visible_col_start)
            .map(|c| c.name.as_str())
            .collect();
        let header = Row::new(header_cells)
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .height(1);

        // Build rows with column offset
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
                let cells: Vec<String> = row.iter().skip(visible_col_start).cloned().collect();
                Row::new(cells).style(style)
            })
            .collect();

        // Use calculated widths with offset
        let widths: Vec<Constraint> = col_widths
            .iter()
            .skip(visible_col_start)
            .map(|&w| Constraint::Length(w))
            .collect();

        let scroll_info = if result.columns.len() > 1 {
            format!(
                " [←/→ col {}/{}]",
                visible_col_start + 1,
                result.columns.len()
            )
        } else {
            String::new()
        };

        let title = format!(
            " Results ({} rows, {}ms){} [Enter to edit] ",
            result.rows.len(),
            result.execution_time_ms,
            scroll_info
        );

        let table = Table::new(rows, widths)
            .header(header)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(panel_style(is_active)),
            )
            .row_highlight_style(highlight_style())
            .column_spacing(1);

        let mut table_state = TableState::default();
        table_state.select(Some(state.selected_row));

        frame.render_stateful_widget(table, area, &mut table_state);
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
    let connection_info = if state.is_connecting {
        String::from("⏳ Connecting... | ")
    } else if let Some(ref config) = state.current_connection_config {
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
            ActivePanel::QueryEditor => {
                vec![("F5", "Execute"), ("Tab", "Next panel"), ("Ctrl+Q", "Quit")]
            }
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
                Span::styled(format!("{} ", desc), Style::default().fg(Color::Gray)),
                Span::raw(" "),
            ]
        })
        .collect();

    let help_line = Line::from(spans);
    let paragraph = Paragraph::new(help_line);

    frame.render_widget(paragraph, area);
}

pub fn render_new_connection_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::NewConnection
        && state.dialog_mode != DialogMode::EditConnection
    {
        return;
    }

    let area = centered_rect(60, 70, frame.area());

    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let title = if state.dialog_mode == DialogMode::EditConnection {
        " Edit Connection "
    } else {
        " New Connection "
    };

    let block = Block::default()
        .title(title)
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
    let render_field = |frame: &mut Frame,
                        area: Rect,
                        label: &str,
                        value: &str,
                        field: ConnectionField,
                        is_password: bool| {
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
        let paragraph = Paragraph::new(content).style(style).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(style),
        );

        frame.render_widget(paragraph, area);

        // Show cursor for active text field
        if is_active && field != ConnectionField::DbType {
            let cursor_x = area.x + label.len() as u16 + 2 + nc.cursor_position as u16;
            let cursor_y = area.y;
            frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), cursor_y));
        }
    };

    render_field(
        frame,
        chunks[0],
        "Name",
        &nc.name,
        ConnectionField::Name,
        false,
    );

    // DB Type - special handling with cycle indicator
    let db_type_active = nc.active_field == ConnectionField::DbType;
    let db_style = if db_type_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };
    let db_hint = if db_type_active {
        " (←/→ to change)"
    } else {
        ""
    };
    let db_content = format!("Type: {}{}", nc.db_type, db_hint);
    let db_paragraph = Paragraph::new(db_content).style(db_style).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(db_style),
    );
    frame.render_widget(db_paragraph, chunks[1]);

    render_field(
        frame,
        chunks[2],
        "Host",
        &nc.host,
        ConnectionField::Host,
        false,
    );
    render_field(
        frame,
        chunks[3],
        "Port",
        &nc.port,
        ConnectionField::Port,
        false,
    );
    render_field(
        frame,
        chunks[4],
        "Username",
        &nc.username,
        ConnectionField::Username,
        false,
    );
    render_field(
        frame,
        chunks[5],
        "Password",
        &nc.password,
        ConnectionField::Password,
        true,
    );
    render_field(
        frame,
        chunks[6],
        "Database",
        &nc.database,
        ConnectionField::Database,
        false,
    );
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

pub fn render_edit_row_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::EditRow && state.dialog_mode != DialogMode::AddRow {
        return;
    }

    let Some(ref editing_row) = state.editing_row else {
        return;
    };

    let Some(ref result) = state.query_result else {
        return;
    };

    let area = centered_rect(70, 80, frame.area());

    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let title = if state.dialog_mode == DialogMode::AddRow {
        " Add Row (Tab to switch fields, Enter to save, Esc to cancel) "
    } else {
        " Edit Row (Tab to switch fields, Enter to save, Esc to cancel) "
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    // Calculate how many fields we can show
    let visible_fields = (inner.height as usize).saturating_sub(1);
    let total_fields = result.columns.len();

    // Calculate scroll offset to keep current field visible
    let scroll_offset = if state.editing_column >= visible_fields {
        state.editing_column - visible_fields + 1
    } else {
        0
    };

    let constraints: Vec<Constraint> = (0..visible_fields.min(total_fields))
        .map(|_| Constraint::Length(2))
        .chain(std::iter::once(Constraint::Min(0)))
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (display_idx, chunk) in chunks.iter().enumerate() {
        let field_idx = display_idx + scroll_offset;
        if field_idx >= total_fields {
            break;
        }

        let col_name = &result.columns[field_idx].name;
        let value = editing_row.get(field_idx).map(|s| s.as_str()).unwrap_or("");
        let is_active = field_idx == state.editing_column;
        let is_system = state.is_system_column(field_idx);

        // Style based on active state and system column
        let style = if is_system {
            Style::default().fg(Color::DarkGray)
        } else if is_active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };

        // Add indicators for required and system columns
        let col = &result.columns[field_idx];
        let required_indicator = if !col.nullable && !is_system { "*" } else { "" };
        let system_indicator = if is_system { " [auto]" } else { "" };

        // Build styled content
        let spans = vec![
            Span::styled(col_name.clone(), style),
            Span::styled(required_indicator, Style::default().fg(Color::Red)),
            Span::styled(system_indicator, Style::default().fg(Color::DarkGray)),
            Span::styled(": ", style),
            Span::styled(value, style),
        ];
        let paragraph = Paragraph::new(Line::from(spans));

        frame.render_widget(paragraph, *chunk);

        // Show cursor for active field (only if not a system column in add mode)
        if is_active && !(is_system && state.dialog_mode == DialogMode::AddRow) {
            let cursor_x = chunk.x
                + col_name.len() as u16
                + system_indicator.len() as u16
                + 2
                + state.editing_cursor as u16;
            let cursor_y = chunk.y;
            frame.set_cursor_position((cursor_x.min(chunk.x + chunk.width - 1), cursor_y));
        }
    }
}
