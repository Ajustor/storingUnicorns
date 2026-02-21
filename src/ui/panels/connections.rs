use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{
        Block, Borders, List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use super::common::{highlight_style, panel_style};
use crate::services::{ActivePanel, AppState};
use crate::ui::clickable::{ClickableRegistry, ClickableType};
use crate::ui::layout::PanelType;

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

    // Render scrollbar if content overflows
    if total > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(total.saturating_sub(visible_height)).position(scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}
