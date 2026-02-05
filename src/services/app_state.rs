use crate::config::AppConfig;
use crate::db::DatabaseConnection;
use crate::models::{ConnectionConfig, DatabaseType, QueryResult};

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
            host: if self.host.is_empty() { None } else { Some(self.host.clone()) },
            port: self.port.parse().ok(),
            username: if self.username.is_empty() { None } else { Some(self.username.clone()) },
            password: if self.password.is_empty() { None } else { Some(self.password.clone()) },
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
            DatabaseType::SQLite => DatabaseType::Postgres,
        };
        // Update default port
        self.port = match self.db_type {
            DatabaseType::Postgres => String::from("5432"),
            DatabaseType::MySQL => String::from("3306"),
            DatabaseType::SQLite => String::new(),
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
    
    // Tables list state
    pub tables: Vec<String>,
    pub selected_table: usize,
    
    // Query editor state
    pub query_input: String,
    pub cursor_position: usize,
    
    // Results state
    pub query_result: Option<QueryResult>,
    #[allow(dead_code)]
    pub results_scroll: usize,
    pub selected_row: usize,
    
    // Dialog state
    pub dialog_mode: DialogMode,
    pub new_connection: NewConnectionState,
    
    // Status
    pub status_message: String,
    pub is_loading: bool,
    
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
            tables: Vec::new(),
            selected_table: 0,
            query_input: String::new(),
            cursor_position: 0,
            query_result: None,
            results_scroll: 0,
            selected_row: 0,
            dialog_mode: DialogMode::None,
            new_connection: NewConnectionState::default(),
            status_message: String::from("Press ? for help"),
            is_loading: false,
            should_quit: false,
        }
    }

    pub fn open_new_connection_dialog(&mut self) {
        self.new_connection = NewConnectionState::default();
        self.dialog_mode = DialogMode::NewConnection;
    }

    pub fn close_dialog(&mut self) {
        self.dialog_mode = DialogMode::None;
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
                if !self.tables.is_empty() {
                    self.selected_table = (self.selected_table + 1) % self.tables.len();
                }
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
                if !self.tables.is_empty() {
                    self.selected_table = self
                        .selected_table
                        .checked_sub(1)
                        .unwrap_or(self.tables.len() - 1);
                }
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
}
