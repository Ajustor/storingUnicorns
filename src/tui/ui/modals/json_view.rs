//! `v` on a MongoDB / Redis result: the selected document or value,
//! indented, read only, scrolled with ↑/↓, PgUp/PgDn.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::tui::{AppState, DialogMode};

/// Where the JSON view is drawn.
pub fn json_view_rect(area: Rect) -> Rect {
    centered_rect(70, 80, area)
}

pub fn render_json_view(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::JsonView {
        return;
    }
    let Some(view) = state.nosql.json.as_ref() else {
        return;
    };
    let area = json_view_rect(frame.area());
    frame.render_widget(Clear, area);
    let inner_height = area.height.saturating_sub(2) as usize;
    let total = view.lines.len();
    let position = if total > inner_height {
        format!(
            " [{}-{}/{}]",
            view.scroll + 1,
            (view.scroll + inner_height).min(total),
            total
        )
    } else {
        String::new()
    };
    let block = Block::default()
        .title(format!(
            " {}{position} (↑/↓ défiler, Échap fermer) ",
            view.title
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let lines: Vec<Line> = view
        .lines
        .iter()
        .skip(view.scroll)
        .take(inner_height)
        .map(|l| Line::from(l.as_str()))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::AppConfig;
    use crate::tui::nosql::JsonView;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn the_view_shows_its_lines_from_the_scroll() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.nosql.json = Some(JsonView {
            title: "Document de shop.users".into(),
            lines: (0..100).map(|i| format!("ligne {i}")).collect(),
            scroll: 10,
        });
        state.dialog_mode = DialogMode::JsonView;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render_json_view(f, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Document de shop.users [11-"), "{screen}");
        assert!(screen.contains("ligne 10"), "{screen}");
        assert!(!screen.contains("ligne 9 "), "{screen}");
    }
}
