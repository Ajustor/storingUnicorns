use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use super::common::{draw_cursor, panel_style, selection_style};
use crate::ui::clickable::{ClickableRegistry, ClickableType};
use crate::ui::layout::PanelType;
use crate::services::{ActivePanel, AppState};

/// Highlight SQL with text selection overlay
fn highlight_sql_with_selection(
    text: &str,
    known_columns: &[String],
    sel_start: usize,
    sel_end: usize,
) -> Vec<Line<'static>> {
    use crate::ui::sql_highlight;

    // Get the base highlighted content
    let base_lines = sql_highlight::highlight_sql(text, known_columns);

    // If no selection or invalid range, return base
    if sel_start >= sel_end || sel_start >= text.len() {
        return base_lines;
    }

    // Apply selection highlighting character by character
    let mut result_lines: Vec<Line<'static>> = Vec::new();
    let mut char_idx = 0;

    for line in base_lines {
        let mut new_spans: Vec<Span<'static>> = Vec::new();

        for span in line.spans {
            let span_text = span.content.to_string();
            let span_start = char_idx;
            let span_end = char_idx + span_text.len();

            // Check if this span overlaps with selection
            if span_end <= sel_start || span_start >= sel_end {
                // No overlap, keep original span
                new_spans.push(Span::styled(span_text, span.style));
            } else {
                // Has overlap, split the span
                let overlap_start = sel_start.max(span_start);
                let overlap_end = sel_end.min(span_end);

                // Before selection
                if overlap_start > span_start {
                    let before = &span_text[..(overlap_start - span_start)];
                    if !before.is_empty() {
                        new_spans.push(Span::styled(before.to_string(), span.style));
                    }
                }

                // Selected part
                let selected_start = overlap_start - span_start;
                let selected_end = overlap_end - span_start;
                let selected = &span_text[selected_start..selected_end];
                if !selected.is_empty() {
                    new_spans.push(Span::styled(selected.to_string(), selection_style()));
                }

                // After selection
                if overlap_end < span_end {
                    let after = &span_text[(overlap_end - span_start)..];
                    if !after.is_empty() {
                        new_spans.push(Span::styled(after.to_string(), span.style));
                    }
                }
            }

            char_idx = span_end;
        }

        // Account for newline
        char_idx += 1;

        result_lines.push(Line::from(new_spans));
    }

    result_lines
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

    // Split area: tab bar (1 line) + editor content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    let tab_area = chunks[0];
    let editor_area = chunks[1];

    // Render tab bar and register clickable areas for each tab
    let tabs = &state.query_tabs.tabs;
    let active_tab = state.query_tabs.active_tab;

    let mut tab_spans: Vec<Span> = vec![];
    let mut current_x = tab_area.x;

    for (i, tab) in tabs.iter().enumerate() {
        let modified_marker = if tab.is_modified { "*" } else { "" };
        let tab_name = format!(" {}{} ", tab.name, modified_marker);
        let tab_width = tab_name.len() as u16;

        // Register clickable area for this tab
        let tab_rect = Rect {
            x: current_x,
            y: tab_area.y,
            width: tab_width,
            height: 1,
        };
        registry.register(tab_rect, ClickableType::QueryTab(i));
        current_x += tab_width;

        if i == active_tab {
            tab_spans.push(Span::styled(
                tab_name,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(tab_name, Style::default().fg(Color::DarkGray)));
        }
        if i < tabs.len() - 1 {
            tab_spans.push(Span::raw("│"));
            current_x += 1; // separator width
        }
    }
    // Add hint for tab shortcuts
    tab_spans.push(Span::styled(
        " [Ctrl+1-9:Switch Ctrl+T:New Ctrl+W:Close]",
        Style::default().fg(Color::DarkGray),
    ));

    let tab_line = Line::from(tab_spans);
    let tab_bar = Paragraph::new(tab_line).style(Style::default().bg(Color::Black));
    frame.render_widget(tab_bar, tab_area);

    // Register inner editor area for cursor positioning
    let inner_area = Rect {
        x: editor_area.x + 1,
        y: editor_area.y + 1,
        width: editor_area.width.saturating_sub(2),
        height: editor_area.height.saturating_sub(2),
    };
    registry.register(inner_area, ClickableType::QueryEditor);

    let query_input = state.query_input();
    let cursor_position = state.cursor_position();

    // Use syntax highlighting with selection support
    let highlighted_content = if query_input.is_empty() && !is_active {
        vec![Line::from(Span::styled(
            "-- Enter SQL query here...",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ))]
    } else {
        // Apply selection highlighting if there's a selection
        if let Some((sel_start, sel_end)) = state.get_selection_range() {
            highlight_sql_with_selection(query_input, &state.known_columns, sel_start, sel_end)
        } else {
            crate::ui::sql_highlight::highlight_sql(query_input, &state.known_columns)
        }
    };

    let title = if state.has_selection() {
        " Query Editor [Ctrl+C:Copy | Ctrl+X:Cut | Ctrl+A:Select All] "
    } else {
        " Query Editor [F5/Ctrl+Enter | Ctrl+Space:Complete | Shift+Arrow:Select] "
    };

    let paragraph = Paragraph::new(highlighted_content.clone())
        .wrap(ratatui::widgets::Wrap { trim: false })
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(panel_style(is_active)),
        );

    // Single pass: compute cursor position, total visual lines, and editor scroll
    let inner_height = editor_area.height.saturating_sub(2) as usize;
    let inner_width = editor_area.width.saturating_sub(2) as usize;

    let (editor_scroll, total_visual_lines, cursor_visual_line, cursor_visual_col) =
        if inner_width > 0 && inner_height > 0 {
            let cursor_byte = cursor_position.min(query_input.len());
            let mut visual_line: usize = 0;
            let mut visual_col: usize = 0;
            let mut byte_offset: usize = 0;
            let mut cursor_vline: usize = 0;
            let mut cursor_vcol: usize = 0;
            let mut found_cursor = false;

            for c in query_input.chars() {
                // Record cursor position when we reach its byte offset
                if !found_cursor && byte_offset >= cursor_byte {
                    cursor_vline = visual_line;
                    cursor_vcol = visual_col;
                    found_cursor = true;
                }

                if c == '\n' {
                    visual_line += 1;
                    visual_col = 0;
                } else {
                    visual_col += 1;
                    if visual_col >= inner_width {
                        visual_line += 1;
                        visual_col = 0;
                    }
                }

                byte_offset += c.len_utf8();
            }

            // Handle cursor at end of text
            if !found_cursor {
                cursor_vline = visual_line;
                cursor_vcol = visual_col;
            }

            let total_lines = visual_line + 1;
            let scroll = if cursor_vline >= inner_height {
                cursor_vline - inner_height + 1
            } else {
                0
            };

            (scroll, total_lines, cursor_vline, cursor_vcol)
        } else {
            (0, 1, 0, 0)
        };

    let paragraph = paragraph.scroll((editor_scroll as u16, 0));

    frame.render_widget(paragraph, editor_area);

    // Render scrollbar for editor if content overflows
    if total_visual_lines > inner_height {
        let mut scrollbar_state =
            ScrollbarState::new(total_visual_lines.saturating_sub(inner_height))
                .position(editor_scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        frame.render_stateful_widget(scrollbar, editor_area, &mut scrollbar_state);
    }

    // Render completion popup if active
    if is_active && state.show_completion && !state.completion_suggestions.is_empty() {
        render_completion_popup(frame, editor_area, state, cursor_position, query_input);
    }

    // Show cursor when editing
    if is_active && !state.results_filter_active && !state.tables_filter_active && inner_width > 0 {
        let adjusted_line = cursor_visual_line.saturating_sub(editor_scroll);
        let cursor_x = editor_area.x + 1 + cursor_visual_col as u16;
        let cursor_y = editor_area.y + 1 + adjusted_line as u16;

        // Only show cursor if it's within the visible area
        if cursor_y < editor_area.y + editor_area.height - 1 && cursor_y >= editor_area.y + 1 {
            draw_cursor(frame, cursor_x, cursor_y);
        }
    }
}

