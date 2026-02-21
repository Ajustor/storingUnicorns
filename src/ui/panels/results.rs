use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{
        Block, Borders, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
        TableState,
    },
    Frame,
};

use super::common::{draw_cursor, highlight_style, panel_style};
use crate::ui::clickable::{ClickableRegistry, ClickableType};
use crate::ui::layout::PanelType;
use crate::services::{ActivePanel, AppState};

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
        // Split area: filter bar (if active or has content) + table
        let has_filter = state.results_filter_active || !state.results_filter.is_empty();
        let (filter_area, table_area) = if has_filter {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(3)])
                .split(area);
            (Some(chunks[0]), chunks[1])
        } else {
            (None, area)
        };

        // Render filter bar if active
        if let Some(filter_rect) = filter_area {
            let filter_style = if state.results_filter_active && is_active {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let filter_text = if state.results_filter.is_empty() && !state.results_filter_active {
                "🔍 Press / to filter...".to_string()
            } else {
                format!("🔍 {}", state.results_filter)
            };

            let filter_block = Paragraph::new(filter_text).style(filter_style).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(filter_style),
            );

            frame.render_widget(filter_block, filter_rect);

            // Show cursor in filter input
            if state.results_filter_active && is_active {
                // +1 border, +2 for 🔍 emoji (2 cells wide), +1 space = +4
                let cursor_x = filter_rect.x
                    + 4
                    + state.results_filter[..state.results_filter_cursor].len() as u16;
                let cursor_y = filter_rect.y + 1;
                draw_cursor(frame, cursor_x, cursor_y);
            }
        }

        // Use cached column widths (computed once when results change, not every frame)
        let col_widths = &state.cached_col_widths;

        // Apply horizontal scroll by skipping columns
        let col_offset = state.results_scroll_x;
        let visible_col_start = col_offset.min(result.columns.len().saturating_sub(1));

        // Calculate visible area for rows
        // Inner area: border (1) + header row (1) = start at y+2
        let inner_area = Rect {
            x: table_area.x + 1,
            y: table_area.y + 2, // +1 for border, +1 for header
            width: table_area.width.saturating_sub(2),
            height: table_area.height.saturating_sub(3), // -2 for borders, -1 for header
        };

        let visible_height = inner_area.height as usize;
        state.results_visible_height.set(visible_height);
        let scroll_offset = state.results_scroll;

        // Determine how many columns fit on screen to avoid rendering offscreen ones
        let available_width = inner_area.width as usize;
        let mut visible_col_end = visible_col_start;
        let mut used_width: usize = 0;
        for &w in col_widths.iter().skip(visible_col_start) {
            let col_total = w as usize + 1; // +1 for column_spacing
            if used_width + col_total > available_width + 1 && visible_col_end > visible_col_start {
                break;
            }
            used_width += col_total;
            visible_col_end += 1;
        }
        // Ensure at least one column is visible
        if visible_col_end == visible_col_start {
            visible_col_end = (visible_col_start + 1).min(result.columns.len());
        }
        let visible_col_range = visible_col_start..visible_col_end;

        // Build header with only visible columns
        let header_cells: Vec<&str> = result
            .columns
            .iter()
            .skip(visible_col_range.start)
            .take(visible_col_range.len())
            .map(|c| c.name.as_str())
            .collect();
        let header = Row::new(header_cells)
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .height(1);

        // Count filtered rows without allocating full vec when no filter
        let has_filter = !state.results_filter.is_empty();
        let filter_lower = state.results_filter.to_lowercase();

        let total_filtered_rows = if has_filter {
            result
                .rows
                .iter()
                .filter(|row| {
                    row.iter()
                        .any(|cell| cell.to_lowercase().contains(&filter_lower))
                })
                .count()
        } else {
            result.rows.len()
        };

        // Build rows: only visible slice, only visible columns, no String cloning
        let rows: Vec<Row> = if has_filter {
            result
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row.iter()
                        .any(|cell| cell.to_lowercase().contains(&filter_lower))
                })
                .skip(scroll_offset)
                .take(visible_height)
                .map(|(orig_idx, row)| {
                    let style = if orig_idx == state.selected_row
                        && is_active
                        && !state.results_filter_active
                    {
                        highlight_style()
                    } else {
                        Style::default()
                    };
                    let cells: Vec<&str> = row[visible_col_range.clone()]
                        .iter()
                        .map(|s| s.as_str())
                        .collect();
                    Row::new(cells).style(style)
                })
                .collect()
        } else {
            result
                .rows
                .iter()
                .enumerate()
                .skip(scroll_offset)
                .take(visible_height)
                .map(|(orig_idx, row)| {
                    let style = if orig_idx == state.selected_row
                        && is_active
                        && !state.results_filter_active
                    {
                        highlight_style()
                    } else {
                        Style::default()
                    };
                    let cells: Vec<&str> = row
                        .get(visible_col_range.clone())
                        .unwrap_or(&[])
                        .iter()
                        .map(|s| s.as_str())
                        .collect();
                    Row::new(cells).style(style)
                })
                .collect()
        };

        // Register clickable areas for visible result rows
        {
            let row_iter: Box<dyn Iterator<Item = usize>> = if has_filter {
                Box::new(
                    result
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| {
                            row.iter()
                                .any(|cell| cell.to_lowercase().contains(&filter_lower))
                        })
                        .map(|(i, _)| i)
                        .skip(scroll_offset)
                        .take(visible_height),
                )
            } else {
                Box::new(
                    scroll_offset
                        ..scroll_offset
                            + visible_height.min(result.rows.len().saturating_sub(scroll_offset)),
                )
            };
            for (display_idx, orig_idx) in row_iter.enumerate() {
                let row_rect = Rect {
                    x: inner_area.x,
                    y: inner_area.y + display_idx as u16,
                    width: inner_area.width,
                    height: 1,
                };
                registry.register(row_rect, ClickableType::ResultRow(orig_idx));
            }
        }

        // Use calculated widths for visible columns only
        let widths: Vec<Constraint> = col_widths
            .iter()
            .skip(visible_col_range.start)
            .take(visible_col_range.len())
            .map(|&w| Constraint::Length(w))
            .collect();

        let filter_indicator = if !state.results_filter.is_empty() {
            format!(
                " [filtered: {} of {}]",
                total_filtered_rows,
                result.rows.len()
            )
        } else {
            String::new()
        };

        let scroll_info = if result.columns.len() > 1 {
            format!(
                " [←/→ col {}/{}]",
                visible_col_start + 1,
                result.columns.len()
            )
        } else {
            String::new()
        };

        let row_scroll_info = if total_filtered_rows > visible_height {
            format!(
                " [{}-{}/{}]",
                scroll_offset + 1,
                (scroll_offset + visible_height).min(total_filtered_rows),
                total_filtered_rows
            )
        } else {
            String::new()
        };

        let title = format!(
            " Results ({} rows, {}ms){}{}{} [/ to filter] ",
            result.rows.len(),
            result.execution_time_ms,
            filter_indicator,
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
        let visible_selection = if has_filter {
            result
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row.iter()
                        .any(|cell| cell.to_lowercase().contains(&filter_lower))
                })
                .position(|(idx, _)| idx == state.selected_row)
                .unwrap_or(0)
                .saturating_sub(scroll_offset)
        } else {
            state.selected_row.saturating_sub(scroll_offset)
        };
        let mut table_state = TableState::default();
        table_state.select(Some(visible_selection));

        frame.render_stateful_widget(table, table_area, &mut table_state);

        // Render vertical scrollbar if rows overflow
        if total_filtered_rows > visible_height {
            let mut scrollbar_state =
                ScrollbarState::new(total_filtered_rows.saturating_sub(visible_height))
                    .position(scroll_offset);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓"))
                .track_symbol(Some("│"))
                .thumb_symbol("█");
            frame.render_stateful_widget(scrollbar, table_area, &mut scrollbar_state);
        }

        // Render horizontal scrollbar if columns overflow
        if result.columns.len() > 1 && visible_col_start > 0 {
            let mut h_scrollbar_state = ScrollbarState::new(result.columns.len().saturating_sub(1))
                .position(visible_col_start);
            let h_scrollbar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                .begin_symbol(Some("◄"))
                .end_symbol(Some("►"))
                .track_symbol(Some("─"))
                .thumb_symbol("█");
            frame.render_stateful_widget(h_scrollbar, table_area, &mut h_scrollbar_state);
        }
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
