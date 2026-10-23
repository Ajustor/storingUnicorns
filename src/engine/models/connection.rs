use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AzureAuthMethod {
    Credentials, // SQL Server authentication (username/password)
    /// Token of the Azure CLI (`az login`); serialized name kept from when
    /// it was the only Entra ID method.
    Interactive,
    ManagedIdentity, // Azure managed identity
    /// The user's own Entra ID account through the browser (authorization
    /// code + PKCE) with the organization's app (`azure_client_id`).
    InteractiveBrowser,
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
            AzureAuthMethod::Interactive => write!(f, "Azure CLI"),
            AzureAuthMethod::ManagedIdentity => write!(f, "Managed Identity"),
            AzureAuthMethod::InteractiveBrowser => write!(f, "Azure AD interactif (navigateur)"),
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
    MongoDB,
    Redis,
}

/// What kind of engine a `DatabaseType` is: SQL code paths only run for `Sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Sql,
    /// MongoDB (and its compatible services).
    Document,
    /// Redis (and Valkey).
    KeyValue,
}

impl DatabaseType {
    pub fn family(&self) -> Family {
        match self {
            DatabaseType::MongoDB => Family::Document,
            DatabaseType::Redis => Family::KeyValue,
            _ => Family::Sql,
        }
    }

    /// Whether the engine speaks SQL (every engine but MongoDB and Redis).
    pub fn is_sql(&self) -> bool {
        self.family() == Family::Sql
    }

    /// Whether scripts run as T-SQL batches split on `GO`
    /// (`ops::query::run_script`): SQL Server and Azure.
    pub fn runs_tsql_batches(&self) -> bool {
        matches!(self, DatabaseType::SQLServer | DatabaseType::Azure)
    }
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::Postgres => write!(f, "PostgreSQL"),
            DatabaseType::MySQL => write!(f, "MySQL"),
            DatabaseType::SQLite => write!(f, "SQLite"),
            DatabaseType::SQLServer => write!(f, "SQL Server"),
            DatabaseType::Azure => write!(f, "Azure SQL"),
            DatabaseType::MongoDB => write!(f, "MongoDB"),
            DatabaseType::Redis => write!(f, "Redis"),
        }
    }
}

/// TLS policy for PostgreSQL / MySQL (ignored by the other drivers).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SslMode {
    Disable,
    /// TLS when the server offers it (psql's default).
    #[default]
    Prefer,
    /// Encrypted, certificate not verified.
    Require,
    /// Encrypted, certificate signed by a trusted CA.
    VerifyCa,
    /// `VerifyCa` + the certificate matches the host name.
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 5] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyCa,
        SslMode::VerifyFull,
    ];
}

impl std::fmt::Display for SslMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SslMode::Disable => "Désactivé",
            SslMode::Prefer => "Préféré",
            SslMode::Require => "Obligatoire",
            SslMode::VerifyCa => "Vérifier l'autorité (CA)",
            SslMode::VerifyFull => "Vérification complète",
        })
    }
}

/// How a PostgreSQL / MySQL connection authenticates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DbAuthMethod {
    /// The connection's password (or the driver's environment fallbacks).
    #[default]
    Password,
    /// Cloud SQL IAM database authentication: the user's Google account,
    /// with an OAuth2 token from `gcloud` sent as the password.
    GcpIam,
}

impl std::fmt::Display for DbAuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DbAuthMethod::Password => "Mot de passe",
            DbAuthMethod::GcpIam => "IAM Google Cloud",
        })
    }
}

/// A PostgreSQL- or MySQL-compatible product. Display and dialog defaults
/// only: the driver is still `db_type`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Flavor {
    MariaDb,
    PlanetScale,
    CockroachDb,
    TimescaleDb,
    Supabase,
    Neon,
    Redshift,
}

impl Flavor {
    pub const ALL: [Flavor; 7] = [
        Flavor::MariaDb,
        Flavor::PlanetScale,
        Flavor::CockroachDb,
        Flavor::TimescaleDb,
        Flavor::Supabase,
        Flavor::Neon,
        Flavor::Redshift,
    ];

