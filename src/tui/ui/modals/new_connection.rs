use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_rect;
use crate::engine::db::azure::AzureCloud;
use crate::engine::db::gcp_iam;
use crate::engine::models::{AzureAuthMethod, DatabaseType, SslMode};
use crate::engine::presets;
use crate::tui::{shows_ssl, uses_tls, AppState, ConnectionField, DialogMode};
use crate::tui::ui::widgets::draw_cursor;

/// Rows per field: the value line and its bottom rule.
const FIELD_HEIGHT: u16 = 2;
/// The most fields shown at once (PostgreSQL / MySQL with SSL, Azure with
/// the browser sign-in), so the dialog never resizes while cycling the type.
const MAX_FIELDS: u16 = 11;
/// Placeholder of an optional field the app fills in by itself.
const AUTO: &str = "auto";
/// Width of the port when the sign-in choice shares its row.
const PORT_WIDTH: u16 = 16;

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
    let azure_cli = is_azure && nc.azure_auth_method == AzureAuthMethod::Interactive;
    let browser = is_azure && nc.azure_auth_method == AzureAuthMethod::InteractiveBrowser;
    let show_tenant = azure_cli || browser;
    let managed_identity = is_azure && nc.azure_auth_method == AzureAuthMethod::ManagedIdentity;
    let show_username = !azure_cli;
    let gcp_iam = nc.gcp_iam();
    let show_password =
        !gcp_iam && (!is_azure || nc.azure_auth_method == AzureAuthMethod::Credentials);
    let show_ssl = shows_ssl(&nc.db_type);
    // PostgreSQL / MySQL's sign-in choice (password / IAM).
    let show_db_auth = uses_tls(&nc.db_type);
    let is_mongo = nc.db_type == DatabaseType::MongoDB;
    let is_redis = nc.db_type == DatabaseType::Redis;
    // MongoDB / Redis have no compatible products.
    let show_flavor = nc.db_type.is_sql();

    // Name, URL, Type, Host, Port, Database, plus the product (SQL), the
    // engine- and authentication-specific fields. PostgreSQL / MySQL's
    // sign-in choice shares the port's row: with SSL, they already fill the
    // dialog.
    let fields = 6
        + u16::from(show_flavor)
        + u16::from(is_mongo)
        + u16::from(is_azure)
        + u16::from(show_tenant)
        + u16::from(browser)
        + u16::from(show_username)
        + u16::from(show_password)
        + 2 * u16::from(show_ssl);
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
            let label_width = label.chars().count() as u16;
            let cursor_x = area.x + label_width + 2 + nc.cursor_position as u16;
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

    // MongoDB keeps its connection string (without the password) there.
    let url_label = if is_mongo { "URI" } else { "URL" };
    render_field(
        frame,
        chunks[idx],
        url_label,
        &nc.url,
        ConnectionField::Url,
        false,
    );
    if nc.url.is_empty() {
        let hint = match nc.db_type {
            DatabaseType::MongoDB => "mongodb://… ou mongodb+srv://… (ou hôte et port)",
            DatabaseType::Redis => "redis://… ou rediss://… (Entrée remplit les champs)",
            _ => "",
        };
        if !hint.is_empty() {
            render_placeholder(frame, chunks[idx], url_label, hint);
        }
    }
    idx += 1;

    if show_flavor {
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
    }

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
            AzureAuthMethod::Interactive => "Azure CLI",
            AzureAuthMethod::ManagedIdentity => "Managed Identity",
            AzureAuthMethod::InteractiveBrowser => "Azure AD (navigateur)",
        };
        let az_content = format!("Auth: {}{}", auth_label, az_hint);
        let az_paragraph = Paragraph::new(az_content).style(az_style).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(az_style),
        );
        frame.render_widget(az_paragraph, chunks[idx]);
        idx += 1;

        // Azure CLI's tenant ID; the browser sign-in's tenant and
        // application, both optional: "auto" while empty (the server's
        // tenant, Microsoft's drivers' application). Short labels: a GUID
        // fits on 80 columns.
        if show_tenant {
            let label = if browser { "Tenant" } else { "Tenant ID" };
            render_field(
                frame,
                chunks[idx],
                label,
                &nc.tenant_id,
                ConnectionField::TenantId,
                false,
            );
            if browser && nc.tenant_id.is_empty() {
                render_placeholder(frame, chunks[idx], label, AUTO);
            }
            idx += 1;
        }
        if browser {
            const LABEL: &str = "ID app";
            render_field(
                frame,
                chunks[idx],
                LABEL,
                &nc.azure_client_id,
                ConnectionField::ClientId,
                false,
            );
            if nc.azure_client_id.is_empty() {
                render_placeholder(frame, chunks[idx], LABEL, AUTO);
            }
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
    // Like the GUI's placeholder: the preset's host hint while empty.
    if let (true, Some(f)) = (nc.host.is_empty(), nc.flavor) {
        render_placeholder(frame, chunks[idx], "Host", presets::preset(f).host_hint);
    }
    // Where the tokens come from, deduced from the host: on the row's rule,
    // right-aligned, where the host value can't hide it.
    if is_azure {
        let cloud = format!(" {} ", AzureCloud::from_host(&nc.host));
        render_on_rule(frame, chunks[idx], &cloud);
    }
    idx += 1;

    if show_db_auth {
        let row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(PORT_WIDTH), Constraint::Min(0)])
            .split(chunks[idx]);
        render_field(
            frame,
            row[0],
            "Port",
            &nc.port,
            ConnectionField::Port,
            false,
        );
        // A short hint: the row is shared.
        let active = nc.active_field == ConnectionField::DbAuth;
        let style = Style::default().fg(if active { Color::Cyan } else { Color::Gray });
        let hint = if active { " ←/→" } else { "" };
        let paragraph = Paragraph::new(format!("Auth: {}{hint}", nc.db_auth))
            .style(style)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(style),
            );
        frame.render_widget(paragraph, row[1]);
    } else {
        render_field(
            frame,
            chunks[idx],
            "Port",
            &nc.port,
            ConnectionField::Port,
            false,
        );
    }
    idx += 1;

    if gcp_iam {
        // The Google account, kept in the username; the database user it
        // stands for on the rule, where the value can't hide it.
        const LABEL: &str = "Compte Google";
        render_field(
            frame,
            chunks[idx],
            LABEL,
            &nc.username,
            ConnectionField::Username,
            false,
        );
        if nc.username.is_empty() {
            render_placeholder(frame, chunks[idx], LABEL, "prenom.nom@domaine.com");
        } else {
            let user = gcp_iam::database_user(&nc.db_type, &nc.username);
            render_on_rule(frame, chunks[idx], &format!(" utilisateur base : {user} "));
        }
        idx += 1;
    } else if managed_identity {
        // The user-assigned identity's client ID, kept in the username.
        const LABEL: &str = "ID client";
        render_field(
            frame,
            chunks[idx],
            LABEL,
            &nc.username,
            ConnectionField::Username,
            false,
        );
        if nc.username.is_empty() {
            render_placeholder(frame, chunks[idx], LABEL, "vide = identité système");
        }
        idx += 1;
    } else if browser {
        // The Entra ID account (login hint), kept in the username.
        const LABEL: &str = "Compte";
        render_field(
            frame,
            chunks[idx],
            LABEL,
            &nc.username,
            ConnectionField::Username,
            false,
        );
        if nc.username.is_empty() {
            render_placeholder(frame, chunks[idx], LABEL, "facultatif");
        }
        idx += 1;
    } else if show_username {
        render_field(
            frame,
            chunks[idx],
            "Username",
            &nc.username,
            ConnectionField::Username,
            false,
        );
        if is_redis && nc.username.is_empty() {
            render_placeholder(
                frame,
                chunks[idx],
                "Username",
                "utilisateur ACL (facultatif)",
            );
        }
        idx += 1;
    }

    if show_password {
        render_field(
            frame,
            chunks[idx],
            "Password",
            &nc.password,
            ConnectionField::Password,
            true,
        );
        idx += 1;
    }

    if is_mongo {
        const LABEL: &str = "Base d'authentification";
        render_field(
            frame,
            chunks[idx],
            LABEL,
            &nc.auth_source,
            ConnectionField::AuthSource,
            false,
        );
        if nc.auth_source.is_empty() {
            render_placeholder(frame, chunks[idx], LABEL, "admin");
        }
        idx += 1;
    }

    let db_label = match nc.db_type {
        DatabaseType::MongoDB => "Base par défaut",
        DatabaseType::Redis => "Base (0-15)",
        _ => "Database",
    };
    render_field(
        frame,
        chunks[idx],
        db_label,
        &nc.database,
        ConnectionField::Database,
        false,
    );
    if is_mongo && nc.database.is_empty() {
        render_placeholder(frame, chunks[idx], db_label, "vide = toutes");
    }
    idx += 1;

    if show_ssl {
        let mode = nc.ssl_mode.to_string();
        let ssl_label = if show_db_auth { "SSL" } else { "TLS" };
        render_cycle(
            frame,
            chunks[idx],
            ssl_label,
            &mode,
            ConnectionField::SslMode,
        );

        // The IAM sign-in connects with TLS whatever the mode says.
        if gcp_iam && matches!(nc.ssl_mode, SslMode::Disable | SslMode::Prefer) {
            render_on_rule(frame, chunks[idx], " IAM : au moins Obligatoire ");
        }
        idx += 1;
        render_field(
            frame,
            chunks[idx],
            "Certificat CA",
            &nc.ssl_ca,
            ConnectionField::SslCa,
            false,
        );
        // The CA is only read when verifying (VerifyCa / VerifyFull).
        if nc.ssl_ca.is_empty() {
            render_placeholder(
                frame,
                chunks[idx],
                "Certificat CA",
                "utilisé en mode Vérifier",
            );
        }
    }
}

