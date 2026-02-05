mod config;
mod db;
mod models;
mod services;
mod ui;

use std::io;

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use config::AppConfig;
use db::DatabaseConnection;
use services::{ActivePanel, AppState, ConnectionField, DialogMode};
use ui::render_ui;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Load configuration
    let config = AppConfig::load().unwrap_or_default();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut state = AppState::new(config);

    // Main loop
    let res = run_app(&mut terminal, &mut state).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Close any open connection
    if let Some(conn) = state.connection.take() {
        conn.close().await;
    }

    // Save config on exit
    state.config.save()?;

    if let Err(err) = res {
        eprintln!("Error: {err:?}");
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) -> Result<()> {
    loop {
        terminal.draw(|f| render_ui(f, state))?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Handle dialog input first
                if state.is_dialog_open() {
                    handle_dialog_input(state, key.code, key.modifiers);
                    continue;
                }

                match key.code {
                    // Quit
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.should_quit = true;
                    }
                    KeyCode::Char('q') if state.active_panel != ActivePanel::QueryEditor => {
                        state.should_quit = true;
                    }

                    // Panel navigation
                    KeyCode::Tab => state.next_panel(),
                    KeyCode::BackTab => state.prev_panel(),

                    // List navigation
                    KeyCode::Up | KeyCode::Char('k')
                        if state.active_panel != ActivePanel::QueryEditor =>
                    {
                        state.select_prev();
                    }
                    KeyCode::Down | KeyCode::Char('j')
                        if state.active_panel != ActivePanel::QueryEditor =>
                    {
                        state.select_next();
                    }

                    // Connect to selected database
                    KeyCode::Enter if state.active_panel == ActivePanel::Connections => {
                        handle_connect(state).await;
                    }

                    // Select table - generate SELECT query
                    KeyCode::Enter if state.active_panel == ActivePanel::Tables => {
                        if let Some(table) = state.tables.get(state.selected_table) {
                            state.query_input = format!("SELECT * FROM {} LIMIT 100", table);
                            state.cursor_position = state.query_input.len();
                            state.active_panel = ActivePanel::QueryEditor;
                        }
                    }

                    // Execute query
                    KeyCode::F(5) => {
                        handle_execute_query(state).await;
                    }

                    // Query editor input
                    KeyCode::Char(c) if state.active_panel == ActivePanel::QueryEditor => {
                        state.query_input.insert(state.cursor_position, c);
                        state.cursor_position += 1;
                    }
                    KeyCode::Backspace if state.active_panel == ActivePanel::QueryEditor => {
                        if state.cursor_position > 0 {
                            state.cursor_position -= 1;
                            state.query_input.remove(state.cursor_position);
                        }
                    }
                    KeyCode::Delete if state.active_panel == ActivePanel::QueryEditor => {
                        if state.cursor_position < state.query_input.len() {
                            state.query_input.remove(state.cursor_position);
                        }
                    }
                    KeyCode::Left if state.active_panel == ActivePanel::QueryEditor => {
                        state.cursor_position = state.cursor_position.saturating_sub(1);
                    }
                    KeyCode::Right if state.active_panel == ActivePanel::QueryEditor => {
                        if state.cursor_position < state.query_input.len() {
                            state.cursor_position += 1;
                        }
                    }
                    KeyCode::Home if state.active_panel == ActivePanel::QueryEditor => {
                        state.cursor_position = 0;
                    }
                    KeyCode::End if state.active_panel == ActivePanel::QueryEditor => {
                        state.cursor_position = state.query_input.len();
                    }

                    // New connection dialog
                    KeyCode::Char('n') if state.active_panel == ActivePanel::Connections => {
                        state.open_new_connection_dialog();
                    }
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.open_new_connection_dialog();
                    }

                    // Delete connection
                    KeyCode::Char('d') if state.active_panel == ActivePanel::Connections => {
                        if !state.config.connections.is_empty() {
                            let name = state.config.connections[state.selected_connection]
                                .name
                                .clone();
                            state.config.connections.remove(state.selected_connection);
                            if state.selected_connection >= state.config.connections.len()
                                && state.selected_connection > 0
                            {
                                state.selected_connection -= 1;
                            }
                            state.set_status(format!("Deleted connection: {}", name));
                        }
                    }

                    // Refresh tables
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        handle_refresh_tables(state).await;
                    }

                    // Help (placeholder - could show a help dialog)
                    KeyCode::Char('?') => {
                        state.set_status(
                            "Help: Tab=switch panels, Enter=connect/select, F5=execute, n=new, d=delete, q=quit",
                        );
                    }

                    _ => {}
                }
            }
        }

        if state.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_dialog_input(state: &mut AppState, key: KeyCode, modifiers: KeyModifiers) {
    match state.dialog_mode {
        DialogMode::NewConnection => {
            handle_new_connection_dialog(state, key, modifiers);
        }
        DialogMode::DeleteConfirm => {
            // TODO: implement delete confirmation
        }
        DialogMode::None => {}
    }
}

