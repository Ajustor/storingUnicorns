mod config;
mod db;
mod models;
mod services;
mod ui;

use std::{fs::File, io, sync::Arc, time::Instant};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use config::AppConfig;
use db::DatabaseConnection;
use services::{ActivePanel, AppState, ConnectionField, DialogMode};
use ui::{render_ui, ClickableRegistry};

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let debug_mode = args.iter().any(|arg| arg == "--debug" || arg == "-d");

    // Setup logging
    let file = File::create("debug.log");
    let file = match file {
        Ok(file) => file,
        Err(error) => panic!("Error: {:?}", error),
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(Arc::new(file))
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
    let mut state = AppState::new(config, debug_mode);

    if debug_mode {
        state.set_status("Debug mode enabled - queries will be shown in editor");
    }

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

    // Save config and queries on exit
    state.config.save()?;
    state.save_query_tabs();

    if let Err(err) = res {
        eprintln!("Error: {err:?}");
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) -> Result<()> {
    // Track last click for double-click detection
    let mut last_click: Option<(Instant, u16, u16)> = None;
    const DOUBLE_CLICK_THRESHOLD_MS: u128 = 500;

    // Clickable registry for mouse handling
    let clickable_registry = ClickableRegistry::new();

    loop {
        let registry_clone = clickable_registry.clone();
        terminal.draw(|f| render_ui(f, state, &registry_clone))?;

        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    // Only handle key press events, not release events
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // Handle dialog input first
                    if state.is_dialog_open() {
                        let should_save = handle_dialog_input(state, key.code, key.modifiers);
                        if should_save {
                            match state.dialog_mode {
                                DialogMode::EditRow => handle_save_row(state).await,
                                DialogMode::AddRow => handle_insert_row(state).await,
                                _ => {}
                            }
                        }
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

                        // Tab navigation: Ctrl+Tab for next tab, Ctrl+Shift+Tab for previous
                        KeyCode::Tab
                            if state.active_panel == ActivePanel::QueryEditor
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                                && key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            state.query_tabs.prev_tab();
                        }
                        KeyCode::Tab
                            if state.active_panel == ActivePanel::QueryEditor
                                && key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            state.query_tabs.next_tab();
                        }

                        // Panel navigation (Tab without modifiers)
                        KeyCode::Tab if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.next_panel()
                        }
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
                            handle_connect(terminal, state).await;
                        }

                        // Select table or toggle schema - generate SELECT query
                        KeyCode::Enter if state.active_panel == ActivePanel::Tables => {
                            if state.selected_table == 0 {
                                // Toggle schema expansion
                                state.toggle_schema();
                            } else if let Some(table_name) = state.get_selected_table_full_name() {
                                // Generate SELECT query with proper syntax for each DB type
                                let query =
                                    if let Some(ref config) = state.current_connection_config {
                                        match config.db_type {
                                            crate::models::DatabaseType::SQLServer => {
                                                format!("SELECT TOP 100 * FROM {};", table_name)
                                            }
                                            _ => {
                                                format!("SELECT * FROM {} LIMIT 100;", table_name)
                                            }
                                        }
                                    } else {
                                        format!("SELECT * FROM {} LIMIT 100;", table_name)
                                    };
                                state.set_query(query);
                                state.active_panel = ActivePanel::QueryEditor;
                            }
                        }

                        // Toggle schema expansion with Space
                        KeyCode::Char(' ') if state.active_panel == ActivePanel::Tables => {
                            if state.selected_table == 0 {
                                state.toggle_schema();
                            }
                        }

                        // Edit selected row in Results panel
                        KeyCode::Enter if state.active_panel == ActivePanel::Results => {
                            state.open_edit_row_dialog();
                        }

                        // Add new row in Results panel
                        KeyCode::Char('a') if state.active_panel == ActivePanel::Results => {
                            if state.query_result.is_some() {
                                state.open_add_row_dialog();
                            } else {
                                state.set_status("Execute a query first to add rows");
                            }
                        }

                        // Horizontal scroll in Results panel (scroll by columns)
                        KeyCode::Left if state.active_panel == ActivePanel::Results => {
                            state.results_scroll_x = state.results_scroll_x.saturating_sub(1);
                        }
                        KeyCode::Right if state.active_panel == ActivePanel::Results => {
                            // Limit scroll to number of columns
                            if let Some(ref result) = state.query_result {
                                if state.results_scroll_x < result.columns.len().saturating_sub(1) {
                                    state.results_scroll_x += 1;
                                }
                            }
                        }
                        KeyCode::Home if state.active_panel == ActivePanel::Results => {
                            state.results_scroll_x = 0;
                        }
                        KeyCode::End if state.active_panel == ActivePanel::Results => {
                            if let Some(ref result) = state.query_result {
                                state.results_scroll_x = result.columns.len().saturating_sub(1);
                            }
                        }

                        // Execute query
                        KeyCode::F(5) => {
                            handle_execute_query(state).await;
                        }

                        // Execute current query at cursor with Ctrl+Enter
                        KeyCode::Enter
                            if state.active_panel == ActivePanel::QueryEditor
                                && key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            handle_execute_current_query(state).await;
                        }

                        // Newline in query editor
                        KeyCode::Enter if state.active_panel == ActivePanel::QueryEditor => {
                            let pos = state.cursor_position();
                            state.query_input_mut().insert(pos, '\n');
                            state.set_cursor_position(pos + 1);
                            state.query_tabs.current_tab_mut().is_modified = true;
                        }

                        // Query editor input (exclude Ctrl combinations)
                        KeyCode::Char(c)
                            if state.active_panel == ActivePanel::QueryEditor
                                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            let pos = state.cursor_position();
                            state.query_input_mut().insert(pos, c);
                            state.set_cursor_position(pos + 1);
                            state.query_tabs.current_tab_mut().is_modified = true;
                        }
                        KeyCode::Backspace if state.active_panel == ActivePanel::QueryEditor => {
                            let pos = state.cursor_position();
                            if pos > 0 {
                                state.set_cursor_position(pos - 1);
                                state.query_input_mut().remove(pos - 1);
                                state.query_tabs.current_tab_mut().is_modified = true;
                            }
                        }
                        KeyCode::Delete if state.active_panel == ActivePanel::QueryEditor => {
                            let pos = state.cursor_position();
                            if pos < state.query_input().len() {
                                state.query_input_mut().remove(pos);
                                state.query_tabs.current_tab_mut().is_modified = true;
                            }
                        }
                        KeyCode::Left if state.active_panel == ActivePanel::QueryEditor => {
                            let pos = state.cursor_position();
                            state.set_cursor_position(pos.saturating_sub(1));
                        }
                        KeyCode::Right if state.active_panel == ActivePanel::QueryEditor => {
                            let pos = state.cursor_position();
                            if pos < state.query_input().len() {
                                state.set_cursor_position(pos + 1);
                            }
                        }
                        KeyCode::Up if state.active_panel == ActivePanel::QueryEditor => {
                            move_cursor_up(state);
                        }
                        KeyCode::Down if state.active_panel == ActivePanel::QueryEditor => {
                            move_cursor_down(state);
                        }
                        KeyCode::Home if state.active_panel == ActivePanel::QueryEditor => {
                            // Move to start of current line
                            let pos = state.cursor_position();
                            let query = state.query_input().to_string();
                            let before_cursor = &query[..pos];
                            if let Some(line_start) = before_cursor.rfind('\n') {
                                state.set_cursor_position(line_start + 1);
                            } else {
                                state.set_cursor_position(0);
                            }
                        }
                        KeyCode::End if state.active_panel == ActivePanel::QueryEditor => {
                            // Move to end of current line
                            let pos = state.cursor_position();
                            let query = state.query_input().to_string();
                            let after_cursor = &query[pos..];
                            if let Some(line_end) = after_cursor.find('\n') {
                                state.set_cursor_position(pos + line_end);
                            } else {
                                state.set_cursor_position(query.len());
                            }
                        }

                        // New tab: Ctrl+T
                        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.query_tabs.add_tab();
                            state.active_panel = ActivePanel::QueryEditor;
                        }

                        // Close tab: Ctrl+W
                        KeyCode::Char('w')
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && state.active_panel == ActivePanel::QueryEditor =>
                        {
                            if !state.query_tabs.close_current_tab() {
                                state.set_status("Cannot close the last tab");
                            }
                        }

                        // Switch to tab by number: Ctrl+1 to Ctrl+9
                        KeyCode::Char(c @ '1'..='9')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            let tab_idx = (c as usize) - ('1' as usize);
                            if tab_idx < state.query_tabs.tabs.len() {
                                state.query_tabs.switch_to_tab(tab_idx);
                                state.active_panel = ActivePanel::QueryEditor;
                            }
                        }

                        // Save tabs: Ctrl+S
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.save_query_tabs();
                            state.query_tabs.current_tab_mut().is_modified = false;
                            state.set_status("Queries saved");
                        }

                        // New connection dialog
                        KeyCode::Char('n') if state.active_panel == ActivePanel::Connections => {
                            state.open_new_connection_dialog();
                        }
                        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.open_new_connection_dialog();
                        }

                        // Edit connection dialog
                        KeyCode::Char('e') if state.active_panel == ActivePanel::Connections => {
                            if !state.config.connections.is_empty() {
                                state.open_edit_connection_dialog(state.selected_connection);
                            }
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
                Event::Mouse(mouse) => {
                    if !state.is_dialog_open() {
                        handle_mouse_event(
                            state,
                            mouse,
                            terminal,
                            &mut last_click,
                            DOUBLE_CLICK_THRESHOLD_MS,
                            &clickable_registry,
                        )
                        .await;
                    }
                }
                _ => {}
            }
        }

        if state.should_quit {
            break;
        }
    }

    Ok(())
}

