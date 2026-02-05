use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Row, Table, TableState},
    Frame,
};

use super::clickable::{ClickableRegistry, ClickableType};
use super::layout::PanelType;
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

pub fn render_connections_panel(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    registry: &ClickableRegistry,
) {
    let is_active = state.active_panel == ActivePanel::Connections && !state.is_dialog_open();

    // Register panel area
    registry.register(area, ClickableType::Panel(PanelType::Connections));

    // Calculate inner area (excluding borders)
    let inner_area = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let visible_height = inner_area.height as usize;
    let scroll_offset = state.connections_scroll;

    // Register each visible connection item
    for (display_idx, (i, _conn)) in state
        .config
        .connections
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
        .enumerate()
    {
        let item_rect = Rect {
            x: inner_area.x,
            y: inner_area.y + display_idx as u16,
            width: inner_area.width,
            height: 1,
        };
        registry.register(item_rect, ClickableType::Connection(i));
    }

    // Build items with scroll offset
    let items: Vec<ListItem> = state
        .config
        .connections
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
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

    let total = state.config.connections.len();
    let title = if total == 0 {
        " Connections (none) ".to_string()
    } else if total > visible_height {
        format!(
            " Connections [{}-{}/{}] ",
            scroll_offset + 1,
            (scroll_offset + visible_height).min(total),
            total
        )
    } else {
        " Connections ".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        )
        .highlight_style(highlight_style());

    // Adjust selection for scroll offset
    let visible_selection = state.selected_connection.saturating_sub(scroll_offset);
    let mut list_state = ListState::default();
    list_state.select(Some(visible_selection));

    frame.render_stateful_widget(list, area, &mut list_state);
}

pub fn render_tables_panel(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    registry: &ClickableRegistry,
) {
    let is_active = state.active_panel == ActivePanel::Tables && !state.is_dialog_open();

    // Register panel area
    registry.register(area, ClickableType::Panel(PanelType::Tables));

    // Calculate inner area (excluding borders)
    let inner_area = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let visible_height = inner_area.height as usize;
    let scroll_offset = state.tables_scroll;

    // First, build all items to get total count
    let mut all_items: Vec<(ListItem, Option<(usize, Option<usize>)>)> = Vec::new();

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
        all_items.push((
            ListItem::new(format!(
                "{} {} ({})",
                expand_icon,
                schema.name,
                schema.tables.len()
            ))
            .style(schema_style),
            Some((schema_idx, None)), // Schema click info
        ));

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
                all_items.push((
                    ListItem::new(format!("    {}", table)).style(table_style),
                    Some((schema_idx, Some(table_idx))), // Table click info
                ));
            }
        }
    }

    let total_items = all_items.len();

    // Apply scroll and take visible items
    let visible_items: Vec<_> = all_items
        .into_iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
        .collect();

    // Register clickable areas for visible items
    for (display_idx, (_, (_, click_info))) in visible_items.iter().enumerate() {
        if let Some((schema_idx, table_idx_opt)) = click_info {
            let item_rect = Rect {
                x: inner_area.x,
                y: inner_area.y + display_idx as u16,
                width: inner_area.width,
                height: 1,
            };
            if let Some(table_idx) = table_idx_opt {
                registry.register(
                    item_rect,
                    ClickableType::Table {
                        schema_idx: *schema_idx,
                        table_idx: *table_idx,
                    },
                );
            } else {
                registry.register(item_rect, ClickableType::Schema(*schema_idx));
            }
        }
    }

    // Extract just the ListItems
    let items: Vec<ListItem> = visible_items
        .into_iter()
        .map(|(_, (item, _))| item)
        .collect();

    // Fallback to flat table list if no schemas
    let items = if state.schemas.is_empty() && !state.tables.is_empty() {
        state
            .tables
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_height)
            .map(|(i, table)| {
                let style = if i == state.selected_table && is_active {
                    highlight_style()
                } else {
                    Style::default()
                };
                ListItem::new(format!("  {}", table)).style(style)
            })
            .collect()
    } else {
        items
    };

    let title = if !state.is_connected() {
        " Tables (not connected) ".to_string()
    } else if total_items > visible_height {
        format!(
            " Tables [{}-{}/{}] ",
            scroll_offset + 1,
            (scroll_offset + visible_height).min(total_items),
            total_items
        )
    } else {
        " Tables ".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        )
        .highlight_style(highlight_style());

    // Calculate selected index in flat list and adjust for scroll
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
    let visible_selection = selected_idx.saturating_sub(scroll_offset);

    let mut list_state = ListState::default();
    list_state.select(Some(visible_selection));

    frame.render_stateful_widget(list, area, &mut list_state);
}

