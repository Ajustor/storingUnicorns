use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::services::{AppState, DialogMode};
use crate::ui::widgets::draw_cursor;

pub fn render_export_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::Export {
        return;
    }

    let Some(ref export_state) = state.export_state else {
        return;
    };

    let area = centered_rect(50, 30, frame.area());

    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Export Data (Enter to export, Esc to cancel) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line> = Vec::new();

    // Row count info
    if let Some(ref result) = state.query_result {
        lines.push(Line::from(vec![
            Span::styled("Rows to export: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", result.rows.len()),
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(""));
    }

    // Format field
    let format_style = if export_state.active_field == 0 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("Format:  ", format_style),
        Span::styled(
            format!("◄ {} ►", export_state.format.label()),
            if export_state.active_field == 0 {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            },
        ),
    ]));
    lines.push(Line::from(""));

    // File path field
    let path_style = if export_state.active_field == 1 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("File:    ", path_style),
        Span::styled(
            &export_state.file_path,
            if export_state.active_field == 1 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tab: switch field | ←/→: change format | Enter: export",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Show cursor on file path field
    if export_state.active_field == 1 {
        let cursor_x = inner.x
            + 9 // "File:    " length
            + export_state.file_path[..export_state.cursor_position].len() as u16;
        let cursor_y = inner.y + 4; // line index of file path
        draw_cursor(frame, cursor_x, cursor_y);
    }
}

pub fn render_import_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::Import {
        return;
    }

    let Some(ref import_state) = state.import_state else {
        return;
    };

    let area = centered_rect(50, 30, frame.area());

    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Import CSV (Enter to import, Esc to cancel) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line> = Vec::new();

    // File path field
    let path_style = if import_state.active_field == 0 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("CSV File:  ", path_style),
        Span::styled(
            &import_state.file_path,
            if import_state.active_field == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));
    lines.push(Line::from(""));

    // Target table field
    let table_style = if import_state.active_field == 1 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("Table:     ", table_style),
        Span::styled(
            &import_state.target_table,
            if import_state.active_field == 1 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tab: switch field | Enter: import | Esc: cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Show cursor on active field
    match import_state.active_field {
        0 => {
            let cursor_x = inner.x
                + 11 // "CSV File:  " length
                + import_state.file_path[..import_state.cursor_position].len() as u16;
            let cursor_y = inner.y;
            draw_cursor(frame, cursor_x, cursor_y);
        }
        1 => {
            let cursor_x = inner.x
                + 11 // "Table:     " length
                + import_state.target_table[..import_state.cursor_position].len() as u16;
            let cursor_y = inner.y + 2;
            draw_cursor(frame, cursor_x, cursor_y);
        }
        _ => {}
    }
}
