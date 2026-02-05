use super::query_tabs::QueryTabsState;
use crate::config::AppConfig;
use crate::db::DatabaseConnection;
use crate::models::{ConnectionConfig, DatabaseType, QueryResult, SchemaInfo};

/// Active panel in the UI
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePanel {
    Connections,
    Tables,
    QueryEditor,
    Results,
}

/// Dialog mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    None,
    NewConnection,
    EditConnection,
    EditRow,
    AddRow,
    #[allow(dead_code)]
    DeleteConfirm,
}

/// Fields in the new connection dialog
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionField {
    Name,
    DbType,
    Host,
    Port,
    Username,
    Password,
    Database,
}

impl ConnectionField {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::DbType,
            Self::DbType => Self::Host,
            Self::Host => Self::Port,
            Self::Port => Self::Username,
            Self::Username => Self::Password,
            Self::Password => Self::Database,
            Self::Database => Self::Name,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Name => Self::Database,
            Self::DbType => Self::Name,
            Self::Host => Self::DbType,
            Self::Port => Self::Host,
            Self::Username => Self::Port,
            Self::Password => Self::Username,
            Self::Database => Self::Password,
        }
    }
}

/// State for new connection dialog
#[derive(Debug, Clone)]
pub struct NewConnectionState {
    pub name: String,
    pub db_type: DatabaseType,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub database: String,
    pub active_field: ConnectionField,
    pub cursor_position: usize,
}

impl Default for NewConnectionState {
    fn default() -> Self {
        Self {
            name: String::from("New Connection"),
            db_type: DatabaseType::Postgres,
            host: String::from("localhost"),
            port: String::from("5432"),
            username: String::from("postgres"),
            password: String::new(),
            database: String::from("postgres"),
            active_field: ConnectionField::Name,
            cursor_position: 14, // length of "New Connection"
        }
    }
}

impl NewConnectionState {
    pub fn to_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            name: self.name.clone(),
            db_type: self.db_type.clone(),
            host: if self.host.is_empty() {
                None
            } else {
                Some(self.host.clone())
            },
            port: self.port.parse().ok(),
            username: if self.username.is_empty() {
                None
            } else {
                Some(self.username.clone())
            },
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            database: self.database.clone(),
        }
    }

    pub fn get_active_field_value(&self) -> &str {
        match self.active_field {
            ConnectionField::Name => &self.name,
            ConnectionField::DbType => "", // handled separately
            ConnectionField::Host => &self.host,
            ConnectionField::Port => &self.port,
            ConnectionField::Username => &self.username,
            ConnectionField::Password => &self.password,
            ConnectionField::Database => &self.database,
        }
    }

    pub fn get_active_field_mut(&mut self) -> Option<&mut String> {
        match self.active_field {
            ConnectionField::Name => Some(&mut self.name),
            ConnectionField::DbType => None, // handled separately
            ConnectionField::Host => Some(&mut self.host),
            ConnectionField::Port => Some(&mut self.port),
            ConnectionField::Username => Some(&mut self.username),
            ConnectionField::Password => Some(&mut self.password),
            ConnectionField::Database => Some(&mut self.database),
        }
    }

    pub fn cycle_db_type(&mut self) {
        self.db_type = match self.db_type {
            DatabaseType::Postgres => DatabaseType::MySQL,
            DatabaseType::MySQL => DatabaseType::SQLite,
            DatabaseType::SQLite => DatabaseType::SQLServer,
            DatabaseType::SQLServer => DatabaseType::Postgres,
        };
        // Update default port
        self.port = match self.db_type {
            DatabaseType::Postgres => String::from("5432"),
            DatabaseType::MySQL => String::from("3306"),
            DatabaseType::SQLite => String::new(),
            DatabaseType::SQLServer => String::from("1433"),
        };
    }
}

/// Main application state
pub struct AppState {
    pub config: AppConfig,
    pub active_panel: ActivePanel,
    pub connection: Option<DatabaseConnection>,
    pub current_connection_config: Option<ConnectionConfig>,

    // Connection list state
    pub selected_connection: usize,
    pub connections_scroll: usize,

    // Tables list state
    pub tables: Vec<String>,
    pub schemas: Vec<SchemaInfo>,
    pub selected_schema: usize,
    pub selected_table: usize,
    pub tables_scroll: usize,

    // Query tabs state
    pub query_tabs: QueryTabsState,

    // Results state
    pub query_result: Option<QueryResult>,
    pub results_scroll: usize,
    pub results_scroll_x: usize, // Horizontal scroll offset
    pub selected_row: usize,