/// Handle mouse click events
async fn handle_mouse_event<B: ratatui::backend::Backend>(
    state: &mut AppState,
    mouse: crossterm::event::MouseEvent,
    terminal: &mut Terminal<B>,
    last_click: &mut Option<(Instant, u16, u16)>,
    double_click_threshold_ms: u128,
    registry: &ClickableRegistry,
) {
    use ui::ClickableType;

    let x = mouse.column;
    let y = mouse.row;

    // Handle scroll events
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            handle_scroll(state, x, y, registry, -1);
            return;
        }
        MouseEventKind::ScrollDown => {
            handle_scroll(state, x, y, registry, 1);
            return;
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Continue with click handling below
        }
        _ => return,
    }

    // Find what was clicked using the registry
    let clicked_item = registry.find_at(x, y);

    // Check for double-click
    let is_double_click = if let Some((last_time, last_x, last_y)) = *last_click {
        let elapsed = last_time.elapsed().as_millis();
        elapsed < double_click_threshold_ms && x == last_x && y == last_y
    } else {
        false
    };

    if is_double_click {
        // Reset last click and handle double-click
        *last_click = None;

        match clicked_item {
            Some(ClickableType::Connection(idx)) => {
                state.active_panel = ActivePanel::Connections;
                state.selected_connection = idx;
                handle_connect(terminal, state).await;
            }
            Some(ClickableType::Schema(schema_idx)) => {
                state.active_panel = ActivePanel::Tables;
                state.selected_schema = schema_idx;
                state.selected_table = 0;
                state.toggle_schema();
            }
            Some(ClickableType::Table {
                schema_idx,
                table_idx,
            }) => {
                state.active_panel = ActivePanel::Tables;
                state.selected_schema = schema_idx;
                state.selected_table = table_idx + 1;

                // Generate SELECT query
                if let Some(table_name) = state.get_selected_table_full_name() {
                    let query = if let Some(ref config) = state.current_connection_config {
                        match config.db_type {
                            crate::models::DatabaseType::SQLServer => {
                                format!("SELECT TOP 100 * FROM {};", table_name)
                            }
                            _ => {
                                format!("SELECT * FROM {} LIMIT 100;", table_name)
                            }
                        }
                    } else {
                        format!("SELECT * FROM {} LIMIT 100;", table_name)
                    };
                    state.set_query(query);
                    state.active_panel = ActivePanel::QueryEditor;
                }
            }
            Some(ClickableType::ResultRow(row_idx)) => {
                state.active_panel = ActivePanel::Results;
                if let Some(ref result) = state.query_result {
                    if row_idx < result.rows.len() {
                        state.selected_row = row_idx;
                        state.open_edit_row_dialog();
                    }
                }
            }
            _ => {}
        }
    } else {
        // Record this click and handle single-click
        *last_click = Some((Instant::now(), x, y));

        match clicked_item {
            Some(ClickableType::Connection(idx)) => {
                state.active_panel = ActivePanel::Connections;
                if idx < state.config.connections.len() {
                    state.selected_connection = idx;
                }
            }
            Some(ClickableType::Schema(schema_idx)) => {
                state.active_panel = ActivePanel::Tables;
                state.selected_schema = schema_idx;
                state.selected_table = 0;
            }
            Some(ClickableType::Table {
                schema_idx,
                table_idx,
            }) => {
                state.active_panel = ActivePanel::Tables;
                state.selected_schema = schema_idx;
                state.selected_table = table_idx + 1;
            }
            Some(ClickableType::QueryEditor) => {
                state.active_panel = ActivePanel::QueryEditor;
                // Calculate cursor position from click
                if let Some(editor_rect) = registry.get_query_editor_rect() {
                    let click_line = y.saturating_sub(editor_rect.y) as usize;
                    let click_col = x.saturating_sub(editor_rect.x) as usize;
                    let inner_width = editor_rect.width as usize;
                    let query = state.query_input().to_string();
                    let new_cursor_pos =
                        calculate_cursor_from_click(&query, click_line, click_col, inner_width);
                    state.set_cursor_position(new_cursor_pos);
                }
            }
            Some(ClickableType::ResultRow(row_idx)) => {
                state.active_panel = ActivePanel::Results;
                if let Some(ref result) = state.query_result {
                    if row_idx < result.rows.len() {
                        state.selected_row = row_idx;
                    }
                }
            }
            Some(ClickableType::QueryTab(tab_idx)) => {
                state.active_panel = ActivePanel::QueryEditor;
                state.query_tabs.switch_to_tab(tab_idx);
            }
            Some(ClickableType::Panel(panel_type)) => {
                use ui::PanelType;
                match panel_type {
                    PanelType::Connections => state.active_panel = ActivePanel::Connections,
                    PanelType::Tables => state.active_panel = ActivePanel::Tables,
                    PanelType::QueryEditor => state.active_panel = ActivePanel::QueryEditor,
                    PanelType::Results => state.active_panel = ActivePanel::Results,
                }
            }
            None => {}
        }
    }
}

