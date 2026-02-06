use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::services::{AppState, ColumnDefinition, DialogMode};

/// Render the schema modification dialog
pub fn render_schema_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::SchemaModify {
        return;
    }

    let area = centered_rect(70, 80, frame.area());

    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let title = match &state.schema_action {
        Some(action) => format!(" {} ", action.title()),
        None => " Schema Modification ".to_string(),
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

    match &state.schema_action {
        Some(SchemaAction::AddColumn { .. }) | Some(SchemaAction::ModifyColumn { .. }) => {
            render_column_editor(frame, inner, state);
        }
        Some(SchemaAction::ViewColumns { columns }) => {
            render_columns_list(frame, inner, columns, state, false);
        }
        Some(SchemaAction::SelectColumn { columns, operation }) => {
            render_columns_list(frame, inner, columns, state, true);
            // Show operation hint at bottom
            let hint = match operation.as_str() {
                "modify" => "Press Enter to modify selected column",
                "drop" => "Press Enter to drop selected column",
                "rename" => "Press Enter to rename selected column",
                _ => "",
            };
            if !hint.is_empty() && inner.height > 2 {
                let hint_area = Rect {
                    x: inner.x,
                    y: inner.y + inner.height.saturating_sub(1),
                    width: inner.width,
                    height: 1,
                };
                let hint_text = Paragraph::new(hint).style(Style::default().fg(Color::Yellow));
                frame.render_widget(hint_text, hint_area);
            }
        }
        Some(SchemaAction::DropColumn {
            table_name,
            column_name,
        }) => {
            render_drop_confirmation(frame, inner, table_name, column_name);
        }
        Some(SchemaAction::RenameColumn {
            old_name, new_name, ..
        }) => {
            render_rename_column(frame, inner, old_name, new_name, state);
        }
        None => {
            render_action_menu(frame, inner, state);
        }
    }
}

/// Schema action types
#[derive(Debug, Clone)]
pub enum SchemaAction {
    ViewColumns {
        columns: Vec<ColumnDefinition>,
    },
    /// Select a column for an operation (modify, drop, rename)
    SelectColumn {
        columns: Vec<ColumnDefinition>,
        /// The operation to perform: "modify", "drop", or "rename"
        operation: String,
    },
    AddColumn {
        table_name: String,
        column: ColumnDefinition,
    },
    ModifyColumn {
        table_name: String,
        column: ColumnDefinition,
        original_name: String,
    },
    DropColumn {
        table_name: String,
        column_name: String,
    },
    RenameColumn {
        table_name: String,
        old_name: String,
        new_name: String,
    },
}

impl SchemaAction {
    pub fn title(&self) -> &'static str {
        match self {
            SchemaAction::ViewColumns { .. } => "Table Columns",
            SchemaAction::SelectColumn { operation, .. } => match operation.as_str() {
                "modify" => "Select Column to Modify",
                "drop" => "Select Column to Drop",
                "rename" => "Select Column to Rename",
                _ => "Select Column",
            },
            SchemaAction::AddColumn { .. } => "Add Column",
            SchemaAction::ModifyColumn { .. } => "Modify Column",
            SchemaAction::DropColumn { .. } => "Drop Column",
            SchemaAction::RenameColumn { .. } => "Rename Column",
        }
    }
}

/// Render the action menu
fn render_action_menu(frame: &mut Frame, area: Rect, state: &AppState) {
    let table_name = state.schema_table_name.as_deref().unwrap_or("Unknown");

    let items: Vec<ListItem> = vec![
        ListItem::new(format!("  Table: {}", table_name)).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        ListItem::new(""),
        ListItem::new("  [v] View Columns"),
        ListItem::new("  [a] Add Column"),
        ListItem::new("  [m] Modify Column"),
        ListItem::new("  [r] Rename Column"),
        ListItem::new("  [d] Drop Column"),
        ListItem::new(""),
        ListItem::new("  [Esc] Cancel"),
    ];

    let list = List::new(items)
        .block(Block::default())
        .highlight_style(Style::default().bg(Color::DarkGray));

    frame.render_widget(list, area);
}

/// Render the column editor for add/modify
fn render_column_editor(frame: &mut Frame, area: Rect, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Column Name
            Constraint::Length(3), // Data Type
            Constraint::Length(3), // Nullable
            Constraint::Length(3), // Primary Key
            Constraint::Length(3), // Default Value
            Constraint::Min(1),    // Spacer
            Constraint::Length(2), // Help
        ])
        .split(area);

    let column = match &state.schema_action {
        Some(SchemaAction::AddColumn { column, .. }) => column,
        Some(SchemaAction::ModifyColumn { column, .. }) => column,
        _ => return,
    };

    let active_field = state.schema_field_index;

    // Column Name
    render_text_field(
        frame,
        chunks[0],
        "Column Name",
        &column.name,
        active_field == 0,
        state.schema_cursor_pos,
    );

    // Data Type
    render_text_field(
        frame,
        chunks[1],
        "Data Type",
        &column.data_type,
        active_field == 1,
        state.schema_cursor_pos,
    );

    // Nullable
    let nullable_text = if column.nullable { "Yes" } else { "No" };
    render_toggle_field(
        frame,
        chunks[2],
        "Nullable",
        nullable_text,
        active_field == 2,
    );

    // Primary Key
    let pk_text = if column.is_primary_key { "Yes" } else { "No" };
    render_toggle_field(frame, chunks[3], "Primary Key", pk_text, active_field == 3);

    // Default Value
    render_text_field(
        frame,
        chunks[4],
        "Default Value",
        column.default_value.as_deref().unwrap_or(""),
        active_field == 4,
        state.schema_cursor_pos,
    );

    // Help
    let help = Line::from(vec![
        Span::styled(" Tab ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Next field "),
        Span::styled(" ←/→ ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Toggle "),
        Span::styled(" Enter ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Save "),
        Span::styled(" Esc ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Cancel "),
    ]);
    frame.render_widget(Paragraph::new(help), chunks[6]);
}