/// Render the autocompletion popup
fn render_completion_popup(
    frame: &mut Frame,
    editor_area: Rect,
    state: &AppState,
    cursor_position: usize,
    query_input: &str,
) {
    // Calculate popup position based on cursor
    let inner_width = editor_area.width.saturating_sub(2) as usize;
    let text_before_cursor = &query_input[..cursor_position.min(query_input.len())];

    let mut visual_line = 0;
    let mut visual_col = 0;

    for c in text_before_cursor.chars() {
        if c == '\n' {
            visual_line += 1;
            visual_col = 0;
        } else {
            visual_col += 1;
            if visual_col >= inner_width {
                visual_line += 1;
                visual_col = 0;
            }
        }
    }

    // Position popup below cursor
    let popup_x = editor_area.x + 1 + visual_col as u16;
    let popup_y = editor_area.y + 2 + visual_line as u16;

    // Calculate popup dimensions
    let max_width = state
        .completion_suggestions
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(10) as u16
        + 4;
    let popup_width = max_width.min(30);
    let popup_height = (state.completion_suggestions.len() as u16 + 2).min(8);

    // Ensure popup fits on screen
    let popup_x = popup_x.min(editor_area.x + editor_area.width - popup_width - 1);
    let popup_y = if popup_y + popup_height > editor_area.y + editor_area.height {
        // Show above cursor if not enough space below
        (editor_area.y + 1 + visual_line as u16).saturating_sub(popup_height)
    } else {
        popup_y
    };

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    // Build completion list items
    let items: Vec<Line> = state
        .completion_suggestions
        .iter()
        .enumerate()
        .map(|(i, suggestion)| {
            let style = if i == state.completion_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(format!(" {} ", suggestion), style))
        })
        .collect();

    let popup = Paragraph::new(items)
        .block(
            Block::default()
                .title(" Completions ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .style(Style::default().bg(Color::Black));

    frame.render_widget(popup, popup_area);
}
