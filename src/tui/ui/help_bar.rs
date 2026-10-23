use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::{nosql, ActivePanel, AppState, DialogMode};

/// Returns the help entries for the Connections panel.
fn connections_help(state: &AppState) -> Vec<(&'static str, &'static str)> {
    if state.selected_folder.is_some() {
        vec![
            ("Enter/←/→", "Fold"),
            ("r", "Rename"),
            ("d", "Delete"),
            ("f", "New folder"),
            ("n", "New conn"),
            ("Tab", "Next panel"),
        ]
    } else {
        vec![
            ("Enter", "Connect"),
            ("n", "New"),
            ("e", "Edit"),
            ("d", "Delete"),
            ("m", "Move"),
            ("f", "New folder"),
            ("Tab", "Next panel"),
            ("F6/F7 Ctrl+O/L", "Exp/Imp"),
        ]
    }
}

/// Returns the help entries for the Tables panel.
fn tables_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("/", "Filter"),
        ("Enter", "Select"),
        ("Space", "Expand"),
        ("s", "Schema"),
        ("t", "Truncate"),
        ("h/H", "Hide/Show hidden"),
        ("E/I", "Batch exp/imp"),
        ("F6/F7 Ctrl+O/L", "Exp/Imp"),
    ]
}

/// Returns the help entries for the QueryEditor panel.
fn query_editor_help(state: &AppState) -> Vec<(&'static str, &'static str)> {
    if state.show_completion {
        vec![
            ("↑/↓", "Navigate"),
            ("Enter", "Accept"),
            ("Esc", "Dismiss"),
            ("Ctrl+Space/K", "Refresh"),
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
            ("F5/Ctrl+G", "Execute"),
            ("Ctrl+↵/E", "Run current"),
            ("Ctrl+Space/K", "Complete"),
            ("Ctrl+T", "New tab"),
            ("Ctrl+W", "Close tab"),
            ("F6/Ctrl+O", "Export"),
            ("F7/Ctrl+L", "Import"),
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
        ("F6/F7 Ctrl+O/L", "Exp/Imp"),
    ]
}

/// Returns the help entries for dialog modes.
fn dialog_help(mode: DialogMode) -> Vec<(&'static str, &'static str)> {
    match mode {
        DialogMode::NewConnection | DialogMode::EditConnection => vec![
            ("Tab", "Next field"),
            ("Enter", "Save"),
            ("Esc", "Cancel"),
            ("←/→", "Cycle value"),
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
        DialogMode::FolderPrompt => vec![("Enter", "Confirm"), ("Esc", "Cancel")],
        DialogMode::NoSqlConfirm => vec![("o", "Oui"), ("N/Entrée/Échap", "Non")],
        DialogMode::JsonView => vec![
            ("↑/↓", "Défiler"),
            ("PgUp/PgDn", "Page"),
            ("Échap", "Fermer"),
        ],
        DialogMode::Indexes => vec![
            ("↑/↓", "Choisir"),
            ("n", "Nouvel index"),
            ("d", "Supprimer"),
            ("r", "Recharger"),
            ("Échap", "Fermer"),
        ],
        DialogMode::NoSqlForm => vec![
            ("Tab", "Champ suivant"),
            ("←/→", "Choix"),
            ("Entrée", "Valider"),
            ("Échap", "Annuler"),
        ],
        DialogMode::NoSqlTransfer => vec![
            ("Tab", "Champ"),
            ("←/→", "Format/Base"),
            ("Espace", "Cocher"),
            ("a/n", "Tout/Rien"),
            ("Entrée", "Lancer"),
            ("Échap", "Annuler"),
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
        let engine = nosql::engine(state);
        let mut entries = match (state.active_panel, &engine) {
            (ActivePanel::Connections, _) => connections_help(state),
            (ActivePanel::Tables, Some(db)) => nosql::tables_help(db),
            (ActivePanel::Tables, None) => tables_help(),
            (ActivePanel::QueryEditor, Some(_))
                if !state.show_completion && !state.has_selection() =>
            {
                nosql::editor_help()
            }
            (ActivePanel::QueryEditor, _) => query_editor_help(state),
            (ActivePanel::Results, Some(_)) => nosql::results_help(),
            (ActivePanel::Results, None) => results_help(),
        };

        if state.pending_connect.is_some() {
            entries.insert(0, ("Esc", "Annuler la connexion"));
        }
        entries
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