    // Row editing state
    pub editing_row: Option<Vec<String>>,
    pub original_editing_row: Option<Vec<String>>,
    pub editing_table_name: Option<String>,
    pub editing_column: usize,
    pub editing_cursor: usize,
    pub system_columns: Vec<usize>, // Indices of auto-generated columns (id, timestamps, etc.)

    // Dialog state
    pub dialog_mode: DialogMode,
    pub new_connection: NewConnectionState,
    pub editing_connection_index: Option<usize>,

    // Status
    pub status_message: String,
    pub is_loading: bool,
    pub is_connecting: bool,
    pub connection_error: Option<String>,

    // Debug mode
    pub debug_mode: bool,

    // App control
    pub should_quit: bool,
}

impl AppState {
    pub fn new(config: AppConfig, debug_mode: bool) -> Self {
        Self {
            config,
            active_panel: ActivePanel::Connections,
            connection: None,
            current_connection_config: None,
            selected_connection: 0,
            connections_scroll: 0,
            tables: Vec::new(),
            schemas: Vec::new(),
            selected_schema: 0,
            selected_table: 0,
            tables_scroll: 0,
            query_tabs: QueryTabsState::load().unwrap_or_default(),
            query_result: None,
            results_scroll: 0,
            results_scroll_x: 0,
            selected_row: 0,
            editing_row: None,
            original_editing_row: None,
            editing_table_name: None,
            editing_column: 0,
            editing_cursor: 0,
            system_columns: Vec::new(),
            dialog_mode: DialogMode::None,
            new_connection: NewConnectionState::default(),
            editing_connection_index: None,
            status_message: String::from("Press ? for help"),
            is_loading: false,
            is_connecting: false,
            connection_error: None,
            debug_mode,
            should_quit: false,
        }
    }

    pub fn open_new_connection_dialog(&mut self) {
        self.new_connection = NewConnectionState::default();
        self.editing_connection_index = None;
        self.dialog_mode = DialogMode::NewConnection;
    }

    pub fn open_edit_connection_dialog(&mut self, index: usize) {
        if let Some(conn) = self.config.connections.get(index) {
            self.new_connection = NewConnectionState {
                name: conn.name.clone(),
                db_type: conn.db_type.clone(),
                host: conn.host.clone().unwrap_or_default(),
                port: conn.port.map(|p| p.to_string()).unwrap_or_default(),
                username: conn.username.clone().unwrap_or_default(),
                password: conn.password.clone().unwrap_or_default(),
                database: conn.database.clone(),
                active_field: ConnectionField::Name,
                cursor_position: conn.name.len(),
            };
            self.editing_connection_index = Some(index);
            self.dialog_mode = DialogMode::EditConnection;
        }
    }

    pub fn close_dialog(&mut self) {
        self.dialog_mode = DialogMode::None;
        self.editing_connection_index = None;
    }

    pub fn is_dialog_open(&self) -> bool {
        self.dialog_mode != DialogMode::None
    }

    // Query tab helper methods
    pub fn query_input(&self) -> &str {
        &self.query_tabs.current_tab().query
    }

    pub fn query_input_mut(&mut self) -> &mut String {
        &mut self.query_tabs.current_tab_mut().query
    }

    pub fn cursor_position(&self) -> usize {
        self.query_tabs.current_tab().cursor_position
    }

    pub fn set_cursor_position(&mut self, pos: usize) {
        self.query_tabs.current_tab_mut().cursor_position = pos;
    }

    pub fn set_query(&mut self, query: String) {
        let tab = self.query_tabs.current_tab_mut();
        tab.query = query;
        tab.cursor_position = tab.query.len();
        tab.is_modified = true;
    }

    pub fn save_query_tabs(&self) {
        let _ = self.query_tabs.save();
    }

