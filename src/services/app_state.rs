use crate::config::AppConfig;
use crate::db::DatabaseConnection;
use crate::models::{ConnectionConfig, QueryResult};

/// Active panel in the UI
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePanel {
    Connections,
    Tables,
    QueryEditor,
    Results,
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
            status_message: String::from("Ready. Press 'c' to connect, 'q' to quit."),
            is_loading: false,
            should_quit: false,
        }
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