    /// The driver this product speaks.
    pub fn driver(self) -> DatabaseType {
        match self {
            Flavor::MariaDb | Flavor::PlanetScale => DatabaseType::MySQL,
            _ => DatabaseType::Postgres,
        }
    }
}

impl std::fmt::Display for Flavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Flavor::MariaDb => "MariaDB",
            Flavor::PlanetScale => "PlanetScale",
            Flavor::CockroachDb => "CockroachDB",
            Flavor::TimescaleDb => "TimescaleDB",
            Flavor::Supabase => "Supabase",
            Flavor::Neon => "Neon",
            Flavor::Redshift => "Redshift",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Application (client) ID registered by the organization in Entra ID
    /// (`AzureAuthMethod::InteractiveBrowser`).
    #[serde(default)]
    pub azure_client_id: Option<String>,
    /// RGB colour used to tag the connection in the GUI.
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    /// PostgreSQL / MySQL only; `None` = `SslMode::Prefer`.
    #[serde(default)]
    pub ssl_mode: Option<SslMode>,
    /// Extra trusted CA certificate (PEM).
    #[serde(default)]
    pub ssl_ca: Option<std::path::PathBuf>,
    /// Compatible product shown instead of the bare engine.
    #[serde(default)]
    pub flavor: Option<Flavor>,
    /// PostgreSQL / MySQL only; `None` = `DbAuthMethod::Password`.
    #[serde(default)]
    pub db_auth: Option<DbAuthMethod>,
    /// Folder holding the connection in the explorer, a normalized
    /// `/`-separated path such as `"Clients/ACME/Prod"`; `None` = root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Schemas hidden from the explorer, the table search and the
    /// completion, by their exact listed name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_schemas: Vec<String>,
    /// MongoDB: the whole connection string, without its password (kept in
    /// `password`); `None` = built from host / port / username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// MongoDB authentication database (`authSource`); `None` = the URI's,
    /// else `admin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_source: Option<String>,
}

impl ConnectionConfig {
    /// Whether `schema` is hidden on this connection (exact name match).
    pub fn is_schema_hidden(&self, schema: &str) -> bool {
        self.hidden_schemas.iter().any(|s| s == schema)
    }

    /// The SSL mode connections use: Google Cloud IAM sends its token as the
    /// password, so it never goes below `Require`.
    pub fn effective_ssl_mode(&self) -> SslMode {
        let mode = self.ssl_mode.unwrap_or_default();
        match (self.effective_db_auth(), mode) {
            (DbAuthMethod::GcpIam, SslMode::Disable | SslMode::Prefer) => SslMode::Require,
            _ => mode,
        }
    }