fn handle_new_connection_dialog(state: &mut AppState, key: KeyCode, _modifiers: KeyModifiers) {
    let nc = &mut state.new_connection;
    
    match key {
        KeyCode::Esc => {
            state.close_dialog();
            state.set_status("Cancelled");
            return;
        }
        KeyCode::Tab | KeyCode::Down => {
            // Move to next field
            nc.active_field = nc.active_field.next();
            nc.cursor_position = nc.get_active_field_value().len();
        }
        KeyCode::BackTab | KeyCode::Up => {
            // Move to previous field
            nc.active_field = nc.active_field.prev();
            nc.cursor_position = nc.get_active_field_value().len();
        }
        KeyCode::Left if nc.active_field == ConnectionField::DbType => {
            nc.cycle_db_type();
        }
        KeyCode::Right if nc.active_field == ConnectionField::DbType => {
            nc.cycle_db_type();
        }
        KeyCode::Enter => {
            // Save the connection
            let config = nc.to_config();
            let name = config.name.clone();
            state.config.add_connection(config);
            state.close_dialog();
            state.set_status(format!("Added connection: {}", name));
            return;
        }
        KeyCode::Char(c) => {
            // For port field, only allow digits
            if nc.active_field == ConnectionField::Port && !c.is_ascii_digit() {
                return;
            }
            let pos = nc.cursor_position;
            if let Some(field) = nc.get_active_field_mut() {
                field.insert(pos, c);
            }
            nc.cursor_position += 1;
        }
        KeyCode::Backspace => {
            if nc.cursor_position > 0 {
                let pos = nc.cursor_position - 1;
                if let Some(field) = nc.get_active_field_mut() {
                    field.remove(pos);
                }
                nc.cursor_position = pos;
            }
        }
        KeyCode::Delete => {
            let pos = nc.cursor_position;
            let len = nc.get_active_field_value().len();
            if pos < len {
                if let Some(field) = nc.get_active_field_mut() {
                    field.remove(pos);
                }
            }
        }
        KeyCode::Home => {
            nc.cursor_position = 0;
        }
        KeyCode::End => {
            nc.cursor_position = nc.get_active_field_value().len();
        }
        KeyCode::Left => {
            nc.cursor_position = nc.cursor_position.saturating_sub(1);
        }
        KeyCode::Right => {
            let len = nc.get_active_field_value().len();
            if nc.cursor_position < len {
                nc.cursor_position += 1;
            }
        }
        _ => {}
    }
}

async fn handle_connect(state: &mut AppState) {
    if state.config.connections.is_empty() {
        state.set_status("No connections configured. Press 'n' to add one.");
        return;
    }

    let conn_config = state.config.connections[state.selected_connection].clone();
    state.set_status(format!("Connecting to {}...", conn_config.name));
    state.is_loading = true;

    match DatabaseConnection::connect(&conn_config).await {
        Ok(conn) => {
            // Close existing connection if any
            if let Some(old_conn) = state.connection.take() {
                old_conn.close().await;
            }

            state.connection = Some(conn);
            state.current_connection_config = Some(conn_config.clone());
            state.set_status(format!("Connected to {}", conn_config.name));

            // Fetch tables
            handle_refresh_tables(state).await;
        }
        Err(e) => {
            state.set_status(format!("Connection failed: {}", e));
        }
    }

    state.is_loading = false;
}

async fn handle_refresh_tables(state: &mut AppState) {
    if let Some(ref conn) = state.connection {
        match conn.get_tables().await {
            Ok(tables) => {
                state.tables = tables;
                state.selected_table = 0;
                state.set_status(format!("Loaded {} tables", state.tables.len()));
            }
            Err(e) => {
                state.set_status(format!("Failed to fetch tables: {}", e));
            }
        }
    }
}

async fn handle_execute_query(state: &mut AppState) {
    if state.query_input.trim().is_empty() {
        state.set_status("Query is empty");
        return;
    }

    if state.connection.is_none() {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    }

    // Clone the query to avoid borrow issues
    let query = state.query_input.clone();
    state.set_status("Executing query...");
    state.is_loading = true;

    let result = state
        .connection
        .as_ref()
        .unwrap()
        .execute_query(&query)
        .await;

    match result {
        Ok(result) => {
            let row_count = result.rows.len();
            let time = result.execution_time_ms;
            state.query_result = Some(result);
            state.selected_row = 0;
            state.set_status(format!("Query executed: {} rows in {}ms", row_count, time));
            state.active_panel = ActivePanel::Results;
        }
        Err(e) => {
            state.set_status(format!("Query error: {}", e));
        }
    }

    state.is_loading = false;
}