/// Handle mouse scroll events
fn handle_scroll(
    state: &mut AppState,
    x: u16,
    y: u16,
    registry: &ClickableRegistry,
    direction: i32,
) {
    use ui::ClickableType;
    use ui::PanelType;

    // Find what panel we're scrolling in
    let item = registry.find_at(x, y);

    match item {
        Some(ClickableType::Connection(_)) | Some(ClickableType::Panel(PanelType::Connections)) => {
            // Scroll connections list
            let max_scroll = state.config.connections.len().saturating_sub(1);
            if direction > 0 {
                // Scroll down
                if state.connections_scroll < max_scroll {
                    state.connections_scroll += 1;
                }
            } else {
                // Scroll up
                state.connections_scroll = state.connections_scroll.saturating_sub(1);
            }
            // Keep selection visible
            if state.selected_connection < state.connections_scroll {
                state.selected_connection = state.connections_scroll;
            }
        }
        Some(ClickableType::Schema(_))
        | Some(ClickableType::Table { .. })
        | Some(ClickableType::Panel(PanelType::Tables)) => {
            // Scroll tables list
            let total_items = count_visible_table_items(state);
            let max_scroll = total_items.saturating_sub(1);
            if direction > 0 {
                // Scroll down
                if state.tables_scroll < max_scroll {
                    state.tables_scroll += 1;
                }
            } else {
                // Scroll up
                state.tables_scroll = state.tables_scroll.saturating_sub(1);
            }
        }
        Some(ClickableType::QueryEditor)
        | Some(ClickableType::QueryTab(_))
        | Some(ClickableType::Panel(PanelType::QueryEditor)) => {
            // Scroll query editor (move cursor up/down by lines)
            if direction > 0 {
                move_cursor_down(state);
            } else {
                move_cursor_up(state);
            }
        }
        Some(ClickableType::ResultRow(_)) | Some(ClickableType::Panel(PanelType::Results)) => {
            // Scroll results
            if let Some(ref result) = state.query_result {
                let max_scroll = result.rows.len().saturating_sub(1);
                if direction > 0 {
                    // Scroll down
                    if state.results_scroll < max_scroll {
                        state.results_scroll += 1;
                    }
                } else {
                    // Scroll up
                    state.results_scroll = state.results_scroll.saturating_sub(1);
                }
                // Keep selection visible
                if state.selected_row < state.results_scroll {
                    state.selected_row = state.results_scroll;
                }
            }
        }
        None => {}
    }
}

