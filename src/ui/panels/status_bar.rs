use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::services::{ActivePanel, AppState};

pub fn render_status_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let connection_info = if state.is_connecting {
        String::from("⏳ Connecting... | ")
    } else if let Some(ref config) = state.current_connection_config {
        format!("Connected: {} ({}) | ", config.name, config.db_type)
    } else {
        String::from("Disconnected | ")
    };

    let status = Line::from(vec![
        Span::styled(connection_info, Style::default().fg(Color::Green)),
        Span::styled(&state.status_message, Style::default().fg(Color::White)),
    ]);

    let paragraph = Paragraph::new(status).style(Style::default().bg(Color::DarkGray));

    frame.render_widget(paragraph, area);
}

pub fn render_help_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let help_items = if state.is_dialog_open() {
        vec![
            ("Tab", "Next field"),
            ("Enter", "Save"),
            ("Esc", "Cancel"),
            ("←/→", "Cycle type"),
        ]
    } else if state.tables_filter_active {
        vec![
            ("Type", "Filter"),
            ("Enter", "Apply"),
            ("Esc", "Cancel"),
            ("Backspace", "Delete"),
        ]
    } else if state.results_filter_active {
        vec![
            ("Type", "Filter"),
            ("Enter", "Apply"),
            ("Esc", "Cancel"),
            ("Backspace", "Delete"),
        ]
    } else {
        match state.active_panel {
            ActivePanel::Connections => vec![
                ("Enter", "Connect"),
                ("n", "New"),
                ("d", "Delete"),
                ("Tab", "Next panel"),
                ("?", "Help"),
                ("q", "Quit"),
            ],
            ActivePanel::Tables => vec![
                ("/", "Filter"),
                ("Enter", "Select"),
                ("s", "Schema"),
                ("Ctrl+R", "Refresh"),
                ("Alt+±", "Resize"),
            ],
            ActivePanel::QueryEditor => {
                if state.has_selection() {
                    vec![
                        ("Ctrl+C", "Copy"),
                        ("Ctrl+X", "Cut"),
                        ("Ctrl+A", "Select all"),
                        ("Esc", "Deselect"),
                    ]
                } else {
                    vec![
                        ("F5", "Execute"),
                        ("Ctrl+↵", "Run current"),
                        ("F6", "Export"),
                        ("F7", "Import"),
                        ("Shift+←→", "Select"),
                    ]
                }
            }
            ActivePanel::Results => vec![
                ("/", "Filter"),
                ("↑/↓", "Navigate"),
                ("Enter", "Edit"),
                ("x", "Export"),
                ("Alt+±", "Resize"),
            ],
        }
    };

    let spans: Vec<Span> = help_items
        .iter()
        .flat_map(|(key, desc)| {
            vec![
                Span::styled(
                    format!(" {} ", key),
                    Style::default().bg(Color::DarkGray).fg(Color::White),
                ),
                Span::styled(format!("{} ", desc), Style::default().fg(Color::Gray)),
                Span::raw(" "),
            ]
        })
        .collect();

    let help_line = Line::from(spans);
    let paragraph = Paragraph::new(help_line);

    frame.render_widget(paragraph, area);
}