    pub fn next_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Connections => ActivePanel::Tables,
            ActivePanel::Tables => ActivePanel::QueryEditor,
            ActivePanel::QueryEditor => ActivePanel::Results,
            ActivePanel::Results => ActivePanel::Connections,
        };
    }

    pub fn prev_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Connections => ActivePanel::Results,
            ActivePanel::Tables => ActivePanel::Connections,
            ActivePanel::QueryEditor => ActivePanel::Tables,
            ActivePanel::Results => ActivePanel::QueryEditor,
        };
    }

    pub fn select_next(&mut self) {
        match self.active_panel {
            ActivePanel::Connections => {
                if !self.config.connections.is_empty() {
                    self.selected_connection =
                        (self.selected_connection + 1) % self.config.connections.len();
                }
            }
            ActivePanel::Tables => {
                self.navigate_tables(true);
            }
            ActivePanel::Results => {
                if let Some(ref result) = self.query_result {
                    if !result.rows.is_empty() {
                        self.selected_row = (self.selected_row + 1) % result.rows.len();
                    }
                }
            }
            _ => {}
        }
    }

    pub fn select_prev(&mut self) {
        match self.active_panel {
            ActivePanel::Connections => {
                if !self.config.connections.is_empty() {
                    self.selected_connection = self
                        .selected_connection
                        .checked_sub(1)
                        .unwrap_or(self.config.connections.len() - 1);
                }
            }
            ActivePanel::Tables => {
                self.navigate_tables(false);
            }
            ActivePanel::Results => {
                if let Some(ref result) = self.query_result {
                    if !result.rows.is_empty() {
                        self.selected_row = self
                            .selected_row
                            .checked_sub(1)
                            .unwrap_or(result.rows.len() - 1);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
    }

    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Toggle schema expansion
    pub fn toggle_schema(&mut self) {
        if let Some(schema) = self.schemas.get_mut(self.selected_schema) {
            schema.expanded = !schema.expanded;
        }
    }

    /// Calculate the visual index of the currently selected item in the tables panel
    fn get_selected_visual_index(&self) -> usize {
        let mut visual_idx = 0;
        for (schema_idx, schema) in self.schemas.iter().enumerate() {
            if schema_idx == self.selected_schema {
                // Found the schema, add the table offset
                return visual_idx + self.selected_table;
            }
            // Count this schema header
            visual_idx += 1;
            // Count expanded tables
            if schema.expanded {
                visual_idx += schema.tables.len();
            }
        }
        visual_idx
    }

    /// Navigate through tables panel (schemas and tables)
    fn navigate_tables(&mut self, forward: bool) {
        if self.schemas.is_empty() {
            return;
        }

        if forward {
            // Move forward
            if let Some(schema) = self.schemas.get(self.selected_schema) {
                if schema.expanded && self.selected_table < schema.tables.len() {
                    // Move to next table within schema
                    self.selected_table += 1;
                    if self.selected_table > schema.tables.len() {
                        // Move to next schema
                        self.selected_table = 0;
                        self.selected_schema = (self.selected_schema + 1) % self.schemas.len();
                    }
                } else {
                    // Move to next schema
                    self.selected_table = 0;
                    self.selected_schema = (self.selected_schema + 1) % self.schemas.len();
                }
            }
        } else {
            // Move backward
            if self.selected_table > 0 {
                self.selected_table -= 1;
            } else if self.selected_schema > 0 {
                self.selected_schema -= 1;
                if let Some(schema) = self.schemas.get(self.selected_schema) {
                    self.selected_table = if schema.expanded {
                        schema.tables.len()
                    } else {
                        0
                    };
                }
            } else {
                // Wrap to last schema
                self.selected_schema = self.schemas.len() - 1;
                if let Some(schema) = self.schemas.get(self.selected_schema) {
                    self.selected_table = if schema.expanded {
                        schema.tables.len()
                    } else {
                        0
                    };
                }
            }
        }

        // Update scroll to keep selection visible
        let visual_idx = self.get_selected_visual_index();
        // Scroll down if selection is below visible area (assume ~10 visible items)
        if visual_idx >= self.tables_scroll + 10 {
            self.tables_scroll = visual_idx.saturating_sub(9);
        }
        // Scroll up if selection is above visible area
        if visual_idx < self.tables_scroll {
            self.tables_scroll = visual_idx;
        }
    }

    /// Get currently selected table full name (schema.table)
    /// Always includes schema for proper query generation
    pub fn get_selected_table_full_name(&self) -> Option<String> {
        if self.selected_table == 0 {
            return None; // Schema header is selected, not a table
        }
        if let Some(schema) = self.schemas.get(self.selected_schema) {
            if schema.expanded {
                if let Some(table) = schema.tables.get(self.selected_table - 1) {
                    // Get quote characters based on database type
                    let (quote_start, quote_end) = self.get_quote_chars();
                    // Always include schema for clarity and correctness
                    return Some(format!(
                        "{1}{0}{2}.{1}{3}{2}",
                        schema.name, quote_start, quote_end, table
                    ));
                }
            }
        }
        None
    }

    /// Get quote characters for the current database type
    pub fn get_quote_chars(&self) -> (char, char) {
        match &self.current_connection_config {
            Some(config) => match config.db_type {
                crate::models::DatabaseType::Postgres => ('"', '"'),
                crate::models::DatabaseType::MySQL => ('`', '`'),
                crate::models::DatabaseType::SQLite => ('"', '"'),
                crate::models::DatabaseType::SQLServer => ('[', ']'),
            },
            None => ('"', '"'), // Default to double quotes
        }
    }

    /// Get total visible items count in tables panel
    #[allow(dead_code)]
    pub fn get_tables_visible_count(&self) -> usize {
        self.schemas
            .iter()
            .map(|s| 1 + if s.expanded { s.tables.len() } else { 0 })
            .sum()
    }

    /// Open row edit dialog
    pub fn open_edit_row_dialog(&mut self) {
        if let Some(ref result) = self.query_result {
            if let Some(row) = result.rows.get(self.selected_row) {
                self.editing_row = Some(row.clone());
                self.original_editing_row = Some(row.clone());
                // Extract table name from query (simple heuristic)
                self.editing_table_name = self.extract_table_from_query();
                self.editing_column = 0;
                self.editing_cursor = row.first().map(|s| s.len()).unwrap_or(0);
                self.system_columns = Vec::new(); // No system columns for edit mode
                self.dialog_mode = DialogMode::EditRow;
            }
        }
    }

    /// Open add row dialog
    pub fn open_add_row_dialog(&mut self) {
        if let Some(ref result) = self.query_result {
            // Create empty row with same number of columns
            let empty_row: Vec<String> = result.columns.iter().map(|_| String::new()).collect();

            // Detect system columns (auto-generated: id, created_at, updated_at, etc.)
            self.system_columns = result
                .columns
                .iter()
                .enumerate()
                .filter_map(|(idx, col)| {
                    let name_lower = col.name.to_lowercase();
                    let type_lower = col.type_name.to_lowercase();

                    // Detect common auto-generated column patterns
                    let is_auto_id = name_lower == "id"
                        || name_lower.ends_with("_id")
                            && name_lower.starts_with(
                                &result
                                    .columns
                                    .first()
                                    .map(|c| c.name.to_lowercase())
                                    .unwrap_or_default(),
                            )
                        || type_lower.contains("serial")
                        || type_lower.contains("identity")
                        || type_lower.contains("auto_increment");

                    let is_timestamp = name_lower.contains("created_at")
                        || name_lower.contains("updated_at")
                        || name_lower.contains("createdat")
                        || name_lower.contains("updatedat")
                        || name_lower.contains("created_on")
                        || name_lower.contains("updated_on")
                        || name_lower.contains("inserted_at")
                        || name_lower.contains("modified_at");

                    if is_auto_id || is_timestamp {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect();

            self.editing_row = Some(empty_row);
            self.original_editing_row = None; // No original for new rows
            self.editing_table_name = self.extract_table_from_query();

            // Find first non-system column to start editing
            let first_editable = (0..result.columns.len())
                .find(|idx| !self.system_columns.contains(idx))
                .unwrap_or(0);

            self.editing_column = first_editable;
            self.editing_cursor = 0;
            self.dialog_mode = DialogMode::AddRow;
        }
    }

    /// Check if a column is a system column (auto-generated)
    pub fn is_system_column(&self, idx: usize) -> bool {
        self.system_columns.contains(&idx)
    }

    /// Extract table name from current query (simple heuristic for SELECT ... FROM table)
    /// Supports schema.table format and quoted identifiers
    fn extract_table_from_query(&self) -> Option<String> {
        let query_input = self.query_input();
        let query = query_input.to_uppercase();
        if let Some(from_pos) = query.find("FROM") {
            let after_from = &query_input[from_pos + 4..].trim_start();
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

    /// Update scroll positions to follow cursor
    #[allow(dead_code)]
    pub fn update_connections_scroll(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        if self.selected_connection < self.connections_scroll {
            self.connections_scroll = self.selected_connection;
        } else if self.selected_connection >= self.connections_scroll + visible_height {
            self.connections_scroll = self.selected_connection - visible_height + 1;
        }
    }

    #[allow(dead_code)]
    pub fn update_results_scroll(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        if self.selected_row < self.results_scroll {
            self.results_scroll = self.selected_row;
        } else if self.selected_row >= self.results_scroll + visible_height {
            self.results_scroll = self.selected_row - visible_height + 1;
        }
    }
}
