mod config;
mod db;
mod key_handlers;
mod models;
mod services;
mod ui;

use std::{
    fs::File,
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use update_notifier::check_version;

use config::AppConfig;
use db::DatabaseConnection;
use services::{ActivePanel, AppState, ColumnDefinition, ConnectionField, DialogMode};
use ui::{
    compute_active_panel_area, compute_modal_area, render_neon_border, render_ui,
    run_splash_screen, ClickableRegistry, ModalAnimation, PanelAnimations,
};

/// Find the previous char boundary from a byte position in a string.
/// Returns the byte index of the start of the previous character.
pub(crate) fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut idx = pos - 1;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Find the next char boundary from a byte position in a string.
/// Returns the byte index of the start of the next character.
pub(crate) fn next_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut idx = pos + 1;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let debug_mode = args.iter().any(|arg| arg == "--debug" || arg == "-d");
    let no_animations = args
        .iter()
        .any(|arg| arg == "--no-animations" || arg == "-na");
    let version = args.iter().any(|arg| arg == "--version" || arg == "-v");

    if version {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Setup logging — store log next to config in ~/.config/storing-unicorns/
    let log_path = dirs::config_dir()
        .expect("Could not determine config directory")
        .join("storing-unicorns");
    std::fs::create_dir_all(&log_path).expect("Could not create config directory");
    let file = File::create(log_path.join("debug.log"));
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
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut state = AppState::new(config, debug_mode, no_animations);

    if debug_mode {
        state.set_status("Debug mode enabled - queries will be shown in editor");
    }

    // Splash screen animation
    run_splash_screen(&mut terminal)?;

    // Main loop
    let res = run_app(&mut terminal, &mut state).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        cursor::Show
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

    // Check for updates after TUI exits so the message is visible in terminal
    check_version(
        &env!("CARGO_PKG_NAME").to_lowercase(),
        env!("CARGO_PKG_VERSION"),
        Duration::from_secs(60 * 60 * 24),
    )
    .ok();

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

    // Startup panel reveal animations
    let mut panel_animations: Option<PanelAnimations> = if state.no_animations {
        None
    } else {
        Some(PanelAnimations::new())
    };

    // Modal open animation
    let mut modal_animation: Option<ModalAnimation> = None;
    // Track which dialog was open last frame to detect open transitions
    let mut prev_dialog_mode = DialogMode::None;

    // Neon border: track app start time for continuous animation
    let app_start = Instant::now();

    loop {
        // Detect modal open transition
        if !state.no_animations
            && state.dialog_mode != DialogMode::None
            && state.dialog_mode != prev_dialog_mode
        {
            modal_animation = Some(ModalAnimation::new(state.dialog_mode));
        }
        if state.dialog_mode == DialogMode::None {
            modal_animation = None;
        }
        prev_dialog_mode = state.dialog_mode;

        {
            let elapsed_ms = app_start.elapsed().as_millis();
            let registry_clone = clickable_registry.clone();
            terminal.draw(|f| {
                render_ui(f, state, &registry_clone);

                // Apply panel reveal animations on top of rendered content
                if let Some(ref mut anims) = panel_animations {
                    anims.apply(f, state);
                }

                if !state.no_animations {
                    // Neon border on active panel
                    let panel_area = compute_active_panel_area(f.area(), state);
                    render_neon_border(f, panel_area, elapsed_ms);

                    // Neon border + animation on open modal
                    if state.dialog_mode != DialogMode::None {
                        let modal_area = compute_modal_area(f.area(), state.dialog_mode);
                        render_neon_border(f, modal_area, elapsed_ms);

                        if let Some(ref mut anim) = modal_animation {
                            anim.apply(f, modal_area);
                        }
                    }
                }
            })?;

            // Clean up animations once all done
            if panel_animations.as_ref().is_some_and(|a| a.all_done()) {
                panel_animations = None;
            }

            // results_visible_height is updated by render_results_panel each frame
        }

        // Use ~30fps poll for neon border animation; shorter during startup;
        // no continuous redraw needed when animations are disabled
        let poll_ms = if state.no_animations {
            250
        } else if panel_animations.is_some() {
            16
        } else {
            33
        };
        if event::poll(std::time::Duration::from_millis(poll_ms))? {
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
                                DialogMode::SchemaModify => handle_schema_action(state).await,
                                DialogMode::Export => handle_export(state),
                                DialogMode::Import => handle_import(terminal, state).await,
                                DialogMode::BatchExport => {
                                    handle_batch_export(terminal, state).await
                                }
                                DialogMode::BatchImport => {
                                    handle_batch_import(terminal, state).await
                                }
                                DialogMode::DeleteRowConfirm => handle_delete_row(state).await,
                                DialogMode::TruncateConfirm => handle_truncate_table(state).await,
                                DialogMode::BatchTruncate => {
                                    handle_batch_truncate(terminal, state).await
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }

                    // Handle filter input modes
                    if state.tables_filter_active || state.results_filter_active {
                        if key_handlers::handle_filter_keys(state, key.code)
                            == key_handlers::KeyAction::Consumed
                        {
                            continue;
                        }
                    }

                    // Dispatch to panel-specific handler, then fall through to global
                    let panel_result = match state.active_panel {
                        ActivePanel::Connections => {
                            let r = key_handlers::handle_connections_keys(state, key.code).await;
                            // handle_connect needs terminal, so handle Enter here
                            if r == key_handlers::KeyAction::NotHandled
                                && key.code == KeyCode::Enter
                            {
                                handle_connect(terminal, state).await;
                                key_handlers::KeyAction::Consumed
                            } else {
                                r
                            }
                        }
                        ActivePanel::Tables => {
                            key_handlers::handle_tables_keys(state, key.code, key.modifiers).await
                        }
                        ActivePanel::QueryEditor => {
                            key_handlers::handle_editor_keys(state, key.code, key.modifiers).await
                        }
                        ActivePanel::Results => {
                            key_handlers::handle_results_keys(state, key.code).await
                        }
                    };

                    // If panel didn't handle it, try global shortcuts
                    if panel_result == key_handlers::KeyAction::NotHandled {
                        key_handlers::handle_global_keys(state, key.code, key.modifiers).await;
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
                    PanelType::Results => {
                        // Only allow selecting Results panel if it's visible
                        if state.should_show_results() {
                            state.active_panel = ActivePanel::Results;
                        }
                    }
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
            // Scroll tables list (use filtered count)
            let total_items = count_filtered_table_items(state);
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
            // Scroll results - only if results are visible
            if state.should_show_results() {
                if let Some(ref _result) = state.query_result {
                    // Get filtered rows to account for filter
                    let filtered_rows = state.get_filtered_results().unwrap_or_default();
                    let max_scroll = filtered_rows.len().saturating_sub(1);
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
        }
        None => {}
    }
}

/// Count total items in the filtered tables view
fn count_filtered_table_items(state: &AppState) -> usize {
    let filtered = state.get_filtered_schemas();
    let mut count = 0;
    for (_schema_idx, schema, filtered_tables) in &filtered {
        count += 1; // Schema header
        if schema.expanded {
            count += filtered_tables.len();
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
        DialogMode::SchemaModify => handle_schema_dialog(state, key),
        DialogMode::Export => handle_export_dialog_input(state, key),
        DialogMode::Import => handle_import_dialog_input(state, key),
        DialogMode::BatchExport => handle_batch_export_dialog_input(state, key),
        DialogMode::BatchImport => handle_batch_import_dialog_input(state, key),
        DialogMode::DeleteRowConfirm => match key {
            KeyCode::Char('y') | KeyCode::Enter => true,
            KeyCode::Char('n') | KeyCode::Esc => {
                state.close_dialog();
                state.set_status("Delete cancelled");
                false
            }
            _ => false,
        },
        DialogMode::TruncateConfirm => match key {
            KeyCode::Char('y') | KeyCode::Enter => true,
            KeyCode::Char('n') | KeyCode::Esc => {
                state.truncate_table_name = None;
                state.close_dialog();
                state.set_status("Truncate cancelled");
                false
            }
            _ => false,
        },
        DialogMode::BatchTruncate => {
            handle_batch_truncate_dialog_input(state, key);
            // Return true when Enter is pressed and there are selected tables
            matches!(key, KeyCode::Enter)
                && state
                    .batch_truncate_state
                    .as_ref()
                    .is_some_and(|b| b.tables.iter().any(|(_, _, s)| *s))
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
            // Move to next field (skip Azure-specific fields if not Azure)
            nc.active_field = nc.active_field.next_for(&nc.db_type, &nc.azure_auth_method);
            nc.cursor_position = nc.get_active_field_value().len();
        }
        KeyCode::BackTab | KeyCode::Up => {
            // Move to previous field (skip Azure-specific fields if not Azure)
            nc.active_field = nc.active_field.prev_for(&nc.db_type, &nc.azure_auth_method);
            nc.cursor_position = nc.get_active_field_value().len();
        }
        KeyCode::Left if nc.active_field == ConnectionField::DbType => {
            nc.cycle_db_type();
        }
        KeyCode::Right if nc.active_field == ConnectionField::DbType => {
            nc.cycle_db_type();
        }
        KeyCode::Left if nc.active_field == ConnectionField::AzureAuth => {
            nc.cycle_azure_auth_method();
        }
        KeyCode::Right if nc.active_field == ConnectionField::AzureAuth => {
            nc.cycle_azure_auth_method();
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
            nc.cursor_position += c.len_utf8();
        }
        KeyCode::Backspace => {
            if nc.cursor_position > 0 {
                let field_val = nc.get_active_field_value().to_string();
                let prev = prev_char_boundary(&field_val, nc.cursor_position);
                if let Some(field) = nc.get_active_field_mut() {
                    field.remove(prev);
                }
                nc.cursor_position = prev;
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
            let field_val = nc.get_active_field_value().to_string();
            nc.cursor_position = prev_char_boundary(&field_val, nc.cursor_position);
        }
        KeyCode::Right => {
            let field_val = nc.get_active_field_value().to_string();
            nc.cursor_position = next_char_boundary(&field_val, nc.cursor_position);
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
            state.editing_cursor += c.len_utf8();
            false
        }
        KeyCode::Backspace => {
            if state.editing_cursor > 0 {
                let prev = if let Some(ref row) = state.editing_row {
                    prev_char_boundary(&row[state.editing_column], state.editing_cursor)
                } else {
                    state.editing_cursor.saturating_sub(1)
                };
                state.editing_cursor = prev;
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
            if let Some(ref row) = state.editing_row {
                state.editing_cursor =
                    prev_char_boundary(&row[state.editing_column], state.editing_cursor);
            }
            false
        }
        KeyCode::Right => {
            if let Some(ref row) = state.editing_row {
                state.editing_cursor =
                    next_char_boundary(&row[state.editing_column], state.editing_cursor);
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
            state.editing_cursor += c.len_utf8();
            false
        }
        KeyCode::Backspace => {
            if state.system_columns.contains(&state.editing_column) {
                return false;
            }
            if state.editing_cursor > 0 {
                let prev = if let Some(ref row) = state.editing_row {
                    prev_char_boundary(&row[state.editing_column], state.editing_cursor)
                } else {
                    state.editing_cursor.saturating_sub(1)
                };
                state.editing_cursor = prev;
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
                if let Some(ref row) = state.editing_row {
                    state.editing_cursor =
                        prev_char_boundary(&row[state.editing_column], state.editing_cursor);
                }
            }
            false
        }
        KeyCode::Right => {
            if !state.system_columns.contains(&state.editing_column) {
                if let Some(ref row) = state.editing_row {
                    state.editing_cursor =
                        next_char_boundary(&row[state.editing_column], state.editing_cursor);
                }
            }
            false
        }
        _ => false,
    }
}

fn handle_schema_dialog(state: &mut AppState, key: KeyCode) -> bool {
    use crate::services::ColumnDefinition;
    use crate::ui::modals::SchemaAction;

    // If no action is selected, handle the menu
    if state.schema_action.is_none() {
        match key {
            KeyCode::Esc => {
                state.close_dialog();
                false
            }
            KeyCode::Char('v') => {
                // View columns - will trigger async fetch
                state.schema_pending_operation = Some("view".to_string());
                true // Signal to fetch columns
            }
            KeyCode::Char('a') => {
                // Add column
                if let Some(table_name) = state.schema_table_name.clone() {
                    state.open_schema_action(SchemaAction::AddColumn {
                        table_name,
                        column: ColumnDefinition::default(),
                    });
                }
                false
            }
            KeyCode::Char('m') => {
                // Modify column - will trigger async fetch to select column
                state.schema_pending_operation = Some("modify".to_string());
                true
            }
            KeyCode::Char('r') => {
                // Rename column - will trigger async fetch to select column
                state.schema_pending_operation = Some("rename".to_string());
                true
            }
            KeyCode::Char('d') => {
                // Drop column - will trigger async fetch to select column
                state.schema_pending_operation = Some("drop".to_string());
                true
            }
            _ => false,
        }
    } else {
        // Handle specific action dialogs
        match &state.schema_action.clone() {
            Some(SchemaAction::ViewColumns { columns }) => match key {
                KeyCode::Esc => {
                    state.schema_action = None;
                    false
                }
                KeyCode::Up => {
                    if state.schema_field_index > 0 {
                        state.schema_field_index -= 1;
                    }
                    false
                }
                KeyCode::Down => {
                    if state.schema_field_index < columns.len().saturating_sub(1) {
                        state.schema_field_index += 1;
                    }
                    false
                }
                KeyCode::Enter => {
                    // Open modify dialog for selected column
                    if let Some(col) = columns.get(state.schema_field_index) {
                        if let Some(table_name) = state.schema_table_name.clone() {
                            state.schema_action = Some(SchemaAction::ModifyColumn {
                                table_name,
                                column: col.clone(),
                                original_name: col.name.clone(),
                            });
                            state.schema_field_index = 0;
                            state.schema_cursor_pos = col.name.len();
                        }
                    }
                    false
                }
                _ => false,
            },
            Some(SchemaAction::SelectColumn { columns, operation }) => match key {
                KeyCode::Esc => {
                    state.schema_action = None;
                    false
                }
                KeyCode::Up => {
                    if state.schema_field_index > 0 {
                        state.schema_field_index -= 1;
                    }
                    false
                }
                KeyCode::Down => {
                    if state.schema_field_index < columns.len().saturating_sub(1) {
                        state.schema_field_index += 1;
                    }
                    false
                }
                KeyCode::Enter => {
                    // Execute the selected operation on the selected column
                    if let Some(col) = columns.get(state.schema_field_index) {
                        if let Some(table_name) = state.schema_table_name.clone() {
                            match operation.as_str() {
                                "modify" => {
                                    state.schema_action = Some(SchemaAction::ModifyColumn {
                                        table_name,
                                        column: col.clone(),
                                        original_name: col.name.clone(),
                                    });
                                    state.schema_field_index = 0;
                                    state.schema_cursor_pos = col.name.len();
                                }
                                "drop" => {
                                    state.schema_action = Some(SchemaAction::DropColumn {
                                        table_name,
                                        column_name: col.name.clone(),
                                    });
                                }
                                "rename" => {
                                    state.schema_action = Some(SchemaAction::RenameColumn {
                                        table_name,
                                        old_name: col.name.clone(),
                                        new_name: col.name.clone(),
                                    });
                                    state.schema_cursor_pos = col.name.len();
                                }
                                _ => {}
                            }
                        }
                    }
                    false
                }
                _ => false,
            },
            Some(SchemaAction::AddColumn { column, .. })
            | Some(SchemaAction::ModifyColumn { column, .. }) => {
                handle_column_editor_input(state, key, column.clone())
            }
            Some(SchemaAction::DropColumn { .. }) => match key {
                KeyCode::Esc | KeyCode::Char('n') => {
                    state.schema_action = None;
                    false
                }
                KeyCode::Enter | KeyCode::Char('y') => {
                    // Execute drop
                    true
                }
                _ => false,
            },
            Some(SchemaAction::RenameColumn { new_name, .. }) => match key {
                KeyCode::Esc => {
                    state.schema_action = None;
                    false
                }
                KeyCode::Enter => {
                    // Execute rename
                    true
                }
                KeyCode::Char(c) => {
                    if let Some(SchemaAction::RenameColumn {
                        table_name,
                        old_name,
                        new_name,
                    }) = state.schema_action.take()
                    {
                        let mut new_name = new_name;
                        new_name.insert(state.schema_cursor_pos, c);
                        state.schema_cursor_pos += c.len_utf8();
                        state.schema_action = Some(SchemaAction::RenameColumn {
                            table_name,
                            old_name,
                            new_name,
                        });
                    }
                    false
                }
                KeyCode::Backspace => {
                    if state.schema_cursor_pos > 0 {
                        if let Some(SchemaAction::RenameColumn {
                            table_name,
                            old_name,
                            new_name,
                        }) = state.schema_action.take()
                        {
                            let mut new_name = new_name;
                            state.schema_cursor_pos =
                                prev_char_boundary(&new_name, state.schema_cursor_pos);
                            new_name.remove(state.schema_cursor_pos);
                            state.schema_action = Some(SchemaAction::RenameColumn {
                                table_name,
                                old_name,
                                new_name,
                            });
                        }
                    }
                    false
                }
                KeyCode::Left => {
                    state.schema_cursor_pos = prev_char_boundary(new_name, state.schema_cursor_pos);
                    false
                }
                KeyCode::Right => {
                    state.schema_cursor_pos = next_char_boundary(new_name, state.schema_cursor_pos);
                    false
                }
                _ => false,
            },
            None => false,
        }
    }
}

fn handle_column_editor_input(
    state: &mut AppState,
    key: KeyCode,
    current_column: services::ColumnDefinition,
) -> bool {
    match key {
        KeyCode::Esc => {
            state.schema_action = None;
            false
        }
        KeyCode::Tab | KeyCode::Down => {
            state.schema_field_index = (state.schema_field_index + 1) % 5;
            // Update cursor position for text fields
            match state.schema_field_index {
                0 => state.schema_cursor_pos = current_column.name.len(),
                1 => state.schema_cursor_pos = current_column.data_type.len(),
                4 => {
                    state.schema_cursor_pos = current_column
                        .default_value
                        .as_ref()
                        .map(|s| s.len())
                        .unwrap_or(0)
                }
                _ => state.schema_cursor_pos = 0,
            }
            false
        }
        KeyCode::BackTab | KeyCode::Up => {
            state.schema_field_index = if state.schema_field_index == 0 {
                4
            } else {
                state.schema_field_index - 1
            };
            match state.schema_field_index {
                0 => state.schema_cursor_pos = current_column.name.len(),
                1 => state.schema_cursor_pos = current_column.data_type.len(),
                4 => {
                    state.schema_cursor_pos = current_column
                        .default_value
                        .as_ref()
                        .map(|s| s.len())
                        .unwrap_or(0)
                }
                _ => state.schema_cursor_pos = 0,
            }
            false
        }
        KeyCode::Left | KeyCode::Right
            if state.schema_field_index == 2 || state.schema_field_index == 3 =>
        {
            // Toggle boolean fields
            let mut new_column = current_column.clone();
            if state.schema_field_index == 2 {
                new_column.nullable = !new_column.nullable;
            } else {
                new_column.is_primary_key = !new_column.is_primary_key;
            }
            update_schema_column(state, new_column);
            false
        }
        KeyCode::Enter => {
            // Execute add/modify
            true
        }
        KeyCode::Char(c)
            if state.schema_field_index == 0
                || state.schema_field_index == 1
                || state.schema_field_index == 4 =>
        {
            let mut new_column = current_column.clone();
            let pos = state.schema_cursor_pos;
            match state.schema_field_index {
                0 => {
                    new_column.name.insert(pos, c);
                }
                1 => {
                    new_column.data_type.insert(pos, c);
                }
                4 => {
                    if let Some(ref mut default) = new_column.default_value {
                        default.insert(pos, c);
                    } else {
                        new_column.default_value = Some(c.to_string());
                    }
                }
                _ => {}
            }
            state.schema_cursor_pos += c.len_utf8();
            update_schema_column(state, new_column);
            false
        }
        KeyCode::Backspace
            if state.schema_field_index == 0
                || state.schema_field_index == 1
                || state.schema_field_index == 4 =>
        {
            if state.schema_cursor_pos > 0 {
                let mut new_column = current_column.clone();
                let field_str: &str = match state.schema_field_index {
                    0 => &current_column.name,
                    1 => &current_column.data_type,
                    _ => current_column.default_value.as_deref().unwrap_or(""),
                };
                state.schema_cursor_pos = prev_char_boundary(field_str, state.schema_cursor_pos);
                match state.schema_field_index {
                    0 => {
                        new_column.name.remove(state.schema_cursor_pos);
                    }
                    1 => {
                        new_column.data_type.remove(state.schema_cursor_pos);
                    }
                    4 => {
                        if let Some(ref mut default) = new_column.default_value {
                            if !default.is_empty() {
                                default.remove(state.schema_cursor_pos);
                            }
                            if default.is_empty() {
                                new_column.default_value = None;
                            }
                        }
                    }
                    _ => {}
                }
                update_schema_column(state, new_column);
            }
            false
        }
        KeyCode::Left
            if state.schema_field_index == 0
                || state.schema_field_index == 1
                || state.schema_field_index == 4 =>
        {
            let field_str: &str = match state.schema_field_index {
                0 => &current_column.name,
                1 => &current_column.data_type,
                _ => current_column.default_value.as_deref().unwrap_or(""),
            };
            state.schema_cursor_pos = prev_char_boundary(field_str, state.schema_cursor_pos);
            false
        }
        KeyCode::Right
            if state.schema_field_index == 0
                || state.schema_field_index == 1
                || state.schema_field_index == 4 =>
        {
            let field_str: &str = match state.schema_field_index {
                0 => &current_column.name,
                1 => &current_column.data_type,
                _ => current_column.default_value.as_deref().unwrap_or(""),
            };
            state.schema_cursor_pos = next_char_boundary(field_str, state.schema_cursor_pos);
            false
        }
        _ => false,
    }
}

fn update_schema_column(state: &mut AppState, new_column: services::ColumnDefinition) {
    use crate::ui::modals::SchemaAction;

    match state.schema_action.take() {
        Some(SchemaAction::AddColumn { table_name, .. }) => {
            state.schema_action = Some(SchemaAction::AddColumn {
                table_name,
                column: new_column,
            });
        }
        Some(SchemaAction::ModifyColumn {
            table_name,
            original_name,
            ..
        }) => {
            state.schema_action = Some(SchemaAction::ModifyColumn {
                table_name,
                column: new_column,
                original_name,
            });
        }
        other => {
            state.schema_action = other;
        }
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

    let result = DatabaseConnection::connect(&conn_config).await;

    match result {
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

pub(crate) async fn handle_refresh_tables(state: &mut AppState) {
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

pub(crate) async fn handle_execute_query(state: &mut AppState) {
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
            state.update_known_columns(); // Update columns for autocompletion
            state.compute_col_widths(); // Cache column widths once
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
pub(crate) async fn handle_execute_current_query(state: &mut AppState) {
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
            state.update_known_columns(); // Update columns for autocompletion
            state.compute_col_widths(); // Cache column widths once
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
pub(crate) fn move_cursor_up(state: &mut AppState) {
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
pub(crate) fn move_cursor_down(state: &mut AppState) {
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
            models::DatabaseType::Azure => ('[', ']'),
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

/// Handle schema modification actions
async fn handle_schema_action(state: &mut AppState) {
    use crate::services::SchemaService;
    use crate::ui::modals::SchemaAction;

    let table_name = match &state.schema_table_name {
        Some(name) => name.clone(),
        None => {
            state.set_status("No table selected");
            state.close_dialog();
            return;
        }
    };

    let db_type = match &state.current_connection_config {
        Some(config) => config.db_type.clone(),
        None => {
            state.set_status("Not connected");
            state.close_dialog();
            return;
        }
    };

    // Handle action based on current state
    match &state.schema_action.clone() {
        None => {
            // Menu action - fetch columns for view/modify/rename/drop
            if let Some(columns) = fetch_table_columns(state, &table_name).await {
                let operation = state.schema_pending_operation.take();
                match operation.as_deref() {
                    Some("view") => {
                        state.open_schema_action(SchemaAction::ViewColumns { columns });
                    }
                    Some("modify") | Some("drop") | Some("rename") => {
                        state.open_schema_action(SchemaAction::SelectColumn {
                            columns,
                            operation: operation.unwrap_or_default(),
                        });
                    }
                    _ => {
                        // Default to view
                        state.open_schema_action(SchemaAction::ViewColumns { columns });
                    }
                }
            } else {
                state.set_status("Failed to fetch table columns");
                state.schema_pending_operation = None;
            }
        }
        Some(SchemaAction::ViewColumns { .. }) | Some(SchemaAction::SelectColumn { .. }) => {
            // Already viewing/selecting columns, nothing to do
        }
        Some(SchemaAction::AddColumn { column, .. }) => {
            if column.name.is_empty() {
                state.set_status("Column name is required");
                return;
            }

            let modification = services::SchemaModification::AddColumn {
                table_name: table_name.clone(),
                column: column.clone(),
            };

            let sql = SchemaService::generate_sql(&modification, &db_type);

            // Debug mode: show SQL in editor
            if state.debug_mode {
                let sql_len = sql.len();
                state.set_query(sql);
                state.set_cursor_position(sql_len);
                state.set_status("Debug: ALTER TABLE query copied to editor (not executed)");
                state.close_dialog();
                return;
            }

            // Execute the SQL
            if let Some(ref conn) = state.connection {
                match conn.execute_query(&sql).await {
                    Ok(_) => {
                        state.set_status(format!("Column '{}' added successfully", column.name));
                        // Invalidate cache and refresh autocomplete
                        state.table_cache.invalidate(&table_name).await;
                        state.current_table_context = None;
                        if let Some(cols) = fetch_table_columns(state, &table_name).await {
                            state.known_columns = cols.iter().map(|c| c.name.clone()).collect();
                        }
                    }
                    Err(e) => {
                        state.set_status(format!("Failed to add column: {}", e));
                    }
                }
            }
            state.close_dialog();
        }
        Some(SchemaAction::ModifyColumn {
            column,
            original_name,
            ..
        }) => {
            let modification = services::SchemaModification::ModifyColumn {
                table_name: table_name.clone(),
                column: column.clone(),
            };

            let sql = SchemaService::generate_sql(&modification, &db_type);

            if state.debug_mode {
                let sql_len = sql.len();
                state.set_query(sql);
                state.set_cursor_position(sql_len);
                state.set_status("Debug: ALTER TABLE query copied to editor (not executed)");
                state.close_dialog();
                return;
            }

            if let Some(ref conn) = state.connection {
                match conn.execute_query(&sql).await {
                    Ok(_) => {
                        state.set_status(format!(
                            "Column '{}' modified successfully",
                            original_name
                        ));
                        // Invalidate cache and refresh autocomplete
                        state.table_cache.invalidate(&table_name).await;
                        state.current_table_context = None;
                        if let Some(cols) = fetch_table_columns(state, &table_name).await {
                            state.known_columns = cols.iter().map(|c| c.name.clone()).collect();
                        }
                    }
                    Err(e) => {
                        state.set_status(format!("Failed to modify column: {}", e));
                    }
                }
            }
            state.close_dialog();
        }
        Some(SchemaAction::DropColumn { column_name, .. }) => {
            let modification = services::SchemaModification::DropColumn {
                table_name: table_name.clone(),
                column_name: column_name.clone(),
            };

            let sql = SchemaService::generate_sql(&modification, &db_type);

            if state.debug_mode {
                let sql_len = sql.len();
                state.set_query(sql);
                state.set_cursor_position(sql_len);
                state.set_status("Debug: DROP COLUMN query copied to editor (not executed)");
                state.close_dialog();
                return;
            }

            if let Some(ref conn) = state.connection {
                match conn.execute_query(&sql).await {
                    Ok(_) => {
                        state.set_status(format!("Column '{}' dropped successfully", column_name));
                        // Invalidate cache and refresh autocomplete
                        state.table_cache.invalidate(&table_name).await;
                        state.current_table_context = None;
                        if let Some(cols) = fetch_table_columns(state, &table_name).await {
                            state.known_columns = cols.iter().map(|c| c.name.clone()).collect();
                        }
                    }
                    Err(e) => {
                        state.set_status(format!("Failed to drop column: {}", e));
                    }
                }
            }
            state.close_dialog();
        }
        Some(SchemaAction::RenameColumn {
            old_name, new_name, ..
        }) => {
            if new_name.is_empty() {
                state.set_status("New column name is required");
                return;
            }

            let modification = services::SchemaModification::RenameColumn {
                table_name: table_name.clone(),
                old_name: old_name.clone(),
                new_name: new_name.clone(),
            };

            let sql = SchemaService::generate_sql(&modification, &db_type);

            if state.debug_mode {
                let sql_len = sql.len();
                state.set_query(sql);
                state.set_cursor_position(sql_len);
                state.set_status("Debug: RENAME COLUMN query copied to editor (not executed)");
                state.close_dialog();
                return;
            }

            if let Some(ref conn) = state.connection {
                match conn.execute_query(&sql).await {
                    Ok(_) => {
                        state
                            .set_status(format!("Column '{}' renamed to '{}'", old_name, new_name));
                        // Invalidate cache and refresh autocomplete
                        state.table_cache.invalidate(&table_name).await;
                        state.current_table_context = None;
                        if let Some(cols) = fetch_table_columns(state, &table_name).await {
                            state.known_columns = cols.iter().map(|c| c.name.clone()).collect();
                        }
                    }
                    Err(e) => {
                        state.set_status(format!("Failed to rename column: {}", e));
                    }
                }
            }
            state.close_dialog();
        }
    }
}

/// Fetch table columns asynchronously for autocompletion or schema modification
pub(crate) async fn fetch_table_columns(
    state: &mut AppState,
    table_name: &str,
) -> Option<Vec<ColumnDefinition>> {
    // Check cache first
    if let Some(columns) = state.table_cache.get_column_details(table_name).await {
        return Some(
            columns
                .into_iter()
                .map(|c| ColumnDefinition {
                    name: c.name,
                    data_type: c.type_name,
                    nullable: c.nullable,
                    is_primary_key: c.is_primary_key,
                    default_value: None,
                })
                .collect(),
        );
    }

    // Fetch from database
    if let Some(ref conn) = state.connection {
        match conn.get_table_column_details(table_name).await {
            Ok(columns) => {
                // Store in cache
                state
                    .table_cache
                    .set(table_name.to_string(), columns.clone())
                    .await;

                return Some(
                    columns
                        .into_iter()
                        .map(|c| ColumnDefinition {
                            name: c.name,
                            data_type: c.type_name,
                            nullable: c.nullable,
                            is_primary_key: c.is_primary_key,
                            default_value: None,
                        })
                        .collect(),
                );
            }
            Err(e) => {
                tracing::error!("Failed to fetch columns for {}: {}", table_name, e);
                state.set_status(format!("Failed to fetch columns: {}", e));
                return None;
            }
        }
    }

    None
}

/// Update completions with cached table columns
pub(crate) async fn update_completions_from_context(state: &mut AppState) {
    // Extract table name from current query
    let query = state.query_input().to_string();
    if let Some(table_name) = crate::ui::sql_highlight::extract_table_from_query(&query) {
        // Check if we need to fetch columns
        if state.current_table_context.as_ref() != Some(&table_name) {
            state.current_table_context = Some(table_name.clone());

            // Fetch columns asynchronously
            if let Some(columns) = fetch_table_columns(state, &table_name).await {
                state.known_columns = columns.iter().map(|c| c.name.clone()).collect();
            }
        }
    }
}

/// Handle export dialog input
fn handle_export_dialog_input(state: &mut AppState, key: KeyCode) -> bool {
    let export_state = match state.export_state.as_mut() {
        Some(s) => s,
        None => return false,
    };

    match key {
        KeyCode::Esc => {
            if export_state.path_completion.active {
                export_state.path_completion.dismiss();
                return false;
            }
            state.close_dialog();
            false
        }
        KeyCode::Tab if export_state.active_field == 1 => {
            // Path field: trigger or cycle completion
            if export_state.path_completion.active {
                export_state.path_completion.next();
            } else {
                export_state
                    .path_completion
                    .update_suggestions(&export_state.file_path);
                export_state.path_completion.active =
                    !export_state.path_completion.suggestions.is_empty();
            }
            false
        }
        KeyCode::Tab | KeyCode::Down => {
            export_state.path_completion.dismiss();
            export_state.active_field = (export_state.active_field + 1) % 2;
            if export_state.active_field == 1 {
                export_state.cursor_position = export_state.file_path.len();
            }
            false
        }
        KeyCode::BackTab | KeyCode::Up => {
            export_state.path_completion.dismiss();
            export_state.active_field = if export_state.active_field == 0 { 1 } else { 0 };
            if export_state.active_field == 1 {
                export_state.cursor_position = export_state.file_path.len();
            }
            false
        }
        KeyCode::Left if export_state.active_field == 0 => {
            export_state.format = export_state.format.next();
            export_state.update_extension();
            false
        }
        KeyCode::Right if export_state.active_field == 0 => {
            export_state.format = export_state.format.next();
            export_state.update_extension();
            false
        }
        KeyCode::Enter => {
            if export_state.path_completion.active {
                if let Some(suggestion) = export_state.path_completion.apply() {
                    export_state.file_path = suggestion;
                    export_state.cursor_position = export_state.file_path.len();
                }
                return false;
            }
            // Signal to perform export
            true
        }
        KeyCode::Char(c) if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            let pos = export_state.cursor_position;
            export_state.file_path.insert(pos, c);
            export_state.cursor_position += c.len_utf8();
            false
        }
        KeyCode::Backspace if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            if export_state.cursor_position > 0 {
                let prev =
                    prev_char_boundary(&export_state.file_path, export_state.cursor_position);
                export_state.file_path.remove(prev);
                export_state.cursor_position = prev;
            }
            false
        }
        KeyCode::Delete if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            let pos = export_state.cursor_position;
            if pos < export_state.file_path.len() {
                export_state.file_path.remove(pos);
            }
            false
        }
        KeyCode::Home if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            export_state.cursor_position = 0;
            false
        }
        KeyCode::End if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            export_state.cursor_position = export_state.file_path.len();
            false
        }
        KeyCode::Left if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            export_state.cursor_position =
                prev_char_boundary(&export_state.file_path, export_state.cursor_position);
            false
        }
        KeyCode::Right if export_state.active_field == 1 => {
            export_state.path_completion.dismiss();
            export_state.cursor_position =
                next_char_boundary(&export_state.file_path, export_state.cursor_position);
            false
        }
        _ => false,
    }
}

/// Handle import dialog input
fn handle_import_dialog_input(state: &mut AppState, key: KeyCode) -> bool {
    let import_state = match state.import_state.as_mut() {
        Some(s) => s,
        None => return false,
    };

    match key {
        KeyCode::Esc => {
            if import_state.path_completion.active {
                import_state.path_completion.dismiss();
                return false;
            }
            state.close_dialog();
            false
        }
        KeyCode::Tab if import_state.active_field == 0 => {
            // File path field: trigger or cycle completion
            if import_state.path_completion.active {
                import_state.path_completion.next();
            } else {
                import_state
                    .path_completion
                    .update_suggestions(&import_state.file_path);
                import_state.path_completion.active =
                    !import_state.path_completion.suggestions.is_empty();
            }
            false
        }
        KeyCode::Tab | KeyCode::Down => {
            import_state.path_completion.dismiss();
            import_state.active_field = (import_state.active_field + 1) % 2;
            import_state.cursor_position = match import_state.active_field {
                0 => import_state.file_path.len(),
                1 => import_state.target_table.len(),
                _ => 0,
            };
            false
        }
        KeyCode::BackTab | KeyCode::Up => {
            import_state.path_completion.dismiss();
            import_state.active_field = if import_state.active_field == 0 { 1 } else { 0 };
            import_state.cursor_position = match import_state.active_field {
                0 => import_state.file_path.len(),
                1 => import_state.target_table.len(),
                _ => 0,
            };
            false
        }
        KeyCode::Enter => {
            if import_state.path_completion.active {
                if let Some(suggestion) = import_state.path_completion.apply() {
                    import_state.file_path = suggestion;
                    import_state.cursor_position = import_state.file_path.len();
                }
                return false;
            }
            // Signal to perform import
            true
        }
        KeyCode::Char(c) => {
            import_state.path_completion.dismiss();
            let pos = import_state.cursor_position;
            let field = match import_state.active_field {
                0 => &mut import_state.file_path,
                1 => &mut import_state.target_table,
                _ => return false,
            };
            field.insert(pos, c);
            import_state.cursor_position += c.len_utf8();
            false
        }
        KeyCode::Backspace => {
            import_state.path_completion.dismiss();
            if import_state.cursor_position > 0 {
                let field = match import_state.active_field {
                    0 => &mut import_state.file_path,
                    1 => &mut import_state.target_table,
                    _ => return false,
                };
                let prev = prev_char_boundary(field, import_state.cursor_position);
                field.remove(prev);
                import_state.cursor_position = prev;
            }
            false
        }
        KeyCode::Delete => {
            import_state.path_completion.dismiss();
            let field = match import_state.active_field {
                0 => &mut import_state.file_path,
                1 => &mut import_state.target_table,
                _ => return false,
            };
            let pos = import_state.cursor_position;
            if pos < field.len() {
                field.remove(pos);
            }
            false
        }
        KeyCode::Home => {
            import_state.path_completion.dismiss();
            import_state.cursor_position = 0;
            false
        }
        KeyCode::End => {
            import_state.path_completion.dismiss();
            let field = match import_state.active_field {
                0 => &import_state.file_path,
                1 => &import_state.target_table,
                _ => return false,
            };
            import_state.cursor_position = field.len();
            false
        }
        KeyCode::Left => {
            import_state.path_completion.dismiss();
            let field = match import_state.active_field {
                0 => &import_state.file_path,
                1 => &import_state.target_table,
                _ => return false,
            };
            import_state.cursor_position = prev_char_boundary(field, import_state.cursor_position);
            false
        }
        KeyCode::Right => {
            import_state.path_completion.dismiss();
            let field = match import_state.active_field {
                0 => &import_state.file_path,
                1 => &import_state.target_table,
                _ => return false,
            };
            import_state.cursor_position = next_char_boundary(field, import_state.cursor_position);
            false
        }
        _ => false,
    }
}

/// Handle export action
fn handle_export(state: &mut AppState) {
    let export_state = match state.export_state.clone() {
        Some(s) => s,
        None => {
            state.set_status("Export error: no export state");
            state.close_dialog();
            return;
        }
    };

    let result = match &state.query_result {
        Some(r) => r,
        None => {
            state.set_status("No results to export");
            state.close_dialog();
            return;
        }
    };

    if export_state.file_path.is_empty() {
        state.set_status("File path is required");
        return;
    }

    let table_name = export_state.table_name.as_deref().unwrap_or("table");
    let (quote_start, quote_end) = get_quote_chars(state);

    match services::export_import::export_to_file(
        result,
        export_state.format,
        &export_state.file_path,
        table_name,
        quote_start,
        quote_end,
    ) {
        Ok(row_count) => {
            state.set_status(format!(
                "Exported {} rows to {} ({})",
                row_count,
                export_state.file_path,
                export_state.format.label()
            ));
        }
        Err(e) => {
            state.set_status(format!("Export failed: {}", e));
        }
    }

    state.close_dialog();
}

/// Handle import action
async fn handle_import<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) {
    let import_state = match state.import_state.clone() {
        Some(s) => s,
        None => {
            state.set_status("Import error: no import state");
            state.close_dialog();
            return;
        }
    };

    if import_state.file_path.is_empty() {
        state.set_status("File path is required");
        return;
    }

    if import_state.target_table.is_empty() {
        state.set_status("Target table name is required");
        return;
    }

    if state.connection.is_none() {
        state.set_status("Not connected to a database");
        state.close_dialog();
        return;
    }

    // Read the CSV file
    let content = match std::fs::read_to_string(&import_state.file_path) {
        Ok(c) => c,
        Err(e) => {
            state.set_status(format!("Failed to read file: {}", e));
            state.close_dialog();
            return;
        }
    };

    // Parse CSV
    let (columns, rows) = match services::export_import::parse_csv(&content) {
        Ok(data) => data,
        Err(e) => {
            state.set_status(format!("CSV parse error: {}", e));
            state.close_dialog();
            return;
        }
    };

    let (quote_start, quote_end) = get_quote_chars(state);
    let actions = services::export_import::build_upsert_import_actions(
        &import_state.target_table,
        &columns,
        &rows,
        quote_start,
        quote_end,
    );

    let total = actions.len();
    let mut success_count = 0;
    let mut update_count = 0;
    let mut insert_count = 0;
    let mut last_error: Option<String> = None;

    // Initialize progress
    if let Some(ref mut is) = state.import_state {
        is.import_progress = Some((0, total));
    }
    state.set_status(format!("Importing {} rows...", total));

    // Redraw to show initial progress
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    for (i, action) in actions.iter().enumerate() {
        let conn = state.connection.as_ref().unwrap();
        let result = match action {
            services::export_import::ImportAction::Upsert {
                update_query,
                insert_query,
            } => {
                // Try UPDATE first
                match conn.execute_query(update_query).await {
                    Ok(res) if res.rows_affected > 0 => {
                        update_count += 1;
                        Ok(())
                    }
                    Ok(_) => {
                        // No rows affected → element doesn't exist, INSERT without id
                        match conn.execute_query(insert_query).await {
                            Ok(_) => {
                                insert_count += 1;
                                Ok(())
                            }
                            Err(e) => Err(format!("{}", e)),
                        }
                    }
                    Err(e) => Err(format!("{}", e)),
                }
            }
            services::export_import::ImportAction::InsertOnly { query } => {
                match conn.execute_query(query).await {
                    Ok(_) => {
                        insert_count += 1;
                        Ok(())
                    }
                    Err(e) => Err(format!("{}", e)),
                }
            }
        };

        match result {
            Ok(()) => success_count += 1,
            Err(e) => {
                last_error = Some(e);
            }
        }

        // Update progress and redraw periodically (every 10 rows or last row)
        if i % 10 == 0 || i == total - 1 {
            if let Some(ref mut is) = state.import_state {
                is.import_progress = Some((i + 1, total));
            }
            let temp_registry = ClickableRegistry::new();
            let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));
        }
    }

    if success_count == total {
        state.set_status(format!(
            "Import complete: {} rows ({} updated, {} inserted) into {}",
            success_count, update_count, insert_count, import_state.target_table
        ));
    } else if let Some(err) = last_error {
        state.set_status(format!(
            "Import partial: {}/{} rows ({} updated, {} inserted). Last error: {}",
            success_count, total, update_count, insert_count, err
        ));
    } else {
        state.set_status(format!(
            "Import: {}/{} rows ({} updated, {} inserted)",
            success_count, total, update_count, insert_count
        ));
    }

    state.close_dialog();
}

/// Handle batch export dialog input
fn handle_batch_export_dialog_input(state: &mut AppState, key: KeyCode) -> bool {
    let batch = match state.batch_export_state.as_mut() {
        Some(s) => s,
        None => return false,
    };

    // Don't allow input while exporting
    if batch.progress.is_some() {
        return false;
    }

    match key {
        KeyCode::Esc => {
            if batch.path_completion.active {
                batch.path_completion.dismiss();
                return false;
            }
            state.close_dialog();
            false
        }
        KeyCode::Tab if batch.active_field == 1 => {
            // Directory field: trigger or cycle completion
            if batch.path_completion.active {
                batch.path_completion.next();
            } else {
                batch.path_completion.update_suggestions(&batch.directory);
                batch.path_completion.active = !batch.path_completion.suggestions.is_empty();
            }
            false
        }
        KeyCode::Tab | KeyCode::BackTab => {
            batch.path_completion.dismiss();
            let max_field = 2;
            if matches!(key, KeyCode::Tab) {
                batch.active_field = (batch.active_field + 1) % (max_field + 1);
            } else {
                batch.active_field = if batch.active_field == 0 {
                    max_field
                } else {
                    batch.active_field - 1
                };
            }
            // Update cursor position when switching to directory field
            if batch.active_field == 1 {
                batch.cursor_position = batch.directory.len();
            }
            false
        }
        KeyCode::Enter => {
            if batch.path_completion.active {
                if let Some(suggestion) = batch.path_completion.apply() {
                    batch.directory = suggestion;
                    batch.cursor_position = batch.directory.len();
                }
                return false;
            }
            // Start batch export
            let selected = batch.get_selected_tables();
            if selected.is_empty() {
                state.set_status("No tables selected for export");
                false
            } else {
                true
            }
        }
        // Format cycling (when on format field)
        KeyCode::Left if batch.active_field == 0 => {
            batch.format = batch.format.next();
            false
        }
        KeyCode::Right if batch.active_field == 0 => {
            batch.format = batch.format.next();
            false
        }
        // Directory field text input
        KeyCode::Char(c) if batch.active_field == 1 => {
            batch.path_completion.dismiss();
            let pos = batch.cursor_position;
            batch.directory.insert(pos, c);
            batch.cursor_position += c.len_utf8();
            false
        }
        KeyCode::Backspace if batch.active_field == 1 => {
            batch.path_completion.dismiss();
            if batch.cursor_position > 0 {
                let prev = prev_char_boundary(&batch.directory, batch.cursor_position);
                batch.directory.remove(prev);
                batch.cursor_position = prev;
            }
            false
        }
        KeyCode::Left if batch.active_field == 1 => {
            batch.path_completion.dismiss();
            batch.cursor_position = prev_char_boundary(&batch.directory, batch.cursor_position);
            false
        }
        KeyCode::Right if batch.active_field == 1 => {
            batch.path_completion.dismiss();
            batch.cursor_position = next_char_boundary(&batch.directory, batch.cursor_position);
            false
        }
        KeyCode::Home if batch.active_field == 1 => {
            batch.path_completion.dismiss();
            batch.cursor_position = 0;
            false
        }
        KeyCode::End if batch.active_field == 1 => {
            batch.path_completion.dismiss();
            batch.cursor_position = batch.directory.len();
            false
        }
        // Table list navigation
        KeyCode::Up if batch.active_field == 2 => {
            if batch.selected_index > 0 {
                batch.selected_index -= 1;
                if batch.selected_index < batch.scroll_offset {
                    batch.scroll_offset = batch.selected_index;
                }
            }
            false
        }
        KeyCode::Down if batch.active_field == 2 => {
            if batch.selected_index + 1 < batch.tables.len() {
                batch.selected_index += 1;
                // Auto-scroll (estimate visible height as 15)
                let visible = 15usize;
                if batch.selected_index >= batch.scroll_offset + visible {
                    batch.scroll_offset = batch.selected_index.saturating_sub(visible - 1);
                }
            }
            false
        }
        KeyCode::Char(' ') if batch.active_field == 2 => {
            batch.toggle_selected();
            false
        }
        KeyCode::Char('a') if batch.active_field == 2 => {
            batch.select_all();
            false
        }
        KeyCode::Char('n') if batch.active_field == 2 => {
            batch.deselect_all();
            false
        }
        _ => false,
    }
}

/// Handle batch import dialog input
fn handle_batch_import_dialog_input(state: &mut AppState, key: KeyCode) -> bool {
    let batch = match state.batch_import_state.as_mut() {
        Some(s) => s,
        None => return false,
    };

    // Don't allow input while importing
    if batch.progress.is_some() {
        return false;
    }

    match key {
        KeyCode::Esc => {
            if batch.path_completion.active {
                batch.path_completion.dismiss();
                return false;
            }
            state.close_dialog();
            false
        }
        KeyCode::Tab if batch.active_field == 0 => {
            // Directory field: trigger or cycle completion
            if batch.path_completion.active {
                batch.path_completion.next();
            } else {
                batch.path_completion.update_suggestions(&batch.directory);
                batch.path_completion.active = !batch.path_completion.suggestions.is_empty();
            }
            false
        }
        KeyCode::Tab | KeyCode::BackTab => {
            batch.path_completion.dismiss();
            let max_field = 1;
            if matches!(key, KeyCode::Tab) {
                batch.active_field = (batch.active_field + 1) % (max_field + 1);
            } else {
                batch.active_field = if batch.active_field == 0 {
                    max_field
                } else {
                    batch.active_field - 1
                };
            }
            if batch.active_field == 0 {
                batch.cursor_position = batch.directory.len();
            }
            false
        }
        KeyCode::Enter => {
            if batch.path_completion.active {
                if let Some(suggestion) = batch.path_completion.apply() {
                    batch.directory = suggestion;
                    batch.cursor_position = batch.directory.len();
                }
                batch.auto_select_matching_files();
                return false;
            }
            let selected = batch.get_selected_tables();
            if selected.is_empty() {
                state.set_status("No tables selected for import");
                false
            } else {
                true
            }
        }
        // Directory field text input
        KeyCode::Char(c) if batch.active_field == 0 => {
            batch.path_completion.dismiss();
            let pos = batch.cursor_position;
            batch.directory.insert(pos, c);
            batch.cursor_position += c.len_utf8();
            batch.auto_select_matching_files();
            false
        }
        KeyCode::Backspace if batch.active_field == 0 => {
            batch.path_completion.dismiss();
            if batch.cursor_position > 0 {
                let prev = prev_char_boundary(&batch.directory, batch.cursor_position);
                batch.directory.remove(prev);
                batch.cursor_position = prev;
            }
            batch.auto_select_matching_files();
            false
        }
        KeyCode::Left if batch.active_field == 0 => {
            batch.path_completion.dismiss();
            batch.cursor_position = prev_char_boundary(&batch.directory, batch.cursor_position);
            false
        }
        KeyCode::Right if batch.active_field == 0 => {
            batch.path_completion.dismiss();
            batch.cursor_position = next_char_boundary(&batch.directory, batch.cursor_position);
            false
        }
        KeyCode::Home if batch.active_field == 0 => {
            batch.path_completion.dismiss();
            batch.cursor_position = 0;
            false
        }
        KeyCode::End if batch.active_field == 0 => {
            batch.path_completion.dismiss();
            batch.cursor_position = batch.directory.len();
            false
        }
        // Table list navigation
        KeyCode::Up if batch.active_field == 1 => {
            if batch.selected_index > 0 {
                batch.selected_index -= 1;
                if batch.selected_index < batch.scroll_offset {
                    batch.scroll_offset = batch.selected_index;
                }
            }
            false
        }
        KeyCode::Down if batch.active_field == 1 => {
            if batch.selected_index + 1 < batch.tables.len() {
                batch.selected_index += 1;
                let visible = 15usize;
                if batch.selected_index >= batch.scroll_offset + visible {
                    batch.scroll_offset = batch.selected_index.saturating_sub(visible - 1);
                }
            }
            false
        }
        KeyCode::Char(' ') if batch.active_field == 1 => {
            batch.toggle_selected();
            false
        }
        KeyCode::Char('a') if batch.active_field == 1 => {
            batch.select_all();
            false
        }
        KeyCode::Char('n') if batch.active_field == 1 => {
            batch.deselect_all();
            false
        }
        _ => false,
    }
}

/// Handle batch export action
async fn handle_batch_export<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) {
    let batch = match state.batch_export_state.clone() {
        Some(s) => s,
        None => {
            state.set_status("Batch export error: no state");
            state.close_dialog();
            return;
        }
    };

    let selected_tables = batch.get_selected_tables();
    if selected_tables.is_empty() {
        state.set_status("No tables selected");
        state.close_dialog();
        return;
    }

    if state.connection.is_none() {
        state.set_status("Not connected to a database");
        state.close_dialog();
        return;
    }

    // Create directory if it doesn't exist
    let dir = std::path::PathBuf::from(&batch.directory);
    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            state.set_status(format!("Failed to create directory: {}", e));
            state.close_dialog();
            return;
        }
    }

    let total = selected_tables.len();
    let (quote_start, quote_end) = get_quote_chars(state);
    let mut success_count = 0;
    let mut last_error: Option<String> = None;

    for (i, (schema, table)) in selected_tables.iter().enumerate() {
        let clean_name = services::export_import::BatchExportState::clean_table_name(table);
        let full_table_name = format!("{0}{1}{2}.{0}{3}{2}", quote_start, schema, quote_end, table);

        // Update progress
        if let Some(ref mut bs) = state.batch_export_state {
            bs.progress = Some((i, total, table.clone()));
        }
        let temp_registry = ClickableRegistry::new();
        let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

        // Execute SELECT * FROM table
        let query = format!("SELECT * FROM {}", full_table_name);
        match state
            .connection
            .as_ref()
            .unwrap()
            .execute_query(&query)
            .await
        {
            Ok(result) => {
                let file_name = format!("{}.{}", clean_name, batch.format.extension());
                let file_path = dir.join(&file_name);

                match services::export_import::export_to_file(
                    &result,
                    batch.format,
                    file_path.to_str().unwrap_or(&file_name),
                    &full_table_name,
                    quote_start,
                    quote_end,
                ) {
                    Ok(_) => success_count += 1,
                    Err(e) => last_error = Some(format!("{}: {}", table, e)),
                }
            }
            Err(e) => {
                last_error = Some(format!("{}: {}", table, e));
            }
        }
    }

    // Final progress update
    if let Some(ref mut bs) = state.batch_export_state {
        bs.progress = Some((total, total, String::from("Done")));
    }
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    if success_count == total {
        state.set_status(format!(
            "Batch export complete: {} tables exported to {}",
            success_count, batch.directory
        ));
    } else if let Some(err) = last_error {
        state.set_status(format!(
            "Batch export partial: {}/{} tables. Last error: {}",
            success_count, total, err
        ));
    } else {
        state.set_status(format!(
            "Batch export: {}/{} tables exported",
            success_count, total
        ));
    }

    state.close_dialog();
}

/// Handle batch import action
async fn handle_batch_import<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) {
    let batch = match state.batch_import_state.clone() {
        Some(s) => s,
        None => {
            state.set_status("Batch import error: no state");
            state.close_dialog();
            return;
        }
    };

    let selected_tables = batch.get_selected_tables();
    if selected_tables.is_empty() {
        state.set_status("No tables selected");
        state.close_dialog();
        return;
    }

    if state.connection.is_none() {
        state.set_status("Not connected to a database");
        state.close_dialog();
        return;
    }

    let total = selected_tables.len();
    let (quote_start, quote_end) = get_quote_chars(state);
    let mut success_count = 0;
    let mut total_rows = 0usize;
    let mut total_updates = 0usize;
    let mut total_inserts = 0usize;
    let mut last_error: Option<String> = None;

    for (i, (schema, table)) in selected_tables.iter().enumerate() {
        let clean_name = services::export_import::BatchExportState::clean_table_name(table);
        let csv_path = std::path::Path::new(&batch.directory).join(format!("{}.csv", clean_name));

        // Update progress
        if let Some(ref mut bs) = state.batch_import_state {
            bs.progress = Some((i, total, table.clone()));
        }
        let temp_registry = ClickableRegistry::new();
        let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

        // Read CSV file
        let content = match std::fs::read_to_string(&csv_path) {
            Ok(c) => c,
            Err(e) => {
                last_error = Some(format!("{}: {}", table, e));
                continue;
            }
        };

        // Parse CSV
        let (columns, rows) = match services::export_import::parse_csv(&content) {
            Ok(data) => data,
            Err(e) => {
                last_error = Some(format!("{}: CSV parse error: {}", table, e));
                continue;
            }
        };

        let full_table_name = format!("{0}{1}{2}.{0}{3}{2}", quote_start, schema, quote_end, table);

        let actions = services::export_import::build_upsert_import_actions(
            &full_table_name,
            &columns,
            &rows,
            quote_start,
            quote_end,
        );

        let mut table_success = true;
        let mut table_updates = 0usize;
        let mut table_inserts = 0usize;
        for action in &actions {
            let conn = state.connection.as_ref().unwrap();
            let result = match action {
                services::export_import::ImportAction::Upsert {
                    update_query,
                    insert_query,
                } => match conn.execute_query(update_query).await {
                    Ok(res) if res.rows_affected > 0 => {
                        table_updates += 1;
                        Ok(())
                    }
                    Ok(_) => match conn.execute_query(insert_query).await {
                        Ok(_) => {
                            table_inserts += 1;
                            Ok(())
                        }
                        Err(e) => Err(format!("{}", e)),
                    },
                    Err(e) => Err(format!("{}", e)),
                },
                services::export_import::ImportAction::InsertOnly { query } => {
                    match conn.execute_query(query).await {
                        Ok(_) => {
                            table_inserts += 1;
                            Ok(())
                        }
                        Err(e) => Err(format!("{}", e)),
                    }
                }
            };

            match result {
                Ok(()) => {}
                Err(e) => {
                    last_error = Some(format!("{}: {}", table, e));
                    table_success = false;
                    break;
                }
            }
        }

        total_rows += table_updates + table_inserts;
        total_updates += table_updates;
        total_inserts += table_inserts;

        if table_success {
            success_count += 1;
        }
    }

    // Final progress update
    if let Some(ref mut bs) = state.batch_import_state {
        bs.progress = Some((total, total, String::from("Done")));
    }
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    if success_count == total {
        state.set_status(format!(
            "Batch import complete: {} tables, {} rows ({} updated, {} inserted) from {}",
            success_count, total_rows, total_updates, total_inserts, batch.directory
        ));
    } else if let Some(err) = last_error {
        state.set_status(format!(
            "Batch import partial: {}/{} tables, {} rows ({} updated, {} inserted). Last error: {}",
            success_count, total, total_rows, total_updates, total_inserts, err
        ));
    } else {
        state.set_status(format!(
            "Batch import: {}/{} tables, {} rows ({} updated, {} inserted)",
            success_count, total, total_rows, total_updates, total_inserts
        ));
    }

    state.close_dialog();
}

fn handle_batch_truncate_dialog_input(state: &mut AppState, key: KeyCode) {
    let Some(ref mut batch) = state.batch_truncate_state else {
        return;
    };

    match key {
        KeyCode::Esc => {
            state.batch_truncate_state = None;
            state.close_dialog();
        }
        KeyCode::Up => {
            if batch.selected_index > 0 {
                batch.selected_index -= 1;
                if batch.selected_index < batch.scroll_offset {
                    batch.scroll_offset = batch.selected_index;
                }
            }
        }
        KeyCode::Down => {
            if batch.selected_index + 1 < batch.tables.len() {
                batch.selected_index += 1;
                // Scroll will be adjusted by visible height check
            }
        }
        KeyCode::Char(' ') => {
            batch.toggle_selected();
        }
        KeyCode::Char('a') => {
            batch.select_all();
        }
        KeyCode::Char('n') => {
            batch.deselect_all();
        }
        KeyCode::Enter => {
            // Will be handled by async handler in event loop
        }
        _ => {}
    }
}

async fn handle_delete_row(state: &mut AppState) {
    let table_name = match &state.editing_table_name {
        Some(name) => name.clone(),
        None => {
            state.set_status("Cannot delete: table name not found");
            state.close_dialog();
            return;
        }
    };

    let columns = match &state.query_result {
        Some(result) => result.columns.clone(),
        None => {
            state.set_status("Cannot delete: no query result");
            state.close_dialog();
            return;
        }
    };

    let row_values = match &state.query_result {
        Some(result) => match result.rows.get(state.selected_row) {
            Some(row) => row.clone(),
            None => {
                state.set_status("Cannot delete: no row selected");
                state.close_dialog();
                return;
            }
        },
        None => {
            state.close_dialog();
            return;
        }
    };

    if state.connection.is_none() {
        state.set_status("Cannot delete: not connected");
        state.close_dialog();
        return;
    }

    let quote_chars = get_quote_chars(state);

    // Debug mode: show query
    if state.debug_mode {
        let query = db::utils::build_delete_query(
            &table_name,
            &columns,
            &row_values,
            quote_chars.0,
            quote_chars.1,
        );
        let query_len = query.len();
        state.set_query(query);
        state.set_cursor_position(query_len);
        state.set_status("Debug: DELETE query copied to editor (not executed)");
        state.close_dialog();
        return;
    }

    let query = db::utils::build_delete_query(
        &table_name,
        &columns,
        &row_values,
        quote_chars.0,
        quote_chars.1,
    );

    state.set_status("Deleting row...");

    let result = state
        .connection
        .as_ref()
        .unwrap()
        .execute_query(&query)
        .await;

    match result {
        Ok(result) => {
            if result.rows_affected > 0 {
                // Remove the row from the current result set
                if let Some(ref mut qr) = state.query_result {
                    if state.selected_row < qr.rows.len() {
                        qr.rows.remove(state.selected_row);
                        if state.selected_row >= qr.rows.len() && state.selected_row > 0 {
                            state.selected_row -= 1;
                        }
                    }
                }
                state.set_status(format!(
                    "Row deleted ({} row(s) affected)",
                    result.rows_affected
                ));
            } else {
                state.set_status("No rows were deleted (row may have been modified)");
            }
        }
        Err(e) => {
            state.set_status(format!("Delete failed: {}", e));
        }
    }

    state.close_dialog();
}

async fn handle_truncate_table(state: &mut AppState) {
    let table_name = match state.truncate_table_name.take() {
        Some(name) => name,
        None => {
            state.close_dialog();
            return;
        }
    };

    if state.connection.is_none() {
        state.set_status("Cannot truncate: not connected");
        state.close_dialog();
        return;
    }

    // Debug mode: show query
    if state.debug_mode {
        let query = format!("DELETE FROM {}", table_name);
        let query_len = query.len();
        state.set_query(query);
        state.set_cursor_position(query_len);
        state.set_status("Debug: DELETE FROM query copied to editor (not executed)");
        state.close_dialog();
        return;
    }

    state.set_status(format!("Deleting all data from {}...", table_name));

    let query = format!("DELETE FROM {}", table_name);
    let result = state
        .connection
        .as_ref()
        .unwrap()
        .execute_query(&query)
        .await;

    match result {
        Ok(result) => {
            state.set_status(format!(
                "Truncated {}: {} row(s) deleted",
                table_name, result.rows_affected
            ));
        }
        Err(e) => {
            state.set_status(format!("Truncate failed: {}", e));
        }
    }

    state.close_dialog();
}

async fn handle_batch_truncate<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) {
    let quote_chars = get_quote_chars(state);
    let batch = match state.batch_truncate_state.clone() {
        Some(b) => b,
        None => {
            state.close_dialog();
            return;
        }
    };

    let selected_tables = batch.get_selected_tables(quote_chars.0, quote_chars.1);

    if selected_tables.is_empty() {
        state.set_status("No tables selected for truncation");
        return;
    }

    if state.connection.is_none() {
        state.set_status("Cannot truncate: not connected");
        state.close_dialog();
        return;
    }

    // Debug mode: show queries
    if state.debug_mode {
        let queries: Vec<String> = selected_tables
            .iter()
            .map(|t| format!("DELETE FROM {};", t))
            .collect();
        let query = queries.join("\n");
        let query_len = query.len();
        state.set_query(query);
        state.set_cursor_position(query_len);
        state.set_status("Debug: DELETE FROM queries copied to editor (not executed)");
        state.close_dialog();
        return;
    }

    let total = selected_tables.len();
    let mut success_count = 0;
    let mut total_rows: u64 = 0;
    let mut last_error: Option<String> = None;

    for (i, table_name) in selected_tables.iter().enumerate() {
        // Update progress
        if let Some(ref mut bs) = state.batch_truncate_state {
            bs.progress = Some((i + 1, total, table_name.clone()));
        }
        let temp_registry = ClickableRegistry::new();
        let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

        let query = format!("DELETE FROM {}", table_name);
        match state
            .connection
            .as_ref()
            .unwrap()
            .execute_query(&query)
            .await
        {
            Ok(result) => {
                total_rows += result.rows_affected;
                success_count += 1;
            }
            Err(e) => {
                last_error = Some(format!("{}: {}", table_name, e));
            }
        }
    }

    // Final progress
    if let Some(ref mut bs) = state.batch_truncate_state {
        bs.progress = Some((total, total, String::from("Done")));
    }
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    if success_count == total {
        state.set_status(format!(
            "Batch truncate complete: {}/{} tables, {} total rows deleted",
            success_count, total, total_rows
        ));
    } else if let Some(err) = last_error {
        state.set_status(format!(
            "Batch truncate partial: {}/{} tables, {} rows deleted. Last error: {}",
            success_count, total, total_rows, err
        ));
    } else {
        state.set_status(format!(
            "Batch truncate: {}/{} tables, {} rows deleted",
            success_count, total, total_rows
        ));
    }

    state.close_dialog();
}
