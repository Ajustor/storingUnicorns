use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AzureAuthMethod {
    Credentials,     // SQL Server authentication (username/password)
    Interactive,     // Azure AD Interactive login
    ManagedIdentity, // Azure managed identity
}

impl Default for AzureAuthMethod {
    fn default() -> Self {
        Self::Credentials
    }
}

impl std::fmt::Display for AzureAuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AzureAuthMethod::Credentials => write!(f, "SQL Auth"),
            AzureAuthMethod::Interactive => write!(f, "Azure AD"),
            AzureAuthMethod::ManagedIdentity => write!(f, "Managed Identity"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DatabaseType {
    Postgres,
    MySQL,
    SQLite,
    SQLServer,
    Azure,
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::Postgres => write!(f, "PostgreSQL"),
            DatabaseType::MySQL => write!(f, "MySQL"),
            DatabaseType::SQLite => write!(f, "SQLite"),
            DatabaseType::SQLServer => write!(f, "SQL Server"),
            DatabaseType::Azure => write!(f, "Azure SQL"),
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
    #[serde(default)]
    pub azure_auth_method: Option<AzureAuthMethod>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// RGB colour used to tag the connection in the GUI.
    #[serde(default)]
    pub color: Option<[u8; 3]>,
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
            DatabaseType::Azure => {
                // Azure SQL Database - displayed connection info
                format!(
                    "azure://{}@{}:{}/{}",
                    self.username.as_deref().unwrap_or("<username>"),
                    self.host.as_deref().unwrap_or("*.database.windows.net"),
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
            azure_auth_method: None,
            tenant_id: None,
            color: None,
        }
    }
}

/// Represents a column in query results
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    /// Whether the column allows NULL values
    pub nullable: bool,
    /// Whether the column is a primary key
    pub is_primary_key: bool,
}

/// An index of a table.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    /// Indexed columns, in index order.
    pub columns: Vec<String>,
    pub unique: bool,
    /// Whether this index backs the primary key.
    pub primary: bool,
}

/// A foreign key of a table.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignKeyInfo {
    pub name: String,
    /// Referencing columns of this table, in key order.
    pub columns: Vec<String>,
    pub ref_table: String,
    /// Referenced columns of `ref_table`, matching `columns` one to one.
    pub ref_columns: Vec<String>,
}

/// Columns, indexes and foreign keys of a table.
#[derive(Debug, Clone, Default)]
pub struct TableDetails {
    pub columns: Vec<Column>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

/// Represents query results
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<String>>,
    pub rows_affected: u64,
    pub execution_time_ms: u128,
    /// True when fetching stopped at a row cap (more rows were available).
    pub truncated: bool,
    /// Primary-key columns of the table the rows come from, when the query
    /// reads a single table (`statements::single_table_source`); empty
    /// otherwise or when that table has no primary key.
    pub primary_key: Vec<String>,
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
        if self.schema.is_empty()
            || self.schema == "public"
            || self.schema == "dbo"
            || self.schema == "main"
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_connection_toml_still_parses_without_color() {
        let c: ConnectionConfig =
            toml::from_str("name = \"x\"\ndb_type = \"SQLite\"\ndatabase = \"a.db\"").unwrap();
        assert_eq!(c.color, None);
    }

    #[test]
    fn query_result_defaults_to_not_truncated() {
        assert!(!QueryResult::default().truncated);
    }
}
