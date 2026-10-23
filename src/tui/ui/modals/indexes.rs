//! `s` on a MongoDB collection: its indexes (`n` new, `d` drop, `r`
//! reload).

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::tui::nosql::index_line;
use crate::tui::{AppState, DialogMode};

/// Where the indexes dialog is drawn.
pub fn indexes_rect(area: Rect) -> Rect {
    centered_rect(70, 60, area)
}

pub fn render_indexes_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::Indexes {
        return;
    }
    let Some(view) = state.nosql.indexes.as_ref() else {
        return;
    };
    let area = indexes_rect(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            " Index de {} (n nouveau, d supprimer, r recharger, Échap fermer) ",
            view.coll
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &view.error {
        lines.push(Line::from(Span::styled(
            format!("Erreur : {e}"),
            Style::default().fg(Color::Red),
        )));
    } else if view.indexes.is_empty() {
        lines.push(Line::from(Span::styled(
            "Aucun index",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let height = area.height.saturating_sub(2 + lines.len() as u16) as usize;
    let skip = view.selected.saturating_sub(height.saturating_sub(1));
    for (i, index) in view.indexes.iter().enumerate().skip(skip).take(height) {
        let style = if i == view.selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(index_line(index), style)));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::AppConfig;
    use crate::engine::nosql::mongo::{CollectionRef, MongoIndex};
    use crate::tui::nosql::IndexesView;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(state: &AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| render_indexes_dialog(f, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn indexes_are_listed_with_their_options_or_the_error() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        let coll = CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        };
        state.nosql.indexes = Some(IndexesView {
            coll: coll.clone(),
            indexes: vec![MongoIndex {
                name: "email_1".into(),
                keys: bson::doc! { "email": 1 },
                unique: true,
                sparse: false,
                ttl: Some(60),
                partial: None,
                hidden: false,
            }],
            selected: 0,
            error: None,
        });
        state.dialog_mode = DialogMode::Indexes;
        let all = screen(&state);
        assert!(all.contains("Index de shop.users"), "{all}");
        assert!(all.contains("email_1"), "{all}");
        assert!(all.contains("unique, TTL 60 s"), "{all}");

        state.nosql.indexes.as_mut().unwrap().error = Some("non implémenté".into());
        state.nosql.indexes.as_mut().unwrap().indexes.clear();
        let all = screen(&state);
        assert!(all.contains("Erreur : non implémenté"), "{all}");
    }
}