/// Render the columns list
fn render_columns_list(
    frame: &mut Frame,
    area: Rect,
    columns: &[ColumnDefinition],
    state: &AppState,
    selectable: bool,
) {
    let items: Vec<ListItem> = columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let pk_marker = if col.is_primary_key { " PK" } else { "" };
            let nullable_marker = if col.nullable { " NULL" } else { " NOT NULL" };
            let default_marker = col
                .default_value
                .as_ref()
                .map(|v| format!(" DEFAULT {}", v))
                .unwrap_or_default();

            let style = if i == state.schema_field_index {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let prefix = if selectable && i == state.schema_field_index {
                "▶ "
            } else {
                "  "
            };

            ListItem::new(format!(
                "{}{} - {}{}{}{}",
                prefix, col.name, col.data_type, pk_marker, nullable_marker, default_marker
            ))
            .style(style)
        })
        .collect();

    // Reserve space for hint if selectable
    let list_height = if selectable {
        Constraint::Min(2)
    } else {
        Constraint::Min(3)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([list_height, Constraint::Length(2)])
        .split(area);

    let list = List::new(items).block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(list, chunks[0]);

    // Help
    let help = if selectable {
        Line::from(vec![
            Span::styled(" ↑/↓ ", Style::default().bg(Color::DarkGray)),
            Span::raw(" Navigate "),
            Span::styled(" Enter ", Style::default().bg(Color::DarkGray)),
            Span::raw(" Select "),
            Span::styled(" Esc ", Style::default().bg(Color::DarkGray)),
            Span::raw(" Cancel "),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ↑/↓ ", Style::default().bg(Color::DarkGray)),
            Span::raw(" Navigate "),
            Span::styled(" Enter ", Style::default().bg(Color::DarkGray)),
            Span::raw(" Modify "),
            Span::styled(" Esc ", Style::default().bg(Color::DarkGray)),
            Span::raw(" Close "),
        ])
    };
    frame.render_widget(Paragraph::new(help), chunks[1]);
}

/// Render drop confirmation
fn render_drop_confirmation(frame: &mut Frame, area: Rect, table_name: &str, column_name: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    let warning = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "⚠️  WARNING: This action cannot be undone!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!(
            "Are you sure you want to drop column '{}' from table '{}'?",
            column_name, table_name
        )),
        Line::from(""),
    ]);

    frame.render_widget(warning, chunks[0]);

    let help = Line::from(vec![
        Span::styled(
            " Enter/y ",
            Style::default().bg(Color::Red).fg(Color::White),
        ),
        Span::raw(" Confirm Drop "),
        Span::styled(" Esc/n ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Cancel "),
    ]);
    frame.render_widget(Paragraph::new(help), chunks[1]);
}

/// Render rename column dialog
fn render_rename_column(
    frame: &mut Frame,
    area: Rect,
    old_name: &str,
    new_name: &str,
    state: &AppState,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Old Name (readonly)
            Constraint::Length(3), // New Name
            Constraint::Min(1),    // Spacer
            Constraint::Length(2), // Help
        ])
        .split(area);

    // Old name (readonly)
    let old_para = Paragraph::new(format!("Current Name: {}", old_name))
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(old_para, chunks[0]);

    // New name
    render_text_field(
        frame,
        chunks[1],
        "New Name",
        new_name,
        true,
        state.schema_cursor_pos,
    );

    // Help
    let help = Line::from(vec![
        Span::styled(" Enter ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Save "),
        Span::styled(" Esc ", Style::default().bg(Color::DarkGray)),
        Span::raw(" Cancel "),
    ]);
    frame.render_widget(Paragraph::new(help), chunks[3]);
}

/// Helper to render a text field
fn render_text_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    is_active: bool,
    cursor_pos: usize,
) {
    let style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };

    let content = format!("{}: {}", label, value);
    let paragraph = Paragraph::new(content).style(style).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(style),
    );

    frame.render_widget(paragraph, area);

    if is_active {
        let cursor_x = area.x + label.len() as u16 + 2 + cursor_pos as u16;
        let cursor_y = area.y;
        frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), cursor_y));
    }
}

/// Helper to render a toggle field
fn render_toggle_field(frame: &mut Frame, area: Rect, label: &str, value: &str, is_active: bool) {
    let style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };

    let marker = if is_active { "◄ ► " } else { "    " };
    let content = format!("{}: {}{}", label, marker, value);
    let paragraph = Paragraph::new(content).style(style).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(style),
    );

    frame.render_widget(paragraph, area);
}
