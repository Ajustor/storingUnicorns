use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::models::{AzureAuthMethod, DatabaseType};
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

    let nc = &state.new_connection;
    let is_azure = nc.db_type == DatabaseType::Azure;
    let show_tenant = is_azure && nc.azure_auth_method == AzureAuthMethod::Interactive;

    // Build constraints dynamically based on db type
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(3), // Name
        Constraint::Length(3), // DB Type
    ];
    if is_azure {
        constraints.push(Constraint::Length(3)); // Azure Auth
        if show_tenant {
            constraints.push(Constraint::Length(3)); // Tenant ID
        }
    }
    constraints.push(Constraint::Length(3)); // Host
    constraints.push(Constraint::Length(3)); // Port
    constraints.push(Constraint::Length(3)); // Username
    constraints.push(Constraint::Length(3)); // Password
    constraints.push(Constraint::Length(3)); // Database
    constraints.push(Constraint::Min(1));    // Spacer

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

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

        // Show cursor for active text field (not for cycle fields)
        if is_active
            && field != ConnectionField::DbType
            && field != ConnectionField::AzureAuth
        {
            let cursor_x = area.x + label.len() as u16 + 2 + nc.cursor_position as u16;
            let cursor_y = area.y;
            frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), cursor_y));
        }
    };

    let mut idx = 0;

    // Name
    render_field(
        frame,
        chunks[idx],
        "Name",
        &nc.name,
        ConnectionField::Name,
        false,
    );
    idx += 1;

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
    frame.render_widget(db_paragraph, chunks[idx]);
    idx += 1;

    // Azure Auth Method (only for Azure)
    if is_azure {
        let azure_active = nc.active_field == ConnectionField::AzureAuth;
        let az_style = if azure_active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        let az_hint = if azure_active {
            " (←/→ to change)"
        } else {
            ""
        };
        let auth_label = match nc.azure_auth_method {
            AzureAuthMethod::Credentials => "SQL Credentials",
            AzureAuthMethod::Interactive => "Azure AD Interactive",
            AzureAuthMethod::ManagedIdentity => "Managed Identity",
        };
        let az_content = format!("Auth: {}{}", auth_label, az_hint);
        let az_paragraph = Paragraph::new(az_content).style(az_style).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(az_style),
        );
        frame.render_widget(az_paragraph, chunks[idx]);
        idx += 1;

        // Tenant ID (only for Interactive)
        if show_tenant {
            render_field(
                frame,
                chunks[idx],
                "Tenant ID",
                &nc.tenant_id,
                ConnectionField::TenantId,
                false,
            );
            idx += 1;
        }
    }

    render_field(
        frame,
        chunks[idx],
        "Host",
        &nc.host,
        ConnectionField::Host,
        false,
    );
    idx += 1;

    render_field(
        frame,
        chunks[idx],
        "Port",
        &nc.port,
        ConnectionField::Port,
        false,
    );
    idx += 1;

    render_field(
        frame,
        chunks[idx],
        "Username",
        &nc.username,
        ConnectionField::Username,
        false,
    );
    idx += 1;

    render_field(
        frame,
        chunks[idx],
        "Password",
        &nc.password,
        ConnectionField::Password,
        true,
    );
    idx += 1;

    render_field(
        frame,
        chunks[idx],
        "Database",
        &nc.database,
        ConnectionField::Database,
        false,
    );
}
