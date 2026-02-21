use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{
        Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use super::common::{draw_cursor, highlight_style, panel_style};
use crate::services::{ActivePanel, AppState};
use crate::ui::clickable::{ClickableRegistry, ClickableType};
use crate::ui::layout::PanelType;

pub fn render_tables_panel(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    registry: &ClickableRegistry,
) {
    let is_active = state.active_panel == ActivePanel::Tables && !state.is_dialog_open();

    // Register panel area
    registry.register(area, ClickableType::Panel(PanelType::Tables));

    // Split area: filter bar (if active or has content) + list
    let has_filter = state.tables_filter_active || !state.tables_filter.is_empty();
    let (filter_area, list_area) = if has_filter {
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
        let filter_style = if state.tables_filter_active && is_active {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let filter_text = if state.tables_filter.is_empty() && !state.tables_filter_active {
            "🔍 Press / to filter...".to_string()
        } else {
            format!("🔍 {}", state.tables_filter)
        };

        let filter_block = Paragraph::new(filter_text).style(filter_style).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(filter_style),
        );

        frame.render_widget(filter_block, filter_rect);

        // Show cursor in filter input
        if state.tables_filter_active && is_active {
            // +1 border, +2 for 🔍 emoji (2 cells wide), +1 space = +4
            let cursor_x =
                filter_rect.x + 4 + state.tables_filter[..state.tables_filter_cursor].len() as u16;
            let cursor_y = filter_rect.y + 1;
            draw_cursor(frame, cursor_x, cursor_y);
        }
    }

    // Calculate inner area (excluding borders)
    let inner_area = Rect {
        x: list_area.x + 1,
        y: list_area.y + 1,
        width: list_area.width.saturating_sub(2),
        height: list_area.height.saturating_sub(2),
    };

    let visible_height = inner_area.height as usize;
    let scroll_offset = state.tables_scroll;

    // Get filtered schemas and tables
    let filtered_schemas = state.get_filtered_schemas();

    // Build all items from filtered schemas
    let mut all_items: Vec<(ListItem, Option<(usize, Option<usize>)>)> = Vec::new();

    for (schema_idx, schema, filtered_tables) in &filtered_schemas {
        // Schema header
        let is_schema_selected = *schema_idx == state.selected_schema
            && state.selected_table == 0
            && is_active
            && !state.tables_filter_active;
        let expand_icon = if schema.expanded { "▼" } else { "▶" };
        let schema_style = if is_schema_selected {
            highlight_style()
        } else {
            Style::default().fg(Color::Yellow)
        };

        let table_count = if state.tables_filter.is_empty() {
            schema.tables.len()
        } else {
            filtered_tables.len()
        };

        all_items.push((
            ListItem::new(format!("{} {} ({})", expand_icon, schema.name, table_count))
                .style(schema_style),
            Some((*schema_idx, None)),
        ));

        // Tables under this schema (if expanded)
        if schema.expanded {
            for (table_idx, table) in filtered_tables {
                let is_table_selected = *schema_idx == state.selected_schema
                    && *table_idx + 1 == state.selected_table
                    && is_active
                    && !state.tables_filter_active;
                let table_style = if is_table_selected {
                    highlight_style()
                } else {
                    Style::default()
                };
                all_items.push((
                    ListItem::new(format!("    {}", table)).style(table_style),
                    Some((*schema_idx, Some(*table_idx))),
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

    let filter_indicator = if !state.tables_filter.is_empty() {
        format!(" [filtered: {}]", state.tables_filter)
    } else {
        String::new()
    };

    let title = if !state.is_connected() {
        " Tables (not connected) ".to_string()
    } else if total_items > visible_height {
        format!(
            " Tables [{}-{}/{}]{} ",
            scroll_offset + 1,
            (scroll_offset + visible_height).min(total_items),
            total_items,
            filter_indicator
        )
    } else if !filter_indicator.is_empty() {
        format!(" Tables{} ", filter_indicator)
    } else {
        " Tables [/ to filter] ".to_string()
    };

    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(panel_style(is_active)),
    );

    frame.render_widget(list, list_area);

    // Render scrollbar if content overflows
    if total_items > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(total_items.saturating_sub(visible_height)).position(scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        frame.render_stateful_widget(scrollbar, list_area, &mut scrollbar_state);
    }
}
