use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DatabaseType {
    Postgres,
    MySQL,
    SQLite,
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::Postgres => write!(f, "PostgreSQL"),
            DatabaseType::MySQL => write!(f, "MySQL"),
            DatabaseType::SQLite => write!(f, "SQLite"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub name: String,
    pub db_type: DatabaseType,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database: String,
}

impl ConnectionConfig {
    pub fn to_connection_string(&self) -> String {
        match self.db_type {
            DatabaseType::Postgres => {
                format!(
                    "postgres://{}:{}@{}:{}/{}",
                    self.username.as_deref().unwrap_or("postgres"),
                    self.password.as_deref().unwrap_or(""),
                    self.host.as_deref().unwrap_or("localhost"),
                    self.port.unwrap_or(5432),
                    self.database
                )
            }
            DatabaseType::MySQL => {
                format!(
                    "mysql://{}:{}@{}:{}/{}",
                    self.username.as_deref().unwrap_or("root"),
                    self.password.as_deref().unwrap_or(""),
                    self.host.as_deref().unwrap_or("localhost"),
                    self.port.unwrap_or(3306),
                    self.database
                )
            }
            DatabaseType::SQLite => {
                format!("sqlite:{}", self.database)
            }
        }
    }
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            name: String::from("New Connection"),
            db_type: DatabaseType::Postgres,
            host: Some(String::from("localhost")),
            port: Some(5432),
            username: Some(String::from("postgres")),
            password: None,
            database: String::from("postgres"),
        }
    }
}

/// Represents a column in query results
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    #[allow(dead_code)]
    pub type_name: String,
}

/// Represents query results
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<String>>,
    #[allow(dead_code)]
    pub rows_affected: u64,
    pub execution_time_ms: u128,
}
