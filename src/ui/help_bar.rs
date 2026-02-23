use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::services::{ActivePanel, AppState, DialogMode};

/// Returns the help entries for the Connections panel.
fn connections_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Enter", "Connect"),
        ("n", "New"),
        ("e", "Edit"),
        ("d", "Delete"),
        ("Tab", "Next panel"),
        ("F6/F7", "Exp/Imp"),
    ]
}

/// Returns the help entries for the Tables panel.
fn tables_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("/", "Filter"),
        ("Enter", "Select"),
        ("Space", "Expand"),
        ("s", "Schema"),
        ("t", "Truncate"),
        ("F6/F7", "Exp/Imp"),
    ]
}

/// Returns the help entries for the QueryEditor panel.
fn query_editor_help(state: &AppState) -> Vec<(&'static str, &'static str)> {
    if state.show_completion {
        vec![
            ("↑/↓", "Navigate"),
            ("Enter", "Accept"),
            ("Esc", "Dismiss"),
            ("Ctrl+Space", "Refresh"),
        ]
    } else if state.has_selection() {
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
            ("Ctrl+T", "New tab"),
            ("Ctrl+W", "Close tab"),
            ("F6", "Export"),
            ("F7", "Import"),
        ]
    }
}

/// Returns the help entries for the Results panel.
fn results_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("/", "Filter"),
        ("↑/↓", "Navigate"),
        ("Enter", "Edit"),
        ("d/Del", "Delete"),
        ("a", "Add row"),
        ("F6/F7", "Exp/Imp"),
    ]
}

/// Returns the help entries for dialog modes.
fn dialog_help(mode: DialogMode) -> Vec<(&'static str, &'static str)> {
    match mode {
        DialogMode::NewConnection | DialogMode::EditConnection => vec![
            ("Tab", "Next field"),
            ("Enter", "Save"),
            ("Esc", "Cancel"),
            ("←/→", "Cycle type"),
        ],
        DialogMode::EditRow | DialogMode::AddRow => {
            vec![("Tab", "Next field"), ("Enter", "Save"), ("Esc", "Cancel")]
        }
        DialogMode::SchemaModify => vec![
            ("v", "View"),
            ("a", "Add col"),
            ("m", "Modify"),
            ("r", "Rename"),
            ("d", "Drop"),
            ("Esc", "Close"),
        ],
        DialogMode::Export | DialogMode::Import => vec![
            ("Tab", "Completion"),
            ("Enter", "Confirm"),
            ("Esc", "Cancel"),
            ("←/→", "Cycle format"),
        ],
        DialogMode::BatchExport | DialogMode::BatchImport => vec![
            ("Tab", "Completion"),
            ("Space", "Toggle"),
            ("a/n", "All/None"),
            ("Enter", "Start"),
            ("Esc", "Cancel"),
        ],
        DialogMode::DeleteRowConfirm | DialogMode::TruncateConfirm => {
            vec![("y/Enter", "Confirm"), ("n/Esc", "Cancel")]
        }
        DialogMode::BatchTruncate => vec![
            ("Space", "Toggle"),
            ("a/n", "All/None"),
            ("Enter", "Truncate"),
            ("Esc", "Cancel"),
        ],
        _ => vec![("Tab", "Next field"), ("Enter", "Save"), ("Esc", "Cancel")],
    }
}

/// Returns the help entries for active filter modes.
fn filter_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Type", "Filter"),
        ("Enter", "Apply"),
        ("Esc", "Cancel"),
        ("Backspace", "Delete"),
    ]
}

/// Returns contextual help entries based on the current application state.
pub fn get_help_entries(state: &AppState) -> Vec<(&'static str, &'static str)> {
    if state.is_dialog_open() {
        dialog_help(state.dialog_mode)
    } else if state.tables_filter_active || state.results_filter_active {
        filter_help()
    } else {
        match state.active_panel {
            ActivePanel::Connections => connections_help(),
            ActivePanel::Tables => tables_help(),
            ActivePanel::QueryEditor => query_editor_help(state),
            ActivePanel::Results => results_help(),
        }
    }
}

/// Renders the help bar at the bottom of the screen.
pub fn render_help_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let help_items = get_help_entries(state);

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