    /// MongoDB / Redis TLS: `None` = plain connection, `Some(verify)` = TLS,
    /// verifying the server certificate when `verify`. From `ssl_mode`
    /// (`Disable`/`Prefer` = none, `Require` = not verified, `VerifyCa` /
    /// `VerifyFull` = verified), except that a MongoDB URI wins: `mongodb+srv`
    /// or `tls=true` turn TLS on, `tls=false` turns it off,
    /// `tlsInsecure` / `tlsAllowInvalidCertificates=true` skip verification.
    /// Always `None` for the SQL engines.
    pub fn nosql_tls(&self) -> Option<bool> {
        if self.db_type.is_sql() {
            return None;
        }
        let from_mode = match self.ssl_mode.unwrap_or_default() {
            SslMode::Disable | SslMode::Prefer => None,
            SslMode::Require => Some(false),
            SslMode::VerifyCa | SslMode::VerifyFull => Some(true),
        };
        let uri = match (&self.db_type, self.uri.as_deref()) {
            (DatabaseType::MongoDB, Some(uri)) if !uri.trim().is_empty() => uri.trim(),
            _ => return from_mode,
        };
        let srv = uri
            .get(..14)
            .is_some_and(|s| s.eq_ignore_ascii_case("mongodb+srv://"));
        let mut tls = None;
        let mut insecure = false;
        let query = uri.split_once('?').map_or("", |(_, q)| q);
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let on = value.eq_ignore_ascii_case("true");
            match key.to_ascii_lowercase().as_str() {
                "tls" | "ssl" => tls = Some(on),
                "tlsinsecure" | "tlsallowinvalidcertificates" => insecure |= on,
                _ => {}
            }
        }
        if !tls.unwrap_or(srv || from_mode.is_some()) {
            return None;
        }
        Some(!insecure && from_mode != Some(false))
    }

    /// The authentication method, `Password` for the engines without a
    /// choice (only PostgreSQL and MySQL support Google Cloud IAM).
    pub fn effective_db_auth(&self) -> DbAuthMethod {
        match self.db_type {
            DatabaseType::Postgres | DatabaseType::MySQL => self.db_auth.unwrap_or_default(),
            _ => DbAuthMethod::Password,
        }
    }

    /// The flavor when it matches the driver, else the engine name.
    pub fn display_type(&self) -> String {
        match self.flavor {
            Some(f) if f.driver() == self.db_type => f.to_string(),
            _ => self.db_type.to_string(),
        }
    }

    /// For display and logs only; never contains the password. (SQLite
    /// still connects with it: its string is just the file path.)
    pub fn to_connection_string(&self) -> String {
        match self.db_type {
            DatabaseType::Postgres => {
                format!(
                    "postgres://{}@{}:{}/{}",
                    self.username.as_deref().unwrap_or("postgres"),
                    self.host.as_deref().unwrap_or("localhost"),
                    self.port.unwrap_or(5432),
                    self.database
                )
            }
            DatabaseType::MySQL => {
                format!(
                    "mysql://{}@{}:{}/{}",
                    self.username.as_deref().unwrap_or("root"),
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
            DatabaseType::MongoDB => match self.uri.as_deref().map(str::trim) {
                // Saved without its password; stripped again in case it was
                // typed into the file by hand.
                Some(uri) if !uri.is_empty() => crate::engine::presets::split_uri_password(uri).0,
                _ => format!(
                    "mongodb://{}{}:{}/{}",
                    self.user_at(),
                    self.host.as_deref().unwrap_or("localhost"),
                    self.port.unwrap_or(27017),
                    self.database
                ),
            },
            DatabaseType::Redis => format!(
                "{}://{}{}:{}/{}",
                if self.nosql_tls().is_some() {
                    "rediss"
                } else {
                    "redis"
                },
                self.user_at(),
                self.host.as_deref().unwrap_or("localhost"),
                self.port.unwrap_or(6379),
                if self.database.is_empty() {
                    "0"
                } else {
                    &self.database
                }
            ),
        }
    }

    /// `user@` for display URLs, empty without a username.
    fn user_at(&self) -> String {
        match self.username.as_deref() {
            Some(u) if !u.is_empty() => format!("{u}@"),
            _ => String::new(),
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
            azure_client_id: None,
            color: None,
            ssl_mode: None,
            ssl_ca: None,
            flavor: None,
            db_auth: None,
            folder: None,
            hidden_schemas: Vec::new(),
            uri: None,
            auth_source: None,
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
    /// Referenced table, always qualified like `get_tables` (`schema.table`).
    pub ref_table: String,
    /// Referenced columns of `ref_table`, matching `columns` one to one.
    pub ref_columns: Vec<String>,
}

/// A foreign key of another table that points at this one.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingForeignKey {
    /// Constraint name (empty for SQLite, which does not expose it).
    pub name: String,
    /// Referencing table, qualified like `get_tables` (`schema.table`).
    pub table: String,
    /// Columns of the referencing table, in key order.
    pub columns: Vec<String>,
    /// Columns of this table, matching `columns` one to one.
    pub ref_columns: Vec<String>,
}

/// Columns, indexes, foreign keys and incoming foreign keys of a table.
#[derive(Debug, Clone, Default)]
pub struct TableDetails {
    pub columns: Vec<Column>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Why `indexes` is empty when the server couldn't list them
    /// (CockroachDB, Redshift…).
    pub indexes_error: Option<String>,
    /// Same for `foreign_keys`.
    pub foreign_keys_error: Option<String>,
    /// Foreign keys of other tables pointing at this one.
    pub incoming: Vec<IncomingForeignKey>,
    /// Same as `indexes_error` for `incoming`.
    pub incoming_error: Option<String>,
}

/// The value of a NULL cell. Cells are text, so NULL needs a value of its
/// own that can't be typed: a private-use character before "NULL". Drivers
/// produce it, grids show it as an italic "NULL", "Mettre à NULL" sets it
/// and the SQL builders turn it into `NULL`; the text "NULL" is a string
/// like any other.
pub const NULL_CELL: &str = "\u{E000}NULL";

/// Whether `cell` is NULL (`NULL_CELL`).
pub fn is_null(cell: &str) -> bool {
    cell == NULL_CELL
}

/// Text to show for `cell`: "NULL" for NULL, the value otherwise.
pub fn display_cell(cell: &str) -> &str {
    if is_null(cell) {
        "NULL"
    } else {
        cell
    }
}

/// Represents query results
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<String>>,
    pub rows_affected: u64,
    /// The statement returned no rows and the server reported no
    /// affected-row count (SQL Server under `SET NOCOUNT ON`, or a batch
    /// mixing statements): `rows_affected` means nothing, show "executed".
    pub count_unknown: bool,
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
        assert_eq!(c.ssl_mode, None);
        assert_eq!(c.ssl_ca, None);
        assert_eq!(c.flavor, None);
    }

    #[test]
    fn old_config_without_new_fields_loads() {
        let json = r#"{"name":"a","db_type":"Postgres","host":"h","port":5432,
            "username":"u","password":"p","database":"d"}"#;
        let c: ConnectionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.ssl_mode, None);
        assert_eq!(c.ssl_ca, None);
        assert_eq!(c.flavor, None);
        assert_eq!(c.effective_ssl_mode(), SslMode::Prefer);
    }

    #[test]
    fn new_fields_round_trip() {
        let c = ConnectionConfig {
            ssl_mode: Some(SslMode::VerifyFull),
            ssl_ca: Some("C:/ca.pem".into()),
            flavor: Some(Flavor::Supabase),
            ..Default::default()
        };
        let back: ConnectionConfig =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!(back.ssl_ca, Some(std::path::PathBuf::from("C:/ca.pem")));
        assert_eq!(back.flavor, Some(Flavor::Supabase));
        // The connections file is TOML.
        let back: ConnectionConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!(back.ssl_ca, Some(std::path::PathBuf::from("C:/ca.pem")));
        assert_eq!(back.flavor, Some(Flavor::Supabase));
    }

    #[test]
    fn connection_string_never_contains_the_password() {
        let c = ConnectionConfig {
            username: Some("u".into()),
            password: Some("s3cr3t".into()),
            ..Default::default()
        };
        assert!(!c.to_connection_string().contains("s3cr3t"));
        assert_eq!(
            c.to_connection_string(),
            "postgres://u@localhost:5432/postgres"
        );
        let c = ConnectionConfig {
            db_type: DatabaseType::MySQL,
            port: Some(3306),
            ..c
        };
        assert_eq!(
            c.to_connection_string(),
            "mysql://u@localhost:3306/postgres"
        );
    }

    #[test]
    fn display_type_shows_a_matching_flavor_only() {
        let mut c = ConnectionConfig {
            flavor: Some(Flavor::Supabase),
            ..Default::default()
        };
        assert_eq!(c.display_type(), "Supabase");
        c.db_type = DatabaseType::MySQL; // Supabase is PostgreSQL: ignored
        assert_eq!(c.display_type(), "MySQL");
        c.flavor = Some(Flavor::MariaDb);
        assert_eq!(c.display_type(), "MariaDB");
    }

    #[test]
    fn null_cell_is_not_the_text_null() {
        assert!(is_null(NULL_CELL));
        assert!(!is_null("NULL"));
        assert!(!is_null(""));
        assert_eq!(display_cell(NULL_CELL), "NULL");
        assert_eq!(display_cell("NULL"), "NULL");
        assert_eq!(display_cell("x"), "x");
    }

    #[test]
    fn azure_method_labels() {
        assert_eq!(AzureAuthMethod::Credentials.to_string(), "SQL Auth");
        assert_eq!(AzureAuthMethod::Interactive.to_string(), "Azure CLI");
        assert_eq!(
            AzureAuthMethod::InteractiveBrowser.to_string(),
            "Azure AD interactif (navigateur)"
        );
        assert_eq!(
            AzureAuthMethod::ManagedIdentity.to_string(),
            "Managed Identity"
        );
    }

    #[test]
    fn interactive_browser_serializes_by_name() {
        assert_eq!(
            serde_json::to_string(&AzureAuthMethod::InteractiveBrowser).unwrap(),
            "\"InteractiveBrowser\""
        );
        // The CLI method keeps its serialized name.
        assert_eq!(
            serde_json::to_string(&AzureAuthMethod::Interactive).unwrap(),
            "\"Interactive\""
        );
    }

    #[test]
    fn config_without_client_id_loads() {
        let c: ConnectionConfig = toml::from_str(
            "name = \"x\"\ndb_type = \"Azure\"\ndatabase = \"d\"\n\
             azure_auth_method = \"Interactive\"\ntenant_id = \"t\"",
        )
        .unwrap();
        assert_eq!(c.azure_client_id, None);
        assert_eq!(c.azure_auth_method, Some(AzureAuthMethod::Interactive));
    }

    #[test]
    fn interactive_browser_config_round_trips() {
        let c = ConnectionConfig {
            db_type: DatabaseType::Azure,
            host: Some("srv.database.windows.net".into()),
            username: Some("jane@contoso.com".into()),
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: Some("contoso.com".into()),
            azure_client_id: Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into()),
            ..Default::default()
        };
        let text = toml::to_string(&c).unwrap();
        assert!(
            text.contains("azure_auth_method = \"InteractiveBrowser\""),
            "{text}"
        );
        let back: ConnectionConfig = toml::from_str(&text).unwrap();
        assert_eq!(
            back.azure_auth_method,
            Some(AzureAuthMethod::InteractiveBrowser)
        );
        assert_eq!(back.tenant_id.as_deref(), Some("contoso.com"));
        assert_eq!(
            back.azure_client_id.as_deref(),
            Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f")
        );
        assert_eq!(back.username.as_deref(), Some("jane@contoso.com"));
    }

    #[test]
    fn config_without_db_auth_uses_the_password() {
        let c: ConnectionConfig = toml::from_str(
            "name = \"x\"
db_type = \"Postgres\"
database = \"d\"
username = \"u\"",
        )
        .unwrap();
        assert_eq!(c.db_auth, None);
        assert_eq!(c.effective_db_auth(), DbAuthMethod::Password);
        assert_eq!(c.effective_ssl_mode(), SslMode::Prefer);
    }

    #[test]
    fn gcp_iam_config_round_trips() {
        let c = ConnectionConfig {
            db_type: DatabaseType::MySQL,
            username: Some("jane@example.com".into()),
            db_auth: Some(DbAuthMethod::GcpIam),
            ..Default::default()
        };
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("db_auth = \"GcpIam\""), "{text}");
        let back: ConnectionConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.db_auth, Some(DbAuthMethod::GcpIam));
        assert_eq!(back.effective_db_auth(), DbAuthMethod::GcpIam);
    }

    #[test]
    fn gcp_iam_is_for_postgres_and_mysql_only() {
        for (db_type, expected) in [
            (DatabaseType::Postgres, DbAuthMethod::GcpIam),
            (DatabaseType::MySQL, DbAuthMethod::GcpIam),
            (DatabaseType::SQLite, DbAuthMethod::Password),
            (DatabaseType::SQLServer, DbAuthMethod::Password),
            (DatabaseType::Azure, DbAuthMethod::Password),
        ] {
            let c = ConnectionConfig {
                db_type: db_type.clone(),
                db_auth: Some(DbAuthMethod::GcpIam),
                ..Default::default()
            };
            assert_eq!(c.effective_db_auth(), expected, "{db_type}");
        }
    }

    #[test]
    fn gcp_iam_needs_at_least_require() {
        let with = |ssl_mode| ConnectionConfig {
            db_auth: Some(DbAuthMethod::GcpIam),
            ssl_mode,
            ..Default::default()
        };
        assert_eq!(with(None).effective_ssl_mode(), SslMode::Require);
        assert_eq!(
            with(Some(SslMode::Disable)).effective_ssl_mode(),
            SslMode::Require
        );
        assert_eq!(
            with(Some(SslMode::Prefer)).effective_ssl_mode(),
            SslMode::Require
        );
        assert_eq!(
            with(Some(SslMode::Require)).effective_ssl_mode(),
            SslMode::Require
        );
        assert_eq!(
            with(Some(SslMode::VerifyCa)).effective_ssl_mode(),
            SslMode::VerifyCa
        );
        assert_eq!(
            with(Some(SslMode::VerifyFull)).effective_ssl_mode(),
            SslMode::VerifyFull
        );
        // The password keeps the chosen mode.
        let password = ConnectionConfig {
            ssl_mode: Some(SslMode::Disable),
            ..Default::default()
        };
        assert_eq!(password.effective_ssl_mode(), SslMode::Disable);
    }

    #[test]
    fn db_auth_labels() {
        assert_eq!(DbAuthMethod::Password.to_string(), "Mot de passe");
        assert_eq!(DbAuthMethod::GcpIam.to_string(), "IAM Google Cloud");
        assert_eq!(DbAuthMethod::default(), DbAuthMethod::Password);
    }

    #[test]
    fn query_result_defaults_to_not_truncated() {
        assert!(!QueryResult::default().truncated);
    }

    #[test]
    fn nosql_types_serialize_by_name_and_family() {
        assert_eq!(
            serde_json::to_string(&DatabaseType::MongoDB).unwrap(),
            "\"MongoDB\""
        );
        assert_eq!(
            serde_json::to_string(&DatabaseType::Redis).unwrap(),
            "\"Redis\""
        );
        assert_eq!(DatabaseType::MongoDB.to_string(), "MongoDB");
        assert_eq!(DatabaseType::Redis.to_string(), "Redis");
        assert_eq!(DatabaseType::MongoDB.family(), Family::Document);
        assert_eq!(DatabaseType::Redis.family(), Family::KeyValue);
        for sql in [
            DatabaseType::Postgres,
            DatabaseType::MySQL,
            DatabaseType::SQLite,
            DatabaseType::SQLServer,
            DatabaseType::Azure,
        ] {
            assert_eq!(sql.family(), Family::Sql);
            assert!(sql.is_sql());
        }
        assert!(!DatabaseType::MongoDB.is_sql() && !DatabaseType::Redis.is_sql());
        assert!(!DatabaseType::MongoDB.runs_tsql_batches());
    }

    #[test]
    fn config_without_uri_or_auth_source_loads_and_they_are_not_written_empty() {
        let c: ConnectionConfig =
            toml::from_str("name = \"x\"\ndb_type = \"Postgres\"\ndatabase = \"d\"").unwrap();
        assert_eq!(c.uri, None);
        assert_eq!(c.auth_source, None);
        let text = toml::to_string(&c).unwrap();
        assert!(
            !text.contains("uri") && !text.contains("auth_source"),
            "{text}"
        );
    }

    #[test]
    fn mongo_config_round_trips() {
        let c = ConnectionConfig {
            name: "atlas".into(),
            db_type: DatabaseType::MongoDB,
            uri: Some("mongodb+srv://jane@cluster0.abc.mongodb.net/?retryWrites=true".into()),
            auth_source: Some("admin".into()),
            password: Some("s3cr3t".into()),
            database: "shop".into(),
            ..Default::default()
        };
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("db_type = \"MongoDB\""), "{text}");
        let back: ConnectionConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
        let redis = ConnectionConfig {
            db_type: DatabaseType::Redis,
            database: "3".into(),
            ..Default::default()
        };
        let back: ConnectionConfig = toml::from_str(&toml::to_string(&redis).unwrap()).unwrap();
        assert_eq!(back, redis);
    }

    #[test]
    fn nosql_tls_from_the_ssl_mode_and_the_uri() {
        let cfg = |db_type: DatabaseType, ssl_mode, uri: Option<&str>| ConnectionConfig {
            db_type,
            ssl_mode,
            uri: uri.map(str::to_string),
            ..Default::default()
        };
        use DatabaseType::{MongoDB, Postgres, Redis};
        for db in [MongoDB, Redis] {
            assert_eq!(cfg(db.clone(), None, None).nosql_tls(), None);
            assert_eq!(
                cfg(db.clone(), Some(SslMode::Disable), None).nosql_tls(),
                None
            );
            assert_eq!(
                cfg(db.clone(), Some(SslMode::Prefer), None).nosql_tls(),
                None
            );
            assert_eq!(
                cfg(db.clone(), Some(SslMode::Require), None).nosql_tls(),
                Some(false)
            );
            assert_eq!(
                cfg(db.clone(), Some(SslMode::VerifyCa), None).nosql_tls(),
                Some(true)
            );
            assert_eq!(
                cfg(db.clone(), Some(SslMode::VerifyFull), None).nosql_tls(),
                Some(true)
            );
        }
        // SQL engines: never.
        assert_eq!(
            cfg(Postgres, Some(SslMode::VerifyFull), None).nosql_tls(),
            None
        );
        // The MongoDB URI wins.
        let srv = Some("mongodb+srv://u@c.example.net/");
        assert_eq!(cfg(MongoDB, None, srv).nosql_tls(), Some(true));
        assert_eq!(
            cfg(MongoDB, Some(SslMode::Require), srv).nosql_tls(),
            Some(false)
        );
        assert_eq!(
            cfg(
                MongoDB,
                None,
                Some("mongodb+srv://c.example.net/?tls=false")
            )
            .nosql_tls(),
            None
        );
        assert_eq!(
            cfg(MongoDB, None, Some("mongodb://h/?ssl=true")).nosql_tls(),
            Some(true)
        );
        assert_eq!(
            cfg(
                MongoDB,
                Some(SslMode::VerifyFull),
                Some("mongodb://h/?tls=false")
            )
            .nosql_tls(),
            None
        );
        assert_eq!(
            cfg(
                MongoDB,
                None,
                Some("mongodb://h/?TLS=true&tlsAllowInvalidCertificates=true")
            )
            .nosql_tls(),
            Some(false)
        );
        assert_eq!(
            cfg(
                MongoDB,
                None,
                Some("mongodb://h/?tlsInsecure=true&tls=true")
            )
            .nosql_tls(),
            Some(false)
        );
        assert_eq!(
            cfg(MongoDB, Some(SslMode::VerifyFull), Some("mongodb://h/")).nosql_tls(),
            Some(true)
        );
        // A blank URI is no URI.
        assert_eq!(cfg(MongoDB, None, Some("  ")).nosql_tls(), None);
    }

    #[test]
    fn nosql_connection_strings_never_contain_the_password() {
        let mongo = ConnectionConfig {
            db_type: DatabaseType::MongoDB,
            username: Some("jane".into()),
            password: Some("s3cr3t".into()),
            port: Some(27017),
            database: "shop".into(),
            ..Default::default()
        };
        assert_eq!(
            mongo.to_connection_string(),
            "mongodb://jane@localhost:27017/shop"
        );
        let typed = ConnectionConfig {
            uri: Some("mongodb://jane:s3cr3t@a:1,b:2/shop?replicaSet=rs".into()),
            ..mongo.clone()
        };
        assert_eq!(
            typed.to_connection_string(),
            "mongodb://jane@a:1,b:2/shop?replicaSet=rs"
        );
        let anonymous = ConnectionConfig {
            username: None,
            ..mongo
        };
        assert_eq!(
            anonymous.to_connection_string(),
            "mongodb://localhost:27017/shop"
        );
        let redis = ConnectionConfig {
            db_type: DatabaseType::Redis,
            host: Some("cache".into()),
            port: Some(6380),
            username: Some("app".into()),
            password: Some("s3cr3t".into()),
            database: String::new(),
            ..Default::default()
        };
        assert_eq!(redis.to_connection_string(), "redis://app@cache:6380/0");
        let tls = ConnectionConfig {
            ssl_mode: Some(SslMode::VerifyFull),
            database: "2".into(),
            ..redis
        };
        assert_eq!(tls.to_connection_string(), "rediss://app@cache:6380/2");
    }
}
