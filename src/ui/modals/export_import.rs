use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use super::centered_rect;
use crate::services::export_import::PathCompletion;
use crate::services::{AppState, DialogMode};
use crate::ui::widgets::draw_cursor;

/// Render a path completion dropdown at the given position.
/// `x` and `y` are the top-left of where the dropdown should appear (below the field).
/// `max_width` limits the popup width. `max_height` limits how many items to show.
fn render_path_completion(
    frame: &mut Frame,
    completion: &PathCompletion,
    x: u16,
    y: u16,
    max_width: u16,
    max_height: u16,
) {
    if !completion.active || completion.suggestions.is_empty() {
        return;
    }

    let count = completion.suggestions.len().min(max_height as usize);
    let height = count as u16 + 2; // +2 for border
    let width = max_width.min(
        completion
            .suggestions
            .iter()
            .map(|s| s.len() as u16 + 2)
            .max()
            .unwrap_or(10)
            + 4,
    );

    let popup_area = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Tab: cycle | Enter: accept | Esc: close ");

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Compute scroll offset to keep selected item visible
    let scroll_offset = if completion.selected_index >= count {
        completion.selected_index - count + 1
    } else {
        0
    };

    let lines: Vec<Line> = completion
        .suggestions
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(count)
        .map(|(i, suggestion)| {
            let style = if i == completion.selected_index {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            // Truncate if needed
            let display = if suggestion.len() > inner.width as usize {
                format!("{}…", &suggestion[..inner.width as usize - 1])
            } else {
                suggestion.clone()
            };
            Line::from(Span::styled(display, style))
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

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
        "Tab: auto complete | ←/→: change format | Enter: export",
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

        // Show path completion dropdown
        render_path_completion(
            frame,
            &export_state.path_completion,
            inner.x,
            cursor_y + 1,
            inner.width,
            8,
        );
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
        "Tab: auto complete | Enter: import | Esc: cancel",
        Style::default().fg(Color::DarkGray),
    )));

    // Show import progress if active
    if let Some((current, total)) = import_state.import_progress {
        lines.push(Line::from(""));
        let progress_pct = if total > 0 {
            (current as f64 / total as f64 * 100.0) as u16
        } else {
            0
        };
        let bar_width = inner.width.saturating_sub(20) as usize;
        let filled = (bar_width as f64 * current as f64 / total.max(1) as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        let bar = format!(
            "  [{}{}] {}/{} ({}%)",
            "█".repeat(filled),
            "░".repeat(empty),
            current,
            total,
            progress_pct
        );
        lines.push(Line::from(Span::styled(
            format!("  Importing..."),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            bar,
            Style::default().fg(Color::Green),
        )));
    }

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

            // Show path completion dropdown
            render_path_completion(
                frame,
                &import_state.path_completion,
                inner.x,
                cursor_y + 1,
                inner.width,
                8,
            );
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

pub fn render_batch_export_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::BatchExport {
        return;
    }

    let Some(ref batch) = state.batch_export_state else {
        return;
    };

    let area = centered_rect(60, 70, frame.area());
    frame.render_widget(Clear, area);

    let selected_count = batch.tables.iter().filter(|(_, _, s)| *s).count();
    let title = format!(
        " Batch Export ({} selected) — Enter: export, Space: toggle, a: all, Esc: cancel ",
        selected_count
    );
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

    let mut lines: Vec<Line> = Vec::new();

    // Format field
    let format_style = if batch.active_field == 0 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("Format:    ", format_style),
        Span::styled(
            format!("◄ {} ►", batch.format.label()),
            if batch.active_field == 0 {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            },
        ),
    ]));

    // Directory field
    let dir_style = if batch.active_field == 1 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("Directory: ", dir_style),
        Span::styled(
            &batch.directory,
            if batch.active_field == 1 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));

    lines.push(Line::from(""));

    // Table list header
    let list_header_style = if batch.active_field == 2 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(Span::styled("Tables:", list_header_style)));

    // Render table list area
    let list_start_y = 4; // lines above
    let list_height = inner.height.saturating_sub(list_start_y as u16 + 3) as usize; // reserve space for progress/help

    let visible_tables: Vec<_> = batch
        .tables
        .iter()
        .enumerate()
        .skip(batch.scroll_offset)
        .take(list_height)
        .collect();

    for (i, (schema, table, selected)) in &visible_tables {
        let checkbox = if *selected { "[x]" } else { "[ ]" };
        let is_highlighted = *i == batch.selected_index && batch.active_field == 2;
        let style = if is_highlighted {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if *selected {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} {}.{}", checkbox, schema, table),
            style,
        )));
    }

    // Pad remaining lines
    for _ in visible_tables.len()..list_height {
        lines.push(Line::from(""));
    }

    // Progress bar
    if let Some((current, total, ref table_name)) = batch.progress {
        lines.push(Line::from(""));
        let progress_pct = if total > 0 {
            (current as f64 / total as f64 * 100.0) as u16
        } else {
            0
        };
        let bar_width = inner.width.saturating_sub(20) as usize;
        let filled = (bar_width as f64 * current as f64 / total.max(1) as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        lines.push(Line::from(Span::styled(
            format!("  Exporting: {} ({}/{})", table_name, current, total),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  [{}{}] {}%",
                "█".repeat(filled),
                "░".repeat(empty),
                progress_pct
            ),
            Style::default().fg(Color::Green),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Tab: auto complete | Space: toggle | a: all | n: none | Enter: export",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Scrollbar for table list
    if batch.tables.len() > list_height {
        let scrollbar_area = Rect {
            x: inner.x + inner.width - 1,
            y: inner.y + list_start_y as u16,
            width: 1,
            height: list_height as u16,
        };
        let mut scrollbar_state =
            ScrollbarState::new(batch.tables.len().saturating_sub(list_height))
                .position(batch.scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    // Show cursor on directory field
    if batch.active_field == 1 {
        let cursor_x = inner.x
            + 11 // "Directory: " length
            + batch.directory[..batch.cursor_position].len() as u16;
        let cursor_y = inner.y + 1;
        draw_cursor(frame, cursor_x, cursor_y);

        // Show path completion dropdown
        render_path_completion(
            frame,
            &batch.path_completion,
            inner.x,
            cursor_y + 1,
            inner.width,
            8,
        );
    }
}

pub fn render_batch_import_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::BatchImport {
        return;
    }

    let Some(ref batch) = state.batch_import_state else {
        return;
    };

    let area = centered_rect(60, 70, frame.area());
    frame.render_widget(Clear, area);

    let selected_count = batch.tables.iter().filter(|(_, _, s)| *s).count();
    let title = format!(
        " Batch Import ({} selected) — Enter: import, Space: toggle, a: all, Esc: cancel ",
        selected_count
    );
    let block = Block::default()
        .title(title)
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

    // Directory field
    let dir_style = if batch.active_field == 0 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(vec![
        Span::styled("Directory: ", dir_style),
        Span::styled(
            &batch.directory,
            if batch.active_field == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));

    lines.push(Line::from(Span::styled(
        "  (CSV files named {table}.csv will be imported)",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    // Table list header
    let list_header_style = if batch.active_field == 1 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    lines.push(Line::from(Span::styled("Tables:", list_header_style)));

    // Render table list area
    let list_start_y = 4; // lines above
    let list_height = inner.height.saturating_sub(list_start_y as u16 + 3) as usize;

    let visible_tables: Vec<_> = batch
        .tables
        .iter()
        .enumerate()
        .skip(batch.scroll_offset)
        .take(list_height)
        .collect();

    for (i, (schema, table, selected)) in &visible_tables {
        let checkbox = if *selected { "[x]" } else { "[ ]" };
        let is_highlighted = *i == batch.selected_index && batch.active_field == 1;

        // Check if corresponding CSV file exists
        let clean_name = crate::services::export_import::BatchExportState::clean_table_name(table);
        let csv_path = std::path::Path::new(&batch.directory).join(format!("{}.csv", clean_name));
        let file_exists = csv_path.exists();

        let style = if is_highlighted {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if *selected && file_exists {
            Style::default().fg(Color::Green)
        } else if *selected && !file_exists {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::White)
        };

        let file_indicator = if file_exists { " ✓" } else { " ✗" };
        lines.push(Line::from(Span::styled(
            format!(" {} {}.{}{}", checkbox, schema, table, file_indicator),
            style,
        )));
    }

    // Pad remaining lines
    for _ in visible_tables.len()..list_height {
        lines.push(Line::from(""));
    }

    // Progress bar
    if let Some((current, total, ref table_name)) = batch.progress {
        lines.push(Line::from(""));
        let progress_pct = if total > 0 {
            (current as f64 / total as f64 * 100.0) as u16
        } else {
            0
        };
        let bar_width = inner.width.saturating_sub(20) as usize;
        let filled = (bar_width as f64 * current as f64 / total.max(1) as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        lines.push(Line::from(Span::styled(
            format!("  Importing: {} ({}/{})", table_name, current, total),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  [{}{}] {}%",
                "█".repeat(filled),
                "░".repeat(empty),
                progress_pct
            ),
            Style::default().fg(Color::Green),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Tab: auto complete | Space: toggle | a: all | n: none | Enter: import",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Scrollbar for table list
    if batch.tables.len() > list_height {
        let scrollbar_area = Rect {
            x: inner.x + inner.width - 1,
            y: inner.y + list_start_y as u16,
            width: 1,
            height: list_height as u16,
        };
        let mut scrollbar_state =
            ScrollbarState::new(batch.tables.len().saturating_sub(list_height))
                .position(batch.scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    // Show cursor on directory field
    if batch.active_field == 0 {
        let cursor_x = inner.x
            + 11 // "Directory: " length
            + batch.directory[..batch.cursor_position].len() as u16;
        let cursor_y = inner.y;
        draw_cursor(frame, cursor_x, cursor_y);

        // Show path completion dropdown
        render_path_completion(
            frame,
            &batch.path_completion,
            inner.x,
            cursor_y + 1,
            inner.width,
            8,
        );
    }
}
pub fn render_delete_row_confirm(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::DeleteRowConfirm {
        return;
    }

    let area = centered_rect(50, 20, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Delete Row ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 2,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(3),
    };

    let table_name = state.editing_table_name.as_deref().unwrap_or("unknown");

    let lines = vec![
        Line::from(Span::styled(
            format!("Delete selected row from {}?", table_name),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " y/Enter ",
                Style::default()
                    .bg(Color::Red)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Confirm  ", Style::default().fg(Color::Gray)),
            Span::styled(
                " n/Esc ",
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Cancel", Style::default().fg(Color::Gray)),
        ]),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

pub fn render_truncate_confirm(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::TruncateConfirm {
        return;
    }

    let area = centered_rect(50, 20, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Truncate Table ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 2,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(3),
    };

    let table_name = state.truncate_table_name.as_deref().unwrap_or("unknown");

    let lines = vec![
        Line::from(Span::styled(
            "⚠ This will delete ALL data from:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  {}", table_name),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " y/Enter ",
                Style::default()
                    .bg(Color::Red)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Confirm  ", Style::default().fg(Color::Gray)),
            Span::styled(
                " n/Esc ",
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Cancel", Style::default().fg(Color::Gray)),
        ]),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

pub fn render_batch_truncate_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::BatchTruncate {
        return;
    }

    let Some(ref batch) = state.batch_truncate_state else {
        return;
    };

    let area = centered_rect(60, 70, frame.area());
    frame.render_widget(Clear, area);

    let selected_count = batch.tables.iter().filter(|(_, _, s)| *s).count();
    let title = format!(
        " Batch Truncate ({} selected) — Enter: truncate, Space: toggle, Esc: cancel ",
        selected_count
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line> = Vec::new();

    // Warning
    lines.push(Line::from(Span::styled(
        "⚠ Select tables to DELETE ALL DATA from:",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Table list header
    lines.push(Line::from(Span::styled(
        "Tables:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    // Render table list area
    let list_start_y = 3; // lines above
    let list_height = inner.height.saturating_sub(list_start_y as u16 + 3) as usize;

    let visible_tables: Vec<_> = batch
        .tables
        .iter()
        .enumerate()
        .skip(batch.scroll_offset)
        .take(list_height)
        .collect();

    for (i, (schema, table, selected)) in &visible_tables {
        let checkbox = if *selected { "[x]" } else { "[ ]" };
        let is_highlighted = *i == batch.selected_index;
        let style = if is_highlighted {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if *selected {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} {}.{}", checkbox, schema, table),
            style,
        )));
    }

    // Pad remaining lines
    for _ in visible_tables.len()..list_height {
        lines.push(Line::from(""));
    }

    // Progress bar
    if let Some((current, total, ref table_name)) = batch.progress {
        lines.push(Line::from(""));
        let progress_pct = if total > 0 {
            (current as f64 / total as f64 * 100.0) as u16
        } else {
            0
        };
        let bar_width = inner.width.saturating_sub(20) as usize;
        let filled = (bar_width as f64 * current as f64 / total.max(1) as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        lines.push(Line::from(Span::styled(
            format!("  Truncating: {} ({}/{})", table_name, current, total),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  [{}{}] {}%",
                "█".repeat(filled),
                "░".repeat(empty),
                progress_pct
            ),
            Style::default().fg(Color::Red),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Space: toggle | a: all | n: none | Enter: truncate | Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Scrollbar for table list
    if batch.tables.len() > list_height {
        let scrollbar_area = Rect {
            x: inner.x + inner.width - 1,
            y: inner.y + list_start_y as u16,
            width: 1,
            height: list_height as u16,
        };
        let mut scrollbar_state =
            ScrollbarState::new(batch.tables.len().saturating_sub(list_height))
                .position(batch.scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}