/// Count total visible items in tables panel (schemas + expanded tables)
fn count_visible_table_items(state: &AppState) -> usize {
    let mut count = 0;
    for schema in &state.schemas {
        count += 1; // Schema header
        if schema.expanded {
            count += schema.tables.len();
        }
    }
    count
}

/// Calculate cursor position from mouse click in query editor
fn calculate_cursor_from_click(
    text: &str,
    click_line: usize,
    click_col: usize,
    inner_width: usize,
) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut visual_line = 0;
    let mut visual_col = 0;
    let mut char_index = 0;

    for (i, c) in text.char_indices() {
        if visual_line == click_line && visual_col == click_col {
            return i;
        }

        if c == '\n' {
            if visual_line == click_line {
                // Click was beyond end of this line
                return i;
            }
            visual_line += 1;
            visual_col = 0;
        } else {
            visual_col += 1;
            if inner_width > 0 && visual_col >= inner_width {
                visual_line += 1;
                visual_col = 0;
            }
        }
        char_index = i + c.len_utf8();
    }

    // If click is beyond text, return end of text
    if visual_line == click_line && click_col >= visual_col {
        return char_index;
    }

    char_index
}

fn handle_dialog_input(state: &mut AppState, key: KeyCode, modifiers: KeyModifiers) -> bool {
    match state.dialog_mode {
        DialogMode::NewConnection | DialogMode::EditConnection => {
            handle_connection_dialog(state, key, modifiers);
            false
        }
        DialogMode::EditRow => handle_edit_row_dialog(state, key),
        DialogMode::AddRow => handle_add_row_dialog(state, key),
        DialogMode::DeleteConfirm => {
            // TODO: implement delete confirmation
            false
        }
        DialogMode::None => false,
    }
}

