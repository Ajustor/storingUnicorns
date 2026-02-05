use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DatabaseType {
    Postgres,
    MySQL,
    SQLite,
    SQLServer,
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::Postgres => write!(f, "PostgreSQL"),
            DatabaseType::MySQL => write!(f, "MySQL"),
            DatabaseType::SQLite => write!(f, "SQLite"),
            DatabaseType::SQLServer => write!(f, "SQL Server"),
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
            DatabaseType::SQLServer => {
                // For tiberius, we don't use a connection string directly
                // This is just for display/logging purposes
                format!(
                    "sqlserver://{}@{}:{}/{}",
                    self.username.as_deref().unwrap_or("sa"),
                    self.host.as_deref().unwrap_or("localhost"),
                    self.port.unwrap_or(1433),
                    self.database
                )
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

/// Represents a table with its schema
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
}

#[allow(dead_code)]
impl TableInfo {
    #[allow(dead_code)]
    pub fn full_name(&self) -> String {
        if self.schema.is_empty() || self.schema == "public" || self.schema == "dbo" || self.schema == "main" {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }
}

/// Represents a schema with its tables
#[derive(Debug, Clone)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: Vec<String>,
    pub expanded: bool,
}
