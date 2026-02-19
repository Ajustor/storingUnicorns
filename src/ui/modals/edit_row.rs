use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::services::{AppState, DialogMode};

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

    // Calculate how many fields we can show (each field takes 2 lines)
    let visible_fields = (inner.height as usize).saturating_sub(1) / 2;
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
                + required_indicator.len() as u16
                + system_indicator.len() as u16
                + 2
                + state.editing_cursor as u16;
            let cursor_y = chunk.y;
            frame.set_cursor_position((cursor_x.min(chunk.x + chunk.width - 1), cursor_y));
        }
    }
}
