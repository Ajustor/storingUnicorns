use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::engine::models::{AzureAuthMethod, DatabaseType};
use crate::engine::presets;
use crate::tui::{uses_tls, AppState, ConnectionField, DialogMode};
use crate::tui::ui::widgets::draw_cursor;

/// Rows per field: the value line and its bottom rule.
const FIELD_HEIGHT: u16 = 2;
/// The most fields shown at once (PostgreSQL / MySQL with SSL, or Azure
/// interactive), so the dialog never resizes while cycling the type.
const MAX_FIELDS: u16 = 11;

/// Where the connection dialog is drawn: 60 % wide, tall enough for every
/// field (24 rows), clamped to the terminal.
pub fn connection_dialog_rect(area: Rect) -> Rect {
    let width = centered_rect(60, 70, area).width;
    let height = (MAX_FIELDS * FIELD_HEIGHT + 2).min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

pub fn render_new_connection_dialog(frame: &mut Frame, state: &AppState) {
    if state.dialog_mode != DialogMode::NewConnection
        && state.dialog_mode != DialogMode::EditConnection
    {
        return;
    }

    let area = connection_dialog_rect(frame.area());

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
    let show_ssl = uses_tls(&nc.db_type);

    // Name, URL, Modèle, Type, Host, Port, Username, Password, Database,
    // plus the engine-specific fields.
    let fields = 9 + u16::from(is_azure) + u16::from(show_tenant) + 2 * u16::from(show_ssl);
    let mut constraints = vec![Constraint::Length(FIELD_HEIGHT); fields as usize];
    constraints.push(Constraint::Min(0)); // Spacer

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

        // Show cursor for the active field (cycle fields use render_cycle)
        if is_active {
            let cursor_x = area.x + label.len() as u16 + 2 + nc.cursor_position as u16;
            let cursor_y = area.y;
            draw_cursor(frame, cursor_x.min(area.x + area.width - 1), cursor_y);
        }
    };

    // A value changed with ←/→, like the type.
    let render_cycle = |frame: &mut Frame, area: Rect, label: &str, value: &str, field| {
        let active = nc.active_field == field;
        let style = if active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        let hint = if active { " (←/→ to change)" } else { "" };
        let paragraph = Paragraph::new(format!("{label}: {value}{hint}"))
            .style(style)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(style),
            );
        frame.render_widget(paragraph, area);
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

    render_field(
        frame,
        chunks[idx],
        "URL",
        &nc.url,
        ConnectionField::Url,
        false,
    );
    idx += 1;

    let flavor = nc
        .flavor
        .map_or_else(|| "Aucun".to_string(), |f| f.to_string());
    render_cycle(
        frame,
        chunks[idx],
        "Modèle",
        &flavor,
        ConnectionField::Flavor,
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
    // Like the GUI's placeholder: the preset's host hint, dimmed, while
    // the field is empty (the cursor cell keeps its reversed style).
    if let (true, Some(f)) = (nc.host.is_empty(), nc.flavor) {
        let area = chunks[idx];
        let x = area.x + "Host: ".len() as u16;
        let width = (area.x + area.width).saturating_sub(x) as usize;
        frame.buffer_mut().set_stringn(
            x,
            area.y,
            presets::preset(f).host_hint,
            width,
            Style::default().fg(Color::DarkGray),
        );
    }
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
    idx += 1;

    if show_ssl {
        let mode = nc.ssl_mode.to_string();
        render_cycle(frame, chunks[idx], "SSL", &mode, ConnectionField::SslMode);
        idx += 1;
        render_field(
            frame,
            chunks[idx],
            "Certificat CA",
            &nc.ssl_ca,
            ConnectionField::SslCa,
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::AppConfig;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(state: &AppState) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_new_connection_dialog(f, state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    #[test]
    fn every_field_fits_in_80_by_24() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        let rows = screen(&state);
        let all = rows.join("\n");
        for label in [
            "Name:",
            "URL:",
            "Modèle: Aucun",
            "Type: PostgreSQL",
            "Host:",
            "Port:",
            "Username:",
            "Password:",
            "Database:",
            "SSL: Préféré",
            "Certificat CA:",
        ] {
            assert!(all.contains(label), "{label} missing:\n{all}");
        }
        // The bottom border is drawn: nothing was clipped.
        assert!(rows[23].contains('└'), "{all}");
    }

    #[test]
    fn ssl_rows_only_for_postgres_and_mysql() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        state.new_connection.set_db_type(DatabaseType::SQLServer);
        let all = screen(&state).join("\n");
        assert!(!all.contains("SSL:"), "{all}");
        assert!(all.contains("Database:"), "{all}");
    }

    #[test]
    fn an_empty_host_shows_the_preset_hint() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        state.new_connection.flavor = Some(crate::engine::models::Flavor::Supabase);
        state.new_connection.host.clear();
        let all = screen(&state).join("\n");
        assert!(all.contains("Host: db.<projet>.supabase.co"), "{all}");
        state.new_connection.host = "mine".into();
        let all = screen(&state).join("\n");
        assert!(!all.contains("supabase.co"), "{all}");
    }
}
