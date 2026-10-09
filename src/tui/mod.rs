pub mod app_state;
mod key_handlers;
pub mod ui;

use crate::engine::{config, db, services};
pub use app_state::*;

use std::{
    io,
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

use config::AppConfig;
use db::DatabaseConnection;
use services::ColumnDefinition;
use crate::engine::ops::{
    self,
    query::{Executed, RunError},
};
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

pub async fn run(opts: crate::cli::TuiOptions) -> Result<()> {
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
    let mut state = AppState::new(config, opts.debug, opts.no_animations);

    if opts.debug {
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

    // Check for updates after the TUI exits so the message is visible
    // (at most once a day).
    let now = crate::updater::unix_now();
    if crate::updater::should_check(crate::updater::last_check(), now) {
        let skipped = state.config.skipped_version.clone();
        let check = tokio::task::spawn_blocking(crate::updater::fetch_latest);
        if let Ok(Ok(Ok(release))) = tokio::time::timeout(Duration::from_secs(5), check).await {
            crate::updater::record_check(now);
            if let Some(info) = release {
                if !crate::updater::is_skipped(&info, skipped.as_deref()) {
                    println!(
                        "storingUnicorns v{} is available (current v{}). Run `storingUnicorns update` to install it.",
                        info.version,
                        crate::updater::current_version()
                    );
                }
            }
        }
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
                            crate::engine::models::DatabaseType::SQLServer => {
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
        KeyCode::Left if nc.active_field == ConnectionField::Flavor => {
            nc.cycle_flavor_back();
        }
        KeyCode::Right if nc.active_field == ConnectionField::Flavor => {
            nc.cycle_flavor();
        }
        KeyCode::Left if nc.active_field == ConnectionField::SslMode => {
            nc.cycle_ssl_mode_back();
        }
        KeyCode::Right if nc.active_field == ConnectionField::SslMode => {
            nc.cycle_ssl_mode();
        }
        KeyCode::Enter if nc.active_field == ConnectionField::Url && !nc.url.trim().is_empty() => {
            // Fill the form from the pasted URL instead of saving.
            match nc.apply_url() {
                Ok(()) => {
                    nc.cursor_position = 0;
                    state.set_status("Champs remplis depuis l'URL");
                }
                Err(e) => state.set_status(e),
            }
        }
        KeyCode::Enter => {
            // Save the connection
            if let Err(e) = nc.check() {
                state.set_status(e);
                return;
            }
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
            // Cycle fields have no text: typing there does nothing.
            if let Some(field) = nc.get_active_field_mut() {
                field.insert(pos, c);
                nc.cursor_position += c.len_utf8();
            }
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
    use crate::engine::services::ColumnDefinition;
    use crate::tui::ui::modals::SchemaAction;

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
    use crate::tui::ui::modals::SchemaAction;

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
        match ops::query::refresh_schemas(conn).await {
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
    if state.connection.is_none() {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    }
    let query = state.query_input().to_string();
    state.set_status("Executing query...");
    state.is_loading = true;
    let result = ops::query::run_all(state.connection.as_ref().unwrap(), &query).await;
    state.is_loading = false;
    match result {
        Ok(result) => {
            let msg = format!(
                "Query executed: {} rows in {}ms",
                result.rows.len(),
                result.execution_time_ms
            );
            show_result(state, result, Some(query));
            state.set_status(msg);
        }
        Err(e) => state.set_status(format!("Query error: {e}")),
    }
}

/// Execute only the SQL statement (or transaction block) at the cursor.
pub(crate) async fn handle_execute_current_query(state: &mut AppState) {
    if state.connection.is_none() {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    }
    let text = state.query_input().to_string();
    let cursor = state.cursor_position();
    state.set_status("Executing query...");
    state.is_loading = true;
    let outcome =
        ops::query::run_at_cursor(state.connection.as_ref().unwrap(), &text, cursor).await;
    // The single statement that ran, if it was one.
    let sql = match crate::engine::sql::statements::get_execution_unit_at_cursor(&text, cursor) {
        crate::engine::sql::statements::ExecutionUnit::Single(sql) => Some(sql),
        _ => None,
    };
    state.is_loading = false;
    match outcome {
        Ok(Executed::Query(result)) => {
            let msg = format!(
                "Query executed: {} rows in {}ms",
                result.rows.len(),
                result.execution_time_ms
            );
            show_result(state, result, sql);
            state.set_status(msg);
        }
        Ok(Executed::Transaction { result, statements }) => {
            let msg = if result.rows.is_empty() {
                format!(
                    "Transaction committed: {statements} statements in {}ms",
                    result.execution_time_ms
                )
            } else {
                format!(
                    "Transaction committed: {} rows in {}ms",
                    result.rows.len(),
                    result.execution_time_ms
                )
            };
            show_result(state, result, None);
            state.set_status(msg);
        }
        Err(RunError::Query(e)) => state.set_status(format!("Query error: {e}")),
        Err(e) => state.set_status(e.to_string()),
    }
}

/// Show `result`, produced by `sql` (`None` for a transaction block: its
/// rows can't be edited).
fn show_result(
    state: &mut AppState,
    result: crate::engine::models::QueryResult,
    sql: Option<String>,
) {
    state.query_result = Some(result);
    state.result_sql = sql;
    state.update_known_columns(); // Update columns for autocompletion
    state.compute_col_widths(); // Cache column widths once
    state.selected_row = 0;
    state.results_scroll_x = 0; // Reset horizontal scroll
    state.active_panel = ActivePanel::Results;
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

/// Get the quote characters for the current database type
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
        Some(row) => row
            .iter()
            .zip(&original_values)
            .map(|(text, original)| app_state::cell_from_edit_text(text, original))
            .collect::<Vec<_>>(),
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
        let quote_chars = state.get_quote_chars();
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
        return;
    }

    state.set_status("Saving row...");

    // Perform the update
    let result = ops::rows::update_row(
        state.connection.as_ref().unwrap(),
        &table_name,
        &columns,
        &original_values,
        &new_values,
    )
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
        Some(row) => row.iter().map(|text| app_state::insert_cell(text)).collect::<Vec<_>>(),
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

    // Debug mode: show query in editor instead of executing
    if state.debug_mode {
        let db_type = state.connection.as_ref().unwrap().db_type();
        let query = db::utils::build_insert_query(&table_name, &columns, &values, &db_type);
        let query_len = query.len();
        state.set_query(query);
        state.set_cursor_position(query_len);
        state.set_status("Debug: INSERT query copied to editor (not executed)");
        state.close_dialog();
        return;
    }

    state.set_status("Inserting row...");

    // Perform the insert
    let result =
        ops::rows::insert_row(state.connection.as_ref().unwrap(), &table_name, &columns, &values)
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
    use crate::engine::services::SchemaService;
    use crate::tui::ui::modals::SchemaAction;

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
                match ops::schema::apply_modification(
                    conn,
                    &state.table_cache,
                    &modification,
                    &db_type,
                )
                .await
                {
                    Ok(_) => {
                        state.set_status(format!("Column '{}' added successfully", column.name));
                        // Cache already invalidated: refresh autocomplete
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
                match ops::schema::apply_modification(
                    conn,
                    &state.table_cache,
                    &modification,
                    &db_type,
                )
                .await
                {
                    Ok(_) => {
                        state.set_status(format!(
                            "Column '{}' modified successfully",
                            original_name
                        ));
                        // Cache already invalidated: refresh autocomplete
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
                match ops::schema::apply_modification(
                    conn,
                    &state.table_cache,
                    &modification,
                    &db_type,
                )
                .await
                {
                    Ok(_) => {
                        state.set_status(format!("Column '{}' dropped successfully", column_name));
                        // Cache already invalidated: refresh autocomplete
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
                match ops::schema::apply_modification(
                    conn,
                    &state.table_cache,
                    &modification,
                    &db_type,
                )
                .await
                {
                    Ok(_) => {
                        state
                            .set_status(format!("Column '{}' renamed to '{}'", old_name, new_name));
                        // Cache already invalidated: refresh autocomplete
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
    let conn = state.connection.as_ref()?;
    match ops::schema::fetch_columns(conn, &state.table_cache, table_name).await {
        Ok(cols) => Some(cols),
        Err(e) => {
            tracing::error!("Failed to fetch columns for {}: {}", table_name, e);
            state.set_status(format!("Failed to fetch columns: {}", e));
            None
        }
    }
}

/// Update completions with cached table columns
pub(crate) async fn update_completions_from_context(state: &mut AppState) {
    // Extract table name from current query
    let query = state.query_input().to_string();
    if let Some(table_name) = crate::tui::ui::sql_highlight::completion_context_table(&query) {
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
    let (quote_start, quote_end) = state.get_quote_chars();

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

    let quotes = state.get_quote_chars();
    state.set_status(format!("Importing into {}...", import_state.target_table));

    // Redraw to show the initial status
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    // The connection is taken out of the state while importing so the progress
    // callback can redraw the whole UI (rendering never reads the connection).
    let conn = state.connection.take().unwrap();
    let result = ops::transfer::import_csv(
        &conn,
        &import_state.target_table,
        &content,
        quotes,
        |done, total| {
            // Redraw periodically (every 10 rows and on the last row)
            if done % 10 == 1 || done == total {
                if let Some(ref mut is) = state.import_state {
                    is.import_progress = Some((done, total));
                }
                let temp_registry = ClickableRegistry::new();
                let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));
            }
        },
    )
    .await;
    state.connection = Some(conn);

    let stats = match result {
        Ok(s) => s,
        Err(e) => {
            state.set_status(e.to_string());
            state.close_dialog();
            return;
        }
    };

    let success_count = stats.succeeded();
    if success_count == stats.total {
        state.set_status(format!(
            "Import complete: {} rows ({} updated, {} inserted) into {}",
            success_count, stats.updated, stats.inserted, import_state.target_table
        ));
    } else if let Some(err) = stats.errors.last() {
        state.set_status(format!(
            "Import partial: {}/{} rows ({} updated, {} inserted). Last error: {}",
            success_count, stats.total, stats.updated, stats.inserted, err
        ));
    } else {
        state.set_status(format!(
            "Import: {}/{} rows ({} updated, {} inserted)",
            success_count, stats.total, stats.updated, stats.inserted
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
    let quotes = state.get_quote_chars();

    // See handle_import: the connection is taken out so progress can redraw.
    let conn = state.connection.take().unwrap();
    let report = ops::transfer::export_tables(
        &conn,
        &selected_tables,
        &dir,
        batch.format,
        quotes,
        |done, total, table| {
            if let Some(ref mut bs) = state.batch_export_state {
                bs.progress = Some((done, total, table.to_string()));
            }
            let temp_registry = ClickableRegistry::new();
            let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));
        },
    )
    .await;
    state.connection = Some(conn);

    // Final progress update
    if let Some(ref mut bs) = state.batch_export_state {
        bs.progress = Some((total, total, String::from("Done")));
    }
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    let success_count = report.succeeded;
    if success_count == total {
        state.set_status(format!(
            "Batch export complete: {} tables exported to {}",
            success_count, batch.directory
        ));
    } else if let Some(err) = report.errors.last() {
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
    let quotes = state.get_quote_chars();

    // See handle_import: the connection is taken out so progress can redraw.
    let conn = state.connection.take().unwrap();
    let report = ops::transfer::import_tables(
        &conn,
        &selected_tables,
        std::path::Path::new(&batch.directory),
        quotes,
        |done, total, table| {
            if let Some(ref mut bs) = state.batch_import_state {
                bs.progress = Some((done, total, table.to_string()));
            }
            let temp_registry = ClickableRegistry::new();
            let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));
        },
    )
    .await;
    state.connection = Some(conn);

    // Final progress update
    if let Some(ref mut bs) = state.batch_import_state {
        bs.progress = Some((total, total, String::from("Done")));
    }
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    let success_count = report.succeeded;
    let total_rows = report.rows_affected;
    if success_count == total {
        state.set_status(format!(
            "Batch import complete: {} tables, {} rows from {}",
            success_count, total_rows, batch.directory
        ));
    } else if let Some(err) = report.errors.last() {
        state.set_status(format!(
            "Batch import partial: {}/{} tables, {} rows. Last error: {}",
            success_count, total, total_rows, err
        ));
    } else {
        state.set_status(format!(
            "Batch import: {}/{} tables, {} rows",
            success_count, total, total_rows
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

    let quote_chars = state.get_quote_chars();

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

    state.set_status("Deleting row...");

    let result = ops::rows::delete_row(
        state.connection.as_ref().unwrap(),
        &table_name,
        &columns,
        &row_values,
        quote_chars,
    )
    .await;

    match result {
        Ok(rows_affected) => {
            if rows_affected > 0 {
                // Remove the row from the current result set
                if let Some(ref mut qr) = state.query_result {
                    if state.selected_row < qr.rows.len() {
                        qr.rows.remove(state.selected_row);
                        if state.selected_row >= qr.rows.len() && state.selected_row > 0 {
                            state.selected_row -= 1;
                        }
                    }
                }
                state.set_status(format!("Row deleted ({rows_affected} row(s) affected)"));
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
    let quote_chars = state.get_quote_chars();
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
    // See handle_import: the connection is taken out so progress can redraw.
    let conn = state.connection.take().unwrap();
    let report = ops::rows::truncate_tables(&conn, &selected_tables, |done, total, table| {
        if let Some(ref mut bs) = state.batch_truncate_state {
            bs.progress = Some((done, total, table.to_string()));
        }
        let temp_registry = ClickableRegistry::new();
        let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));
    })
    .await;
    state.connection = Some(conn);

    // Final progress
    if let Some(ref mut bs) = state.batch_truncate_state {
        bs.progress = Some((total, total, String::from("Done")));
    }
    let temp_registry = ClickableRegistry::new();
    let _ = terminal.draw(|f| render_ui(f, state, &temp_registry));

    if report.succeeded == report.total {
        state.set_status(format!(
            "Batch truncate complete: {}/{} tables, {} total rows deleted",
            report.succeeded, report.total, report.rows_affected
        ));
    } else if let Some(err) = report.errors.last() {
        state.set_status(format!(
            "Batch truncate partial: {}/{} tables, {} rows deleted. Last error: {}",
            report.succeeded, report.total, report.rows_affected, err
        ));
    } else {
        state.set_status(format!(
            "Batch truncate: {}/{} tables, {} rows deleted",
            report.succeeded, report.total, report.rows_affected
        ));
    }

    state.close_dialog();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{Column, QueryResult};
    use crate::engine::ops::test_support::{count, sqlite_mem};

    fn cols() -> Vec<Column> {
        ["id", "name"]
            .iter()
            .map(|n| Column {
                name: n.to_string(),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: *n == "id",
            })
            .collect()
    }

    async fn debug_state() -> AppState {
        let mut state = AppState::new(AppConfig::default(), true, true);
        state.connection = Some(
            sqlite_mem(&[
                "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
                "INSERT INTO t (id, name) VALUES (1, 'a')",
            ])
            .await,
        );
        state.editing_table_name = Some("t".into());
        state.query_result = Some(QueryResult {
            columns: cols(),
            rows: vec![vec!["1".into(), "a".into()]],
            ..QueryResult::default()
        });
        state
    }

    #[tokio::test]
    async fn row_dialogs_only_open_for_single_keyed_table_results() {
        let mut state = debug_state().await;
        state.query_result.as_mut().unwrap().primary_key = vec!["id".into()];
        let open = |state: &mut AppState, sql: &str| {
            state.result_sql = Some(sql.to_string());
            state.editing_table_name = None;
            state.dialog_mode = DialogMode::None;
            state.open_edit_row_dialog();
            state.editing_table_name.clone()
        };
        assert_eq!(open(&mut state, "SELECT * FROM t"), Some("t".into()));
        assert!(matches!(state.dialog_mode, DialogMode::EditRow));
        assert_eq!(open(&mut state, "SELECT valid_from, id, name FROM t"), Some("t".into()));
        assert_eq!(open(&mut state, "SELECT * FROM t JOIN u ON u.id = t.id"), None);
        assert!(matches!(state.dialog_mode, DialogMode::None));
        assert!(state.status_message.starts_with("Read-only"));
        state.query_result.as_mut().unwrap().primary_key.clear();
        assert_eq!(open(&mut state, "SELECT * FROM t"), None, "no primary key");
        state.result_sql = None;
        state.open_delete_row_confirm();
        assert!(matches!(state.dialog_mode, DialogMode::None));
    }

    #[test]
    fn row_editor_maps_null_at_its_boundary() {
        use crate::engine::models::NULL_CELL;
        use app_state::{cell_from_edit_text, cell_to_edit_text};
        assert_eq!(cell_to_edit_text(NULL_CELL), "NULL");
        assert_eq!(cell_to_edit_text("x"), "x");
        // Unchanged: the original value, NULL or the text "NULL".
        assert_eq!(cell_from_edit_text("NULL", NULL_CELL), NULL_CELL);
        assert_eq!(cell_from_edit_text("NULL", "NULL"), "NULL");
        // Typed NULL means NULL; anything else is text, empty included.
        assert_eq!(cell_from_edit_text("NULL", "a"), NULL_CELL);
        assert_eq!(cell_from_edit_text("", NULL_CELL), "");
        assert_eq!(cell_from_edit_text("b", "a"), "b");
    }

    #[tokio::test]
    async fn save_row_writes_typed_null_and_keeps_null_text() {
        use crate::engine::models::NULL_CELL;
        let mut state = debug_state().await;
        state.debug_mode = false;
        let conn = state.connection.as_ref().unwrap();
        conn.execute_query("INSERT INTO t (id, name) VALUES (2, 'NULL')")
            .await
            .unwrap();
        state.query_result.as_mut().unwrap().primary_key = vec!["id".into()];
        state.query_result.as_mut().unwrap().rows = vec![
            vec!["1".into(), "a".into()],
            vec!["2".into(), "NULL".into()],
        ];
        state.result_sql = Some("SELECT * FROM t".into());
        // Row 1: NULL typed.
        state.selected_row = 0;
        state.open_edit_row_dialog();
        state.editing_row.as_mut().unwrap()[1] = "NULL".into();
        handle_save_row(&mut state).await;
        // Row 2 holds the text "NULL": saved unchanged except its id column.
        state.selected_row = 1;
        state.open_edit_row_dialog();
        assert_eq!(state.editing_row.as_ref().unwrap()[1], "NULL");
        state.editing_row.as_mut().unwrap()[0] = "3".into();
        handle_save_row(&mut state).await;
        let conn = state.connection.as_ref().unwrap();
        let r = conn
            .execute_query("SELECT id, name FROM t ORDER BY id")
            .await
            .unwrap();
        assert_eq!(
            r.rows,
            vec![
                vec!["1".to_string(), NULL_CELL.to_string()],
                vec!["3".to_string(), "NULL".to_string()],
            ]
        );
    }

    #[tokio::test]
    async fn debug_mode_save_row_does_not_execute() {
        let mut state = debug_state().await;
        state.original_editing_row = Some(vec!["1".into(), "a".into()]);
        state.editing_row = Some(vec!["1".into(), "z".into()]);
        handle_save_row(&mut state).await;
        let conn = state.connection.as_ref().unwrap();
        let r = conn
            .execute_query("SELECT name FROM t WHERE id = 1")
            .await
            .unwrap();
        assert_eq!(r.rows[0][0], "a");
        assert!(state.status_message.starts_with("Debug:"));
    }

    #[tokio::test]
    async fn debug_mode_insert_row_does_not_execute() {
        let mut state = debug_state().await;
        state.editing_row = Some(vec!["2".into(), "b".into()]);
        handle_insert_row(&mut state).await;
        assert_eq!(count(state.connection.as_ref().unwrap(), "t").await, "1");
        assert!(state.status_message.starts_with("Debug:"));
    }

    #[tokio::test]
    async fn insert_row_leaves_empty_fields_to_their_defaults() {
        let mut state = debug_state().await;
        // Debug mode shows the INSERT: the empty id is left out.
        state.editing_row = Some(vec!["".into(), "b".into()]);
        handle_insert_row(&mut state).await;
        assert_eq!(state.query_input(), "INSERT INTO t (\"name\") VALUES ('b')");
        state.editing_row = Some(vec!["".into(), "NULL".into()]);
        handle_insert_row(&mut state).await;
        assert_eq!(state.query_input(), "INSERT INTO t (\"name\") VALUES (NULL)");
        state.editing_row = Some(vec!["".into(), "".into()]);
        handle_insert_row(&mut state).await;
        assert_eq!(state.query_input(), "INSERT INTO t DEFAULT VALUES");
        // Executed.
        state.debug_mode = false;
        state.editing_row = Some(vec!["".into(), "c".into()]);
        handle_insert_row(&mut state).await;
        let r = state
            .connection
            .as_ref()
            .unwrap()
            .execute_query("SELECT id, name FROM t ORDER BY id")
            .await
            .unwrap();
        assert_eq!(r.rows[1], ["2", "c"]);
    }

    #[test]
    fn connection_dialog_url_presets_and_ssl_survive_an_edit() {
        use crate::engine::models::{ConnectionConfig, Flavor, SslMode};
        let mut state = AppState::new(AppConfig::default(), false, true);
        let key = |state: &mut AppState, k: KeyCode| {
            handle_dialog_input(state, k, KeyModifiers::NONE);
        };
        state.open_new_connection_dialog();
        key(&mut state, KeyCode::Tab); // URL
        for c in "postgres://u:p%40ss@db.abc.supabase.co/app".chars() {
            key(&mut state, KeyCode::Char(c));
        }
        key(&mut state, KeyCode::Enter); // fills, doesn't save
        assert!(state.config.connections.is_empty());
        assert_eq!(state.new_connection.flavor, Some(Flavor::Supabase));
        assert_eq!(state.new_connection.password, "p@ss");
        while state.new_connection.active_field != ConnectionField::SslMode {
            key(&mut state, KeyCode::Tab);
        }
        key(&mut state, KeyCode::Right); // Prefer -> Require
        state.config.connections.push(ConnectionConfig::default()); // index 0
        state.new_connection.color = Some([9, 9, 9]);
        key(&mut state, KeyCode::Enter);
        assert_eq!(state.config.connections.len(), 2);

        state.open_edit_connection_dialog(1);
        key(&mut state, KeyCode::Enter);
        let saved = &state.config.connections[1];
        assert_eq!(saved.flavor, Some(Flavor::Supabase));
        assert_eq!(saved.ssl_mode, Some(SslMode::Require));
        assert_eq!(saved.color, Some([9, 9, 9]));
        assert_eq!(saved.host.as_deref(), Some("db.abc.supabase.co"));
    }

    #[test]
    fn cycle_fields_go_both_ways_and_ignore_typing() {
        use crate::engine::models::{Flavor, SslMode};
        let mut state = AppState::new(AppConfig::default(), false, true);
        let key = |state: &mut AppState, k: KeyCode| {
            handle_dialog_input(state, k, KeyModifiers::NONE);
        };
        state.open_new_connection_dialog();
        key(&mut state, KeyCode::Tab); // URL
        key(&mut state, KeyCode::Tab); // Modèle
        assert_eq!(state.new_connection.active_field, ConnectionField::Flavor);
        key(&mut state, KeyCode::Char('x'));
        assert_eq!(state.new_connection.cursor_position, 0);
        key(&mut state, KeyCode::Left); // Aucun -> last
        assert_eq!(state.new_connection.flavor, Flavor::ALL.last().copied());
        key(&mut state, KeyCode::Right); // back to Aucun
        assert_eq!(state.new_connection.flavor, None);
        key(&mut state, KeyCode::Right);
        assert_eq!(state.new_connection.flavor, Some(Flavor::ALL[0]));

        while state.new_connection.active_field != ConnectionField::SslMode {
            key(&mut state, KeyCode::Tab);
        }
        state.new_connection.ssl_mode = SslMode::Prefer;
        key(&mut state, KeyCode::Left);
        assert_eq!(state.new_connection.ssl_mode, SslMode::Disable);
        key(&mut state, KeyCode::Left); // wraps
        assert_eq!(state.new_connection.ssl_mode, SslMode::VerifyFull);
        key(&mut state, KeyCode::Right);
        assert_eq!(state.new_connection.ssl_mode, SslMode::Disable);
    }
}
