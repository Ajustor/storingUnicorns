use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use super::centered_rect;
use crate::engine::services::export_import::PathCompletion;
use crate::tui::ui::widgets::draw_cursor;
use crate::tui::{AppState, DialogMode};

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
        "  (CSV files named {schema}.{table}.csv or {table}.csv will be imported)",
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
        let file_exists = crate::engine::services::export_import::find_batch_csv(
            std::path::Path::new(&batch.directory),
            schema,
            table,
            std::path::Path::exists,
        )
        .is_some();

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

/// Where the NoSQL `(o/N)` question is drawn.
pub fn nosql_confirm_rect(area: Rect) -> Rect {
    centered_rect(60, 30, area)
}

/// A MongoDB / Redis `(o/N)` question: destructive console commands,
/// emptying a collection / database, deleting a row or an index.
pub fn render_nosql_confirm(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::NoSqlConfirm {
        return;
    }
    let Some(confirm) = state.nosql.confirm.as_ref() else {
        return;
    };
    let area = nosql_confirm_rect(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Confirmation ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    frame.render_widget(block, area);
    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("⚠ {}", confirm.title),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))];
    let room = (inner.height as usize).saturating_sub(3);
    for line in confirm.lines.iter().take(room) {
        lines.push(Line::from(Span::styled(
            format!("  {line}"),
            Style::default().fg(Color::White),
        )));
    }
    if confirm.lines.len() > room {
        lines.push(Line::from(Span::styled(
            format!("  … et {} autre(s)", confirm.lines.len() - room),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            " o ",
            Style::default()
                .bg(Color::Red)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Oui  ", Style::default().fg(Color::Gray)),
        Span::styled(
            " N/Entrée/Échap ",
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Non", Style::default().fg(Color::Gray)),
    ]));
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// Where the NoSQL transfer modal is drawn.
pub fn nosql_transfer_rect(area: Rect) -> Rect {
    centered_rect(60, 70, area)
}

/// The MongoDB / Redis export / import modal: format, file or folder,
/// database, pattern / filter, import options, collections.
pub fn render_nosql_transfer_dialog(frame: &mut Frame, state: &AppState) {
    use crate::tui::nosql::transfer::{Field, Kind};

    if state.dialog_mode != DialogMode::NoSqlTransfer {
        return;
    }
    let Some(t) = state.nosql.transfer.as_ref() else {
        return;
    };
    let area = nosql_transfer_rect(frame.area());
    frame.render_widget(Clear, area);
    let title = match t.kind {
        Kind::Results => " Exporter le résultat ",
        Kind::Export => " Exporter ",
        Kind::Import => " Importer ",
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
    let style = |f: Field| {
        if t.active == f {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        }
    };
    let check = |on: bool| if on { "[x]" } else { "[ ]" };
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor: Option<(u16, u16)> = None;
    let mut list_start = None;
    for field in t.fields() {
        let (label, value) = match field {
            Field::Format => ("Format", format!("◀ {} ▶", t.format.label())),
            Field::Path if t.path_is_folder() => ("Dossier", t.path.clone()),
            Field::Path => ("Fichier", t.path.clone()),
            Field::Database => (
                "Base",
                format!(
                    "◀ {} ▶",
                    t.databases.get(t.database).map_or("-", String::as_str)
                ),
            ),
            Field::Pattern => ("Motif", t.text.clone()),
            Field::Filter => ("Filtre (une collection)", t.text.clone()),
            Field::Upsert => (
                if t.engine == crate::engine::models::DatabaseType::Redis {
                    "Écraser les clés existantes"
                } else {
                    "Remplacer les documents de même _id"
                },
                check(t.options.upsert).to_string(),
            ),
            Field::KeepStrings => ("Tout garder en texte", check(t.options.keep_strings).into()),
            Field::ParseDates => (
                "Reconnaître les dates ISO",
                check(t.options.parse_dates).into(),
            ),
            Field::List => {
                let n = t.items.iter().filter(|i| i.2).count();
                ("Collections", format!("{n}/{} choisie(s)", t.items.len()))
            }
        };
        if t.active == field {
            let text_cursor = match field {
                Field::Path => Some(t.path_cursor.min(t.path.len())),
                Field::Pattern | Field::Filter => Some(t.text_cursor.min(t.text.len())),
                _ => None,
            };
            if let Some(c) = text_cursor {
                let typed = if field == Field::Path {
                    &t.path
                } else {
                    &t.text
                };
                let x = label.chars().count() + 2 + typed[..c].chars().count();
                cursor = Some((x as u16, lines.len() as u16));
            }
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{label}: "), style(field)),
            Span::styled(value, style(field)),
        ]));
        if field == Field::List {
            list_start = Some(lines.len());
        }
    }
    // The collections, the cursor's line kept in view.
    if let Some(start) = list_start {
        let room = (inner.height as usize).saturating_sub(start + 3);
        let skip = t.selected.saturating_sub(room.saturating_sub(1));
        for (i, (db, coll, on)) in t.items.iter().enumerate().skip(skip).take(room) {
            let selected = i == t.selected && t.active == Field::List;
            let s = if selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("  {} {db}.{coll}", check(*on)),
                s,
            )));
        }
    }
    if t.format == crate::engine::nosql::TransferFormat::Csv
        && t.engine == crate::engine::models::DatabaseType::MongoDB
    {
        // Two lines: the modal is narrow.
        for part in crate::engine::nosql::CSV_NULL_HINT.split_inclusive("; ") {
            lines.push(Line::from(Span::styled(
                part.trim_end(),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    if let Some((done, total, ref current)) = t.progress {
        lines.push(Line::from(Span::styled(
            format!("En cours : {current} ({}/{total})", done + 1),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        let hint = match t.kind {
            Kind::Results => "←/→ format | Entrée exporter | Échap annuler",
            _ if t.fields().contains(&Field::List) => {
                "Tab champ | Espace cocher | a/n tout/rien | Entrée lancer | Échap annuler"
            }
            _ => "Tab champ | ←/→ changer | Entrée lancer | Échap annuler",
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some((x, y)) = cursor {
        draw_cursor(
            frame,
            (inner.x + x).min(inner.x + inner.width.saturating_sub(1)),
            inner.y + y,
        );
    }
}

#[cfg(test)]
mod nosql_tests {
    use super::*;
    use crate::engine::config::AppConfig;
    use crate::engine::models::{DatabaseType, SchemaInfo};
    use crate::tui::nosql::transfer::{Kind, Transfer};
    use crate::tui::nosql::{Confirm, ConfirmAction};
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(state: &AppState, draw: fn(&mut Frame, &AppState)) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn the_question_lists_the_destructive_commands() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.nosql.confirm = Some(Confirm {
            title: "1 commande(s) destructive(s) : exécuter ?".into(),
            lines: vec![
                "Supprime tous les documents de shop.users : db.users.deleteMany({})".into(),
            ],
            action: ConfirmAction::DeleteRow,
        });
        state.dialog_mode = DialogMode::NoSqlConfirm;
        let all = screen(&state, render_nosql_confirm);
        assert!(all.contains("destructive"), "{all}");
        assert!(
            all.contains("Supprime tous les documents de shop.users"),
            "{all}"
        );
        assert!(
            all.contains(" o ") && all.contains("N/Entrée/Échap"),
            "{all}"
        );
    }

    #[test]
    fn the_import_of_collections_shows_the_csv_options() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        let schemas = vec![SchemaInfo {
            name: "shop".into(),
            tables: vec!["users".into(), "orders".into()],
            expanded: true,
        }];
        let mut t = Transfer::batch(
            Kind::Import,
            DatabaseType::MongoDB,
            &schemas,
            &|_| false,
            None,
        );
        t.cycle_format(false); // JSON -> CSV
        state.nosql.transfer = Some(t);
        state.dialog_mode = DialogMode::NoSqlTransfer;
        let all = screen(&state, render_nosql_transfer_dialog);
        for label in [
            " Importer ",
            "Format: ◀ CSV ▶",
            "Dossier: .",
            "Remplacer les documents de même _id: [ ]",
            "Tout garder en texte: [ ]",
            "Reconnaître les dates ISO: [ ]",
            "[ ] shop.users",
            "[ ] shop.orders",
            "null et champ absent s'exportent tous deux vides",
            "valeur nulle",
        ] {
            assert!(all.contains(label), "{label} missing:\n{all}");
        }
    }

    #[test]
    fn the_redis_export_shows_its_database_and_pattern() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        let schemas: Vec<SchemaInfo> = (0..3)
            .map(|i| SchemaInfo {
                name: format!("db{i}"),
                tables: Vec::new(),
                expanded: false,
            })
            .collect();
        let t = Transfer::batch(
            Kind::Export,
            DatabaseType::Redis,
            &schemas,
            &|_| false,
            Some(("db2", None)),
        );
        state.nosql.transfer = Some(t);
        state.dialog_mode = DialogMode::NoSqlTransfer;
        let all = screen(&state, render_nosql_transfer_dialog);
        assert!(all.contains("Base: ◀ db2 ▶"), "{all}");
        assert!(all.contains("Motif: *"), "{all}");
        assert!(all.contains("Fichier: db2.json"), "{all}");
        assert!(!all.contains("Collections"), "{all}");
    }
}
