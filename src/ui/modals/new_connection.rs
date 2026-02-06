use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::services::{AppState, ConnectionField, DialogMode};

pub fn render_new_connection_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::NewConnection
        && state.dialog_mode != DialogMode::EditConnection
    {
        return;
    }

    let area = centered_rect(60, 70, frame.area());

    // Clear the area behind the dialog
    frame.render_widget(Clear, area);

    let title = if state.dialog_mode == DialogMode::EditConnection {
        " Edit Connection "
    } else {
        " New Connection "
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name
            Constraint::Length(3), // DB Type
            Constraint::Length(3), // Host
            Constraint::Length(3), // Port
            Constraint::Length(3), // Username
            Constraint::Length(3), // Password
            Constraint::Length(3), // Database
            Constraint::Min(1),    // Spacer
        ])
        .split(inner);

    let nc = &state.new_connection;

    // Helper to render a field
    let render_field = |frame: &mut Frame,
                        area: Rect,
                        label: &str,
                        value: &str,
                        field: ConnectionField,
                        is_password: bool| {
        let is_active = nc.active_field == field;
        let display_value = if is_password && !value.is_empty() {
            "*".repeat(value.len())
        } else {
            value.to_string()
        };

        let style = if is_active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };

        let content = format!("{}: {}", label, display_value);
        let paragraph = Paragraph::new(content).style(style).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(style),
        );

        frame.render_widget(paragraph, area);

        // Show cursor for active text field
        if is_active && field != ConnectionField::DbType {
            let cursor_x = area.x + label.len() as u16 + 2 + nc.cursor_position as u16;
            let cursor_y = area.y;
            frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), cursor_y));
        }
    };

    render_field(
        frame,
        chunks[0],
        "Name",
        &nc.name,
        ConnectionField::Name,
        false,
    );

    // DB Type - special handling with cycle indicator
    let db_type_active = nc.active_field == ConnectionField::DbType;
    let db_style = if db_type_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };
    let db_hint = if db_type_active {
        " (←/→ to change)"
    } else {
        ""
    };
    let db_content = format!("Type: {}{}", nc.db_type, db_hint);
    let db_paragraph = Paragraph::new(db_content).style(db_style).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(db_style),
    );
    frame.render_widget(db_paragraph, chunks[1]);

    render_field(
        frame,
        chunks[2],
        "Host",
        &nc.host,
        ConnectionField::Host,
        false,
    );
    render_field(
        frame,
        chunks[3],
        "Port",
        &nc.port,
        ConnectionField::Port,
        false,
    );
    render_field(
        frame,
        chunks[4],
        "Username",
        &nc.username,
        ConnectionField::Username,
        false,
    );
    render_field(
        frame,
        chunks[5],
        "Password",
        &nc.password,
        ConnectionField::Password,
        true,
    );
    render_field(
        frame,
        chunks[6],
        "Database",
        &nc.database,
        ConnectionField::Database,
        false,
    );
}
