use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::tui::{AppState, DialogMode};
use crate::tui::ui::widgets::draw_cursor;

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

    // MongoDB / Redis: the document / element of a collection / key.
    let nosql = crate::tui::nosql::engine(state).is_some();
    let target = state.editing_table_name.as_deref().unwrap_or("");
    let title = match (nosql, state.dialog_mode == DialogMode::AddRow) {
        (true, true) => {
            format!(" Ajouter à {target} (Tab champ suivant, Entrée enregistrer, Échap annuler) ")
        }
        (true, false) => format!(
            " Modifier dans {target} (Tab champ suivant, Entrée enregistrer, Échap annuler) "
        ),
        (false, true) => {
            " Add Row (Tab to switch fields, Enter to save, Esc to cancel) ".to_string()
        }
        (false, false) => {
            " Edit Row (Tab to switch fields, Enter to save, Esc to cancel) ".to_string()
        }
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
        let required_indicator = if !col.nullable && !is_system && !nosql {
            "*"
        } else {
            ""
        };
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
            draw_cursor(frame, cursor_x.min(chunk.x + chunk.width - 1), cursor_y);
        }
    }
}

/// Where a NoSQL form (new index, new key, SCAN pattern) is drawn.
pub fn nosql_form_rect(area: Rect) -> Rect {
    centered_rect(60, 40, area)
}

/// A NoSQL form: one line per field, `label: value`, the hint dimmed while
/// the value is empty; ←/→ cycle the choice fields.
pub fn render_nosql_form(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::NoSqlForm {
        return;
    }
    let Some(form) = state.nosql.form.as_ref() else {
        return;
    };
    let area = nosql_form_rect(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            " {} (Tab champ suivant, Entrée valider, Échap annuler) ",
            form.title
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(block, area);
    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    for (i, field) in form.fields.iter().enumerate() {
        let y = inner.y + 2 * i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let active = i == form.active;
        let style = Style::default().fg(if active { Color::Cyan } else { Color::Gray });
        let hint = if field.label == "Valeur" {
            form.value_hint()
        } else {
            field.hint
        };
        let mut spans = vec![
            Span::styled(format!("{}: ", field.label), style),
            Span::styled(field.value.clone(), style),
        ];
        if !field.choices.is_empty() && active {
            spans.push(Span::styled(" (←/→)", Style::default().fg(Color::DarkGray)));
        } else if field.value.is_empty() && !hint.is_empty() {
            spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
        }
        let line = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), line);
        if active && field.choices.is_empty() {
            let x = inner.x + field.label.chars().count() as u16 + 2;
            let cursor = field.value[..form.cursor.min(field.value.len())]
                .chars()
                .count() as u16;
            draw_cursor(frame, (x + cursor).min(inner.x + inner.width - 1), y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::AppConfig;
    use crate::tui::nosql::Form;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn the_new_key_form_hints_the_value_of_its_type() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        let mut form = Form::new_key(3);
        form.fields[1].value = "hash".into();
        state.nosql.form = Some(form);
        state.dialog_mode = DialogMode::NoSqlForm;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render_nosql_form(f, &state)).unwrap();
        let all: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(all.contains("Nouvelle clé dans db3"), "{all}");
        assert!(all.contains("Type: hash"), "{all}");
        assert!(all.contains(r#"Valeur: {"champ": "valeur"}"#), "{all}");
        assert!(all.contains("TTL (s): persistante"), "{all}");
    }
}
