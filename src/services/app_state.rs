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
    #[allow(dead_code)]
    pub connections_scroll: usize,

    // Tables list state
    pub tables: Vec<String>,
    pub schemas: Vec<SchemaInfo>,
    pub selected_schema: usize,
    pub selected_table: usize,
    #[allow(dead_code)]
    pub tables_scroll: usize,

    // Query editor state
    pub query_input: String,
    pub cursor_position: usize,

    // Results state
    pub query_result: Option<QueryResult>,
    #[allow(dead_code)]
    pub results_scroll: usize,
    pub selected_row: usize,

    // Row editing state
    pub editing_row: Option<Vec<String>>,
    pub original_editing_row: Option<Vec<String>>,
    pub editing_table_name: Option<String>,
    pub editing_column: usize,
    pub editing_cursor: usize,

    // Dialog state
    pub dialog_mode: DialogMode,
    pub new_connection: NewConnectionState,
    pub editing_connection_index: Option<usize>,

    // Status
    pub status_message: String,
    pub is_loading: bool,
    pub is_connecting: bool,
    pub connection_error: Option<String>,

    // App control
    pub should_quit: bool,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
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
            query_input: String::new(),
            cursor_position: 0,
            query_result: None,
            results_scroll: 0,
            selected_row: 0,
            editing_row: None,
            original_editing_row: None,
            editing_table_name: None,
            editing_column: 0,
            editing_cursor: 0,
            dialog_mode: DialogMode::None,
            new_connection: NewConnectionState::default(),
            editing_connection_index: None,
            status_message: String::from("Press ? for help"),
            is_loading: false,
            is_connecting: false,
            connection_error: None,
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
    }

    /// Get currently selected table full name (schema.table or just table)
    pub fn get_selected_table_full_name(&self) -> Option<String> {
        if self.selected_table == 0 {
            return None; // Schema header is selected, not a table
        }
        if let Some(schema) = self.schemas.get(self.selected_schema) {
            if schema.expanded {
                if let Some(table) = schema.tables.get(self.selected_table - 1) {
                    return Some(
                        if schema.name == "public" || schema.name == "dbo" || schema.name == "main"
                        {
                            table.clone()
                        } else {
                            format!("{}.{}", schema.name, table)
                        },
                    );
                }
            }
        }
        None
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
                self.dialog_mode = DialogMode::EditRow;
            }
        }
    }

    /// Extract table name from current query (simple heuristic for SELECT ... FROM table)
    fn extract_table_from_query(&self) -> Option<String> {
        let query = self.query_input.to_uppercase();
        if let Some(from_pos) = query.find("FROM") {
            let after_from = &self.query_input[from_pos + 4..].trim_start();
            // Take the first word after FROM
            let table_name: String = after_from
                .chars()
                .take_while(|c| {
                    c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '[' || *c == ']'
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