/// Dimmed `text` on a field's bottom rule, right-aligned; left out when it
/// doesn't fit.
fn render_on_rule(frame: &mut Frame, area: Rect, text: &str) {
    let len = text.chars().count() as u16;
    if area.height > 1 && len < area.width {
        frame.buffer_mut().set_stringn(
            area.x + area.width - len - 1,
            area.y + 1,
            text,
            len as usize,
            Style::default().fg(Color::DarkGray),
        );
    }
}

/// Dimmed text in place of an empty field's value, after `label: `; the
/// cursor cell keeps its reversed style.
fn render_placeholder(frame: &mut Frame, area: Rect, label: &str, text: &str) {
    let x = area.x + label.chars().count() as u16 + 2;
    let width = (area.x + area.width).saturating_sub(x) as usize;
    frame
        .buffer_mut()
        .set_stringn(x, area.y, text, width, Style::default().fg(Color::DarkGray));
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
    fn managed_identity_asks_for_a_client_id_and_shows_the_cloud() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        let nc = &mut state.new_connection;
        nc.set_db_type(DatabaseType::Azure);
        nc.host = "srv.database.usgovcloudapi.net".into();
        nc.username.clear();
        nc.azure_auth_method = AzureAuthMethod::ManagedIdentity;
        let all = screen(&state).join("\n");
        assert!(all.contains("ID client: vide = identité système"), "{all}");
        assert!(all.contains(" Azure Government "), "{all}");
        assert!(!all.contains("Password:"), "{all}");
        assert!(!all.contains("Tenant ID:"), "{all}");

        state.new_connection.azure_auth_method = AzureAuthMethod::Interactive;
        let all = screen(&state).join("\n");
        assert!(all.contains("Tenant ID:"), "{all}");
        assert!(
            !all.contains("Username:") && !all.contains("ID client"),
            "{all}"
        );

        state.new_connection.azure_auth_method = AzureAuthMethod::Credentials;
        state.new_connection.host = "srv.database.windows.net".into();
        let all = screen(&state).join("\n");
        assert!(
            all.contains("Username:") && all.contains("Password:"),
            "{all}"
        );
        assert!(all.contains(" Azure (cloud public) "), "{all}");
    }

    #[test]
    fn interactive_browser_asks_for_the_tenant_the_app_and_the_account() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        let nc = &mut state.new_connection;
        nc.set_db_type(DatabaseType::Azure);
        nc.azure_auth_method = AzureAuthMethod::Interactive;
        nc.cycle_azure_auth_method(); // the browser sign-in, from Azure CLI
        nc.username.clear();
        let rows = screen(&state);
        let all = rows.join("\n");
        // All optional: the tenant and the application are found by
        // themselves.
        for label in [
            "Auth: Azure AD (navigateur)",
            "Tenant: auto",
            "ID app: auto",
            "Compte: facultatif",
            "Database:",
        ] {
            assert!(all.contains(label), "{label} missing:\n{all}");
        }
        assert!(
            !all.contains("Password:") && !all.contains("Username:"),
            "{all}"
        );
        // Every field fits: the bottom border is drawn.
        assert!(rows[23].contains('└'), "{all}");

        let nc = &mut state.new_connection;
        nc.tenant_id = "contoso.com".into();
        nc.azure_client_id = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into();
        nc.username = "jane@contoso.com".into();
        let all = screen(&state).join("\n");
        assert!(all.contains("Tenant: contoso.com"), "{all}");
        assert!(
            all.contains("ID app: 8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f"),
            "{all}"
        );
        assert!(all.contains("Compte: jane@contoso.com"), "{all}");
        assert!(!all.contains("auto"), "{all}");

        state.new_connection.azure_auth_method = AzureAuthMethod::Interactive;
        let all = screen(&state).join("\n");
        assert!(all.contains("Auth: Azure CLI"), "{all}");
        assert!(!all.contains("ID app"), "{all}");
    }

    #[test]
    fn the_sign_in_choice_shares_the_port_row() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        let rows = screen(&state);
        let all = rows.join("\n");
        let port = rows.iter().find(|r| r.contains("Port: 5432")).unwrap();
        assert!(port.contains("Auth: Mot de passe"), "{all}");
        assert!(all.contains("Password:"), "{all}");
        // Every field still fits.
        assert!(rows[23].contains('└'), "{all}");

        state.new_connection.active_field = ConnectionField::DbAuth;
        let all = screen(&state).join("\n");
        assert!(all.contains("Auth: Mot de passe ←/→"), "{all}");

        // Other engines don't offer it.
        state.new_connection.set_db_type(DatabaseType::SQLServer);
        let all = screen(&state).join("\n");
        assert!(!all.contains("Auth:"), "{all}");
    }

    #[test]
    fn gcp_iam_asks_for_the_google_account_and_hints_the_user() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        state.new_connection.set_db_type(DatabaseType::MySQL);
        state.new_connection.username = "root".into();
        state.new_connection.cycle_db_auth();
        let rows = screen(&state);
        let all = rows.join("\n");
        for label in [
            "Auth: IAM Google Cloud",
            "Compte Google: prenom.nom@domaine.com",
            // The engine raises the default mode.
            "IAM : au moins Obligatoire",
        ] {
            assert!(all.contains(label), "{label} missing:\n{all}");
        }
        assert!(!all.contains("Password:"), "{all}");
        assert!(!all.contains("utilisateur base"), "{all}");
        assert!(rows[23].contains('└'), "{all}");

        state.new_connection.username = "jane@example.com".into();
        state.new_connection.ssl_mode = SslMode::VerifyFull;
        let all = screen(&state).join("\n");
        assert!(all.contains("Compte Google: jane@example.com"), "{all}");
        assert!(all.contains(" utilisateur base : jane "), "{all}");
        assert!(!all.contains("au moins"), "{all}");

        state.new_connection.set_db_type(DatabaseType::Postgres);
        let all = screen(&state).join("\n");
        assert!(
            all.contains(" utilisateur base : jane@example.com "),
            "{all}"
        );
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

    #[test]
    fn mongodb_and_redis_show_their_own_fields() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        state.new_connection.set_db_type(DatabaseType::MongoDB);
        let rows = screen(&state);
        let all = rows.join("\n");
        for label in [
            "URI: mongodb://… ou mongodb+srv://…",
            "Type: MongoDB",
            "Port: 27017",
            "Base d'authentification: admin",
            "Base par défaut: vide = toutes",
            "TLS: ",
            "Certificat CA:",
        ] {
            assert!(all.contains(label), "{label} missing:\n{all}");
        }
        assert!(!all.contains("Modèle"), "no products: {all}");
        assert!(!all.contains("Auth:"), "no IAM: {all}");
        assert!(rows[23].contains('└'), "{all}");

        state.new_connection.set_db_type(DatabaseType::Redis);
        let all = screen(&state).join("\n");
        for label in [
            "URL: redis://… ou rediss://…",
            "Type: Redis",
            "Port: 6379",
            "Username: utilisateur ACL (facultatif)",
            "Base (0-15): 0",
            "TLS: ",
        ] {
            assert!(all.contains(label), "{label} missing:\n{all}");
        }
        assert!(!all.contains("Base d'authentification"), "{all}");
    }

    #[test]
    fn an_empty_ca_says_when_it_is_used() {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.open_new_connection_dialog();
        let all = screen(&state).join("\n");
        assert!(
            all.contains("Certificat CA: utilisé en mode Vérifier"),
            "{all}"
        );
        state.new_connection.ssl_ca = "ca.pem".into();
        let all = screen(&state).join("\n");
        assert!(all.contains("Certificat CA: ca.pem "), "{all}");
        assert!(!all.contains("utilisé en mode"), "{all}");
    }
}