pub fn render_query_editor(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    registry: &ClickableRegistry,
) {
    let is_active = state.active_panel == ActivePanel::QueryEditor && !state.is_dialog_open();

    // Register panel area
    registry.register(area, ClickableType::Panel(PanelType::QueryEditor));

    // Register inner editor area for cursor positioning
    let inner_area = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    registry.register(inner_area, ClickableType::QueryEditor);

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
                .title(" Query Editor [F5/Ctrl+Enter] ")
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        );

    frame.render_widget(paragraph, area);

    // Show cursor when editing (calculate position with real line breaks)
    if is_active {
        let inner_width = area.width.saturating_sub(2) as usize; // Account for borders
        if inner_width > 0 {
            // Calculate cursor position considering actual newlines
            let text_before_cursor =
                &state.query_input[..state.cursor_position.min(state.query_input.len())];

            let mut visual_line = 0;
            let mut visual_col = 0;

            for c in text_before_cursor.chars() {
                if c == '\n' {
                    visual_line += 1;
                    visual_col = 0;
                } else {
                    visual_col += 1;
                    // Handle wrapping
                    if visual_col >= inner_width {
                        visual_line += 1;
                        visual_col = 0;
                    }
                }
            }

            let cursor_x = area.x + 1 + visual_col as u16;
            let cursor_y = area.y + 1 + visual_line as u16;

            // Only show cursor if it's within the visible area
            if cursor_y < area.y + area.height - 1 {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }
}

pub fn render_results_panel(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    registry: &ClickableRegistry,
) {
    let is_active = state.active_panel == ActivePanel::Results && !state.is_dialog_open();

    // Register panel area
    registry.register(area, ClickableType::Panel(PanelType::Results));

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

        // Calculate visible area for rows
        // Inner area: border (1) + header row (1) = start at y+2
        let inner_area = Rect {
            x: area.x + 1,
            y: area.y + 2, // +1 for border, +1 for header
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(3), // -2 for borders, -1 for header
        };

        let visible_height = inner_area.height as usize;
        let scroll_offset = state.results_scroll;
        let total_rows = result.rows.len();

        // Build rows with column offset and vertical scroll
        let rows: Vec<Row> = result
            .rows
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_height)
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

        // Register clickable areas for visible result rows
        for (display_idx, (i, _)) in result
            .rows
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_height)
            .enumerate()
        {
            let row_rect = Rect {
                x: inner_area.x,
                y: inner_area.y + display_idx as u16,
                width: inner_area.width,
                height: 1,
            };
            registry.register(row_rect, ClickableType::ResultRow(i));
        }

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

        let row_scroll_info = if total_rows > visible_height {
            format!(
                " [{}-{}/{}]",
                scroll_offset + 1,
                (scroll_offset + visible_height).min(total_rows),
                total_rows
            )
        } else {
            String::new()
        };

        let title = format!(
            " Results ({} rows, {}ms){}{} [Enter to edit] ",
            result.rows.len(),
            result.execution_time_ms,
            row_scroll_info,
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

        // Adjust selection for scroll offset
        let visible_selection = state.selected_row.saturating_sub(scroll_offset);
        let mut table_state = TableState::default();
        table_state.select(Some(visible_selection));

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
                vec![
                    ("F5", "Execute all"),
                    ("Ctrl+↵", "Execute current"),
                    ("Tab", "Next panel"),
                ]
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
