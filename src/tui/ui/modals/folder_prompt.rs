use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::tui::folders::FolderPromptKind;
use crate::tui::ui::widgets::draw_cursor;
use crate::tui::{AppState, DialogMode};

/// Area of the folder prompt: 60% wide, 6 lines high, centered.
pub fn folder_prompt_rect(area: Rect) -> Rect {
    let width = (area.width * 6 / 10).max(30).min(area.width);
    let height = 6.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Text input of a connection folder path (move, new, rename).
pub fn render_folder_prompt(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::FolderPrompt {
        return;
    }
    let Some(prompt) = &state.folder_prompt else {
        return;
    };
    let area = folder_prompt_rect(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(prompt.title())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let hint = match &prompt.kind {
        FolderPromptKind::MoveConnection(name) => {
            format!("Folder of {name} (A/B, empty = root):")
        }
        FolderPromptKind::NewFolder => "Folder path (A/B):".to_string(),
        FolderPromptKind::RenameFolder(_) => "New path (A/B):".to_string(),
    };
    let lines = vec![
        Line::from(Span::styled(hint, Style::default().fg(Color::Gray))),
        Line::from(Span::styled(
            format!("> {}", prompt.input),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Enter: confirm  Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);

    let x = inner.x + 2 + prompt.input[..prompt.cursor].chars().count() as u16;
    draw_cursor(frame, x, inner.y + 1);
}