fn handle_connection_dialog(state: &mut AppState, key: KeyCode, _modifiers: KeyModifiers) {
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

            if let Some(index) = state.editing_connection_index {
                // Editing existing connection
                state.config.connections[index] = config;
                state.close_dialog();
                state.set_status(format!("Updated connection: {}", name));
            } else {
                // Adding new connection
                state.config.add_connection(config);
                state.close_dialog();
                state.set_status(format!("Added connection: {}", name));
            }
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

fn handle_edit_row_dialog(state: &mut AppState, key: KeyCode) -> bool {
    let row = match state.editing_row.as_mut() {
        Some(row) => row,
        None => return false,
    };
    let row_count = row.len();
    if row_count == 0 {
        return false;
    }

    match key {
        KeyCode::Esc => {
            state.close_dialog();
            false
        }
        KeyCode::Tab | KeyCode::Down => {
            // Move to next field
            state.editing_column = (state.editing_column + 1) % row_count;
            if let Some(ref row) = state.editing_row {
                state.editing_cursor = row[state.editing_column].len();
            }
            false
        }
        KeyCode::BackTab | KeyCode::Up => {
            // Move to previous field
            if state.editing_column == 0 {
                state.editing_column = row_count - 1;
            } else {
                state.editing_column -= 1;
            }
            if let Some(ref row) = state.editing_row {
                state.editing_cursor = row[state.editing_column].len();
            }
            false
        }
        KeyCode::Enter => {
            // Signal that we want to save - actual update will be done async
            true
        }
        KeyCode::Char(c) => {
            let pos = state.editing_cursor;
            if let Some(ref mut row) = state.editing_row {
                row[state.editing_column].insert(pos, c);
            }
            state.editing_cursor += 1;
            false
        }
        KeyCode::Backspace => {
            if state.editing_cursor > 0 {
                state.editing_cursor -= 1;
                if let Some(ref mut row) = state.editing_row {
                    row[state.editing_column].remove(state.editing_cursor);
                }
            }
            false
        }
        KeyCode::Delete => {
            if let Some(ref mut row) = state.editing_row {
                let len = row[state.editing_column].len();
                if state.editing_cursor < len {
                    row[state.editing_column].remove(state.editing_cursor);
                }
            }
            false
        }
        KeyCode::Home => {
            state.editing_cursor = 0;
            false
        }
        KeyCode::End => {
            if let Some(ref row) = state.editing_row {
                state.editing_cursor = row[state.editing_column].len();
            }
            false
        }
        KeyCode::Left => {
            state.editing_cursor = state.editing_cursor.saturating_sub(1);
            false
        }
        KeyCode::Right => {
            if let Some(ref row) = state.editing_row {
                let len = row[state.editing_column].len();
                if state.editing_cursor < len {
                    state.editing_cursor += 1;
                }
            }
            false
        }
        _ => false,
    }
}

fn handle_add_row_dialog(state: &mut AppState, key: KeyCode) -> bool {
    let row = match state.editing_row.as_ref() {
        Some(row) => row,
        None => return false,
    };
    let row_count = row.len();
    if row_count == 0 {
        return false;
    }

    // Find next/prev non-system column
    let find_next_editable = |current: usize| -> usize {
        for offset in 1..=row_count {
            let next = (current + offset) % row_count;
            if !state.system_columns.contains(&next) {
                return next;
            }
        }
        current // All columns are system columns, stay on current
    };

    let find_prev_editable = |current: usize| -> usize {
        for offset in 1..=row_count {
            let prev = if current >= offset {
                current - offset
            } else {
                row_count - (offset - current)
            };
            if !state.system_columns.contains(&prev) {
                return prev;
            }
        }
        current
    };

    match key {
        KeyCode::Esc => {
            state.close_dialog();
            false
        }
        KeyCode::Tab | KeyCode::Down => {
            state.editing_column = find_next_editable(state.editing_column);
            if let Some(ref row) = state.editing_row {
                state.editing_cursor = row[state.editing_column].len();
            }
            false
        }
        KeyCode::BackTab | KeyCode::Up => {
            state.editing_column = find_prev_editable(state.editing_column);
            if let Some(ref row) = state.editing_row {
                state.editing_cursor = row[state.editing_column].len();
            }
            false
        }
        KeyCode::Enter => {
            // Signal that we want to insert - actual insert will be done async
            true
        }
        KeyCode::Char(c) => {
            // Don't allow editing system columns
            if state.system_columns.contains(&state.editing_column) {
                return false;
            }
            let pos = state.editing_cursor;
            if let Some(ref mut row) = state.editing_row {
                row[state.editing_column].insert(pos, c);
            }
            state.editing_cursor += 1;
            false
        }
        KeyCode::Backspace => {
            if state.system_columns.contains(&state.editing_column) {
                return false;
            }
            if state.editing_cursor > 0 {
                state.editing_cursor -= 1;
                if let Some(ref mut row) = state.editing_row {
                    row[state.editing_column].remove(state.editing_cursor);
                }
            }
            false
        }
        KeyCode::Delete => {
            if state.system_columns.contains(&state.editing_column) {
                return false;
            }
            if let Some(ref mut row) = state.editing_row {
                let len = row[state.editing_column].len();
                if state.editing_cursor < len {
                    row[state.editing_column].remove(state.editing_cursor);
                }
            }
            false
        }
        KeyCode::Home => {
            state.editing_cursor = 0;
            false
        }
        KeyCode::End => {
            if let Some(ref row) = state.editing_row {
                state.editing_cursor = row[state.editing_column].len();
            }
            false
        }
        KeyCode::Left => {
            if !state.system_columns.contains(&state.editing_column) {
                state.editing_cursor = state.editing_cursor.saturating_sub(1);
            }
            false
        }
        KeyCode::Right => {
            if !state.system_columns.contains(&state.editing_column) {
                if let Some(ref row) = state.editing_row {
                    let len = row[state.editing_column].len();
                    if state.editing_cursor < len {
                        state.editing_cursor += 1;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

async fn handle_connect<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) {
    if state.config.connections.is_empty() {
        state.set_status("No connections configured. Press 'n' to add one.");
        return;
    }

    let conn_config = state.config.connections[state.selected_connection].clone();
    state.set_status(format!("Connecting to {}...", conn_config.name));
    state.is_loading = true;
    state.is_connecting = true;
    state.connection_error = None;

    // Redraw to show connecting state
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

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
            let error_msg = format!("Connection failed: {}", e);
            state.set_status(&error_msg);
            state.connection_error = Some(error_msg);
        }
    }

    state.is_loading = false;
    state.is_connecting = false;
}

async fn handle_refresh_tables(state: &mut AppState) {
    if let Some(ref conn) = state.connection {
        match conn.get_tables_by_schema().await {
            Ok(schemas) => {
                let total_tables: usize = schemas.iter().map(|s| s.tables.len()).sum();
                state.schemas = schemas;
                state.tables = Vec::new(); // Clear legacy flat list
                state.selected_schema = 0;
                state.selected_table = 0;
                state.set_status(format!(
                    "Loaded {} schemas, {} tables",
                    state.schemas.len(),
                    total_tables
                ));
            }
            Err(e) => {
                state.set_status(format!("Failed to fetch tables: {}", e));
            }
        }
    }
}

async fn handle_execute_query(state: &mut AppState) {
    if state.query_input().trim().is_empty() {
        state.set_status("Query is empty");
        return;
    }

    if state.connection.is_none() {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    }

    // Clone the query to avoid borrow issues
    let query = state.query_input().to_string();
    state.set_status("Executing query...");
    state.is_loading = true;

    let result = state
        .connection
        .as_ref()
        .unwrap()
        .execute_query(&query)
        .await;

    match result {
        Ok(mut result) => {
            let row_count = result.rows.len();
            let time = result.execution_time_ms;

            // Try to get column metadata for the table
            if let Some(table_name) = extract_table_from_query(&query) {
                // Get nullability info
                if let Ok(nullability) = state
                    .connection
                    .as_ref()
                    .unwrap()
                    .get_column_nullability(&table_name)
                    .await
                {
                    for col in &mut result.columns {
                        if let Some(&nullable) = nullability.get(&col.name) {
                            col.nullable = nullable;
                        }
                    }
                }

                // Get primary key info
                if let Ok(primary_keys) = state
                    .connection
                    .as_ref()
                    .unwrap()
                    .get_primary_keys(&table_name)
                    .await
                {
                    for col in &mut result.columns {
                        col.is_primary_key = primary_keys.contains(&col.name);
                    }
                }
            }

            state.query_result = Some(result);
            state.selected_row = 0;
            state.results_scroll_x = 0; // Reset horizontal scroll
            state.set_status(format!("Query executed: {} rows in {}ms", row_count, time));
            state.active_panel = ActivePanel::Results;
        }
        Err(e) => {
            state.set_status(format!("Query error: {}", e));
        }
    }

    state.is_loading = false;
}

/// Execute only the SQL statement at the current cursor position
async fn handle_execute_current_query(state: &mut AppState) {
    if state.query_input().trim().is_empty() {
        state.set_status("Query is empty");
        return;
    }

    if state.connection.is_none() {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    }

    // Find the query at the cursor position
    let query_text = state.query_input().to_string();
    let cursor_pos = state.cursor_position();
    let query = get_query_at_cursor(&query_text, cursor_pos);
    if query.trim().is_empty() {
        state.set_status("No query at cursor position");
        return;
    }

    state.set_status("Executing query...");
    state.is_loading = true;

    let result = state
        .connection
        .as_ref()
        .unwrap()
        .execute_query(&query)
        .await;

    match result {
        Ok(mut result) => {
            let row_count = result.rows.len();
            let time = result.execution_time_ms;

            // Try to get column metadata for the table
            if let Some(table_name) = extract_table_from_query(&query) {
                // Get nullability info
                if let Ok(nullability) = state
                    .connection
                    .as_ref()
                    .unwrap()
                    .get_column_nullability(&table_name)
                    .await
                {
                    for col in &mut result.columns {
                        if let Some(&nullable) = nullability.get(&col.name) {
                            col.nullable = nullable;
                        }
                    }
                }

                // Get primary key info
                if let Ok(primary_keys) = state
                    .connection
                    .as_ref()
                    .unwrap()
                    .get_primary_keys(&table_name)
                    .await
                {
                    for col in &mut result.columns {
                        col.is_primary_key = primary_keys.contains(&col.name);
                    }
                }
            }

            state.query_result = Some(result);
            state.selected_row = 0;
            state.results_scroll_x = 0;
            state.set_status(format!("Query executed: {} rows in {}ms", row_count, time));
            state.active_panel = ActivePanel::Results;
        }
        Err(e) => {
            state.set_status(format!("Query error: {}", e));
        }
    }

    state.is_loading = false;
}

/// Get the SQL statement at the given cursor position
/// Statements are separated by semicolons
fn get_query_at_cursor(input: &str, cursor_pos: usize) -> String {
    // Find statement boundaries (separated by ;)
    let mut start = 0;
    let mut end = input.len();

    // Find the start of the current statement
    for (i, c) in input[..cursor_pos].char_indices() {
        if c == ';' {
            start = i + 1;
        }
    }

    // Find the end of the current statement
    if let Some(semicolon_pos) = input[cursor_pos..].find(';') {
        end = cursor_pos + semicolon_pos;
    }

    input[start..end].trim().to_string()
}

/// Move cursor up one line in the query editor
fn move_cursor_up(state: &mut AppState) {
    let text = state.query_input().to_string();
    let cursor = state.cursor_position();

    // Find the start of the current line
    let line_start = text[..cursor].rfind('\n').map(|p| p + 1).unwrap_or(0);

    // Find the column position on the current line
    let col = cursor - line_start;

    // If we're on the first line, move to start
    if line_start == 0 {
        state.set_cursor_position(0);
        return;
    }

    // Find the start of the previous line
    let prev_line_end = line_start - 1; // Position of the \n
    let prev_line_start = text[..prev_line_end]
        .rfind('\n')
        .map(|p| p + 1)
        .unwrap_or(0);

    // Calculate the length of the previous line
    let prev_line_len = prev_line_end - prev_line_start;

    // Move to the same column on the previous line, or end of line if shorter
    state.set_cursor_position(prev_line_start + col.min(prev_line_len));
}

/// Move cursor down one line in the query editor
fn move_cursor_down(state: &mut AppState) {
    let text = state.query_input().to_string();
    let cursor = state.cursor_position();

    // Find the start of the current line
    let line_start = text[..cursor].rfind('\n').map(|p| p + 1).unwrap_or(0);

    // Find the column position on the current line
    let col = cursor - line_start;

    // Find the end of the current line
    let line_end = text[cursor..]
        .find('\n')
        .map(|p| cursor + p)
        .unwrap_or(text.len());

    // If we're on the last line, move to end
    if line_end == text.len() {
        state.set_cursor_position(text.len());
        return;
    }

    // Find the end of the next line
    let next_line_start = line_end + 1;
    let next_line_end = text[next_line_start..]
        .find('\n')
        .map(|p| next_line_start + p)
        .unwrap_or(text.len());

    // Calculate the length of the next line
    let next_line_len = next_line_end - next_line_start;

    // Move to the same column on the next line, or end of line if shorter
    state.set_cursor_position(next_line_start + col.min(next_line_len));
}

/// Extract table name from a query (simple heuristic for SELECT ... FROM table)
/// Supports schema.table format and quoted identifiers
fn extract_table_from_query(query: &str) -> Option<String> {
    let query_upper = query.to_uppercase();
    if let Some(from_pos) = query_upper.find("FROM") {
        let after_from = query[from_pos + 4..].trim_start();
        // Take the first word after FROM (including schema.table and quoted identifiers)
        let table_name: String = after_from
            .chars()
            .take_while(|c| {
                c.is_alphanumeric()
                    || *c == '_'
                    || *c == '.'
                    || *c == '['
                    || *c == ']'
                    || *c == '"'
                    || *c == '`'
            })
            .collect();
        if !table_name.is_empty() {
            return Some(table_name);
        }
    }
    None
}

/// Get the quote characters for the current database type
fn get_quote_chars(state: &AppState) -> (char, char) {
    match &state.current_connection_config {
        Some(config) => match config.db_type {
            models::DatabaseType::Postgres => ('"', '"'),
            models::DatabaseType::MySQL => ('`', '`'),
            models::DatabaseType::SQLite => ('"', '"'),
            models::DatabaseType::SQLServer => ('[', ']'),
        },
        None => ('"', '"'), // Default to double quotes
    }
}

async fn handle_save_row(state: &mut AppState) {
    // Get required data for update
    let table_name = match &state.editing_table_name {
        Some(name) => name.clone(),
        None => {
            state.set_status("Cannot save: table name not found");
            state.close_dialog();
            return;
        }
    };

    let columns = match &state.query_result {
        Some(result) => result.columns.clone(),
        None => {
            state.set_status("Cannot save: no query result");
            state.close_dialog();
            return;
        }
    };

    let original_values = match &state.original_editing_row {
        Some(row) => row.clone(),
        None => {
            state.set_status("Cannot save: original row not found");
            state.close_dialog();
            return;
        }
    };

    let new_values = match &state.editing_row {
        Some(row) => row.clone(),
        None => {
            state.set_status("Cannot save: edited row not found");
            state.close_dialog();
            return;
        }
    };

    // Check if there are any changes
    if original_values == new_values {
        state.set_status("No changes to save");
        state.close_dialog();
        return;
    }

    // Check if connected
    if state.connection.is_none() {
        state.set_status("Cannot save: not connected");
        state.close_dialog();
        return;
    }

    // Debug mode: show query in editor instead of executing
    if state.debug_mode {
        let quote_chars = get_quote_chars(state);
        if let Some(query) = db::utils::build_update_query(
            &table_name,
            &columns,
            &original_values,
            &new_values,
            quote_chars.0,
            quote_chars.1,
        ) {
            let query_len = query.len();
            state.set_query(query);
            state.set_cursor_position(query_len);
            state.set_status("Debug: UPDATE query copied to editor (not executed)");
        } else {
            state.set_status("Debug: No changes to generate query");
        }
        state.close_dialog();
    }

    state.set_status("Saving row...");

    // Perform the update
    let result = state
        .connection
        .as_ref()
        .unwrap()
        .update_row(&table_name, &columns, &original_values, &new_values)
        .await;

    match result {
        Ok(rows_affected) => {
            if rows_affected > 0 {
                // Update the row in the current result set
                if let Some(ref mut result) = state.query_result {
                    if let Some(row) = result.rows.get_mut(state.selected_row) {
                        *row = new_values;
                    }
                }
                state.set_status(format!("Row updated ({} row(s) affected)", rows_affected));
            } else {
                state.set_status("No rows were updated (row may have been modified)");
            }
        }
        Err(e) => {
            state.set_status(format!("Update failed: {}", e));
        }
    }

    state.close_dialog();
}

async fn handle_insert_row(state: &mut AppState) {
    // Get required data for insert
    let table_name = match &state.editing_table_name {
        Some(name) => name.clone(),
        None => {
            state.set_status("Cannot insert: table name not found");
            state.close_dialog();
            return;
        }
    };

    let columns = match &state.query_result {
        Some(result) => result.columns.clone(),
        None => {
            state.set_status("Cannot insert: no query result");
            state.close_dialog();
            return;
        }
    };

    let values = match &state.editing_row {
        Some(row) => row.clone(),
        None => {
            state.set_status("Cannot insert: row data not found");
            state.close_dialog();
            return;
        }
    };

    // Check if connected
    if state.connection.is_none() {
        state.set_status("Cannot insert: not connected");
        state.close_dialog();
        return;
    }

    // Get system columns to exclude from insert
    let system_cols = state.system_columns.clone();

    // Debug mode: show query in editor instead of executing
    if state.debug_mode {
        let quote_chars = get_quote_chars(state);
        if let Some(query) = db::utils::build_insert_query(
            &table_name,
            &columns,
            &values,
            &system_cols,
            quote_chars.0,
            quote_chars.1,
        ) {
            let query_len = query.len();
            state.set_query(query);
            state.set_cursor_position(query_len);
            state.set_status("Debug: INSERT query copied to editor (not executed)");
        } else {
            state.set_status("Debug: No columns to insert");
        }
        state.close_dialog();
    }

    state.set_status("Inserting row...");

    // Perform the insert
    let result = state
        .connection
        .as_ref()
        .unwrap()
        .insert_row(&table_name, &columns, &values, &system_cols)
        .await;

    match result {
        Ok(rows_affected) => {
            if rows_affected > 0 {
                state.set_status(format!(
                    "Row inserted ({} row(s) affected). Press F5 to refresh.",
                    rows_affected
                ));
            } else {
                state.set_status("No rows were inserted");
            }
        }
        Err(e) => {
            state.set_status(format!("Insert failed: {}", e));
        }
    }

    state.close_dialog();
}
