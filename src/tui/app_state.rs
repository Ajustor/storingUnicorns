use std::cell::Cell;
use std::sync::Arc;

use crate::engine::config::AppConfig;
use crate::engine::db::DatabaseConnection;
use crate::engine::models::{display_cell, NULL_CELL};
use crate::engine::models::{
    AzureAuthMethod, ConnectionConfig, DatabaseType, DbAuthMethod, Flavor, QueryResult, SchemaInfo,
    SslMode,
};
use crate::engine::presets;
use crate::engine::services::export_import::{
    BatchExportState, BatchImportState, BatchTruncateState, ExportState, ImportState,
};
use crate::engine::services::query_tabs::QueryTabsState;
use crate::engine::services::table_cache::{FetchQueue, TableCache};
use crate::engine::sql::completion::{self, Suggestion, TableRef};
use crate::engine::sql::statements::single_table_source;
use crate::tui::columns::{self, ColumnStore};
use crate::tui::ui::modals::SchemaAction;

/// Text of a cell in the row editor: NULL is shown (and typed) as `NULL`.
pub fn cell_to_edit_text(cell: &str) -> String {
    display_cell(cell).to_string()
}

/// Value of a cell from the row editor's `text`, the cell holding
/// `original` before: unchanged text keeps the original value (so a stored
/// text "NULL" stays a string), `NULL` typed means NULL.
pub fn cell_from_edit_text(text: &str, original: &str) -> String {
    if text == display_cell(original) {
        original.to_string()
    } else if text == "NULL" {
        NULL_CELL.to_string()
    } else {
        text.to_string()
    }
}

/// Cell of a new row from the add-row editor's `text`: left empty, it gets
/// its default (not inserted); `NULL` typed means NULL.
pub fn insert_cell(text: &str) -> Option<String> {
    match text {
        "" => None,
        "NULL" => Some(NULL_CELL.to_string()),
        _ => Some(text.to_string()),
    }
}

/// Status shown when the current result can't be edited row by row.
const READ_ONLY_RESULT: &str =
    "Read-only result: rows must come from one table with its whole primary key";

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
    DeleteRowConfirm,
    TruncateConfirm,
    BatchTruncate,
    SchemaModify,
    Export,
    Import,
    BatchExport,
    BatchImport,
    /// Path of a connection folder (`folder_prompt`).
    FolderPrompt,
    /// MongoDB / Redis `(o/N)` question (`nosql::Confirm`).
    NoSqlConfirm,
    /// A document / value, indented, read only (`v`).
    JsonView,
    /// The indexes of a MongoDB collection (`s`).
    Indexes,
    /// New index, new key, SCAN pattern (`nosql::Form`).
    NoSqlForm,
    /// MongoDB / Redis export / import (`nosql::transfer`).
    NoSqlTransfer,
}

/// Fields in the new connection dialog
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionField {
    Name,
    /// Pasted connection URL; Enter fills the other fields from it.
    Url,
    Flavor,
    DbType,
    AzureAuth,
    TenantId,
    /// Entra ID application (client) ID of an interactive browser sign-in.
    ClientId,
    Host,
    Port,
    /// PostgreSQL / MySQL sign-in (`DbAuthMethod`), cycled.
    DbAuth,
    Username,
    Password,
    /// MongoDB `authSource` (« Base d'authentification »).
    AuthSource,
    Database,
    SslMode,
    SslCa,
}

impl ConnectionField {
    /// Get the next field in the full chain (Azure-aware cycling handled separately)
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Url,
            Self::Url => Self::Flavor,
            Self::Flavor => Self::DbType,
            Self::DbType => Self::AzureAuth,
            Self::AzureAuth => Self::TenantId,
            Self::TenantId => Self::ClientId,
            Self::ClientId => Self::Host,
            Self::Host => Self::Port,
            Self::Port => Self::DbAuth,
            Self::DbAuth => Self::Username,
            Self::Username => Self::Password,
            Self::Password => Self::AuthSource,
            Self::AuthSource => Self::Database,
            Self::Database => Self::SslMode,
            Self::SslMode => Self::SslCa,
            Self::SslCa => Self::Name,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Name => Self::SslCa,
            Self::Url => Self::Name,
            Self::Flavor => Self::Url,
            Self::DbType => Self::Flavor,
            Self::AzureAuth => Self::DbType,
            Self::TenantId => Self::AzureAuth,
            Self::ClientId => Self::TenantId,
            Self::Host => Self::ClientId,
            Self::Port => Self::Host,
            Self::DbAuth => Self::Port,
            Self::Username => Self::DbAuth,
            Self::Password => Self::Username,
            Self::AuthSource => Self::Password,
            Self::Database => Self::AuthSource,
            Self::SslMode => Self::Database,
            Self::SslCa => Self::SslMode,
        }
    }

    /// Get next field, skipping the fields the engine and its sign-in
    /// (Azure `auth_method`, PostgreSQL / MySQL `db_auth`) don't use.
    pub fn next_for(
        self,
        db_type: &DatabaseType,
        auth_method: &AzureAuthMethod,
        db_auth: DbAuthMethod,
    ) -> Self {
        let mut field = self.next();
        while Self::should_skip(&field, db_type, auth_method, db_auth) {
            field = field.next();
        }
        field
    }

    /// Get previous field, skipping the fields not used (see `next_for`).
    pub fn prev_for(
        self,
        db_type: &DatabaseType,
        auth_method: &AzureAuthMethod,
        db_auth: DbAuthMethod,
    ) -> Self {
        let mut field = self.prev();
        while Self::should_skip(&field, db_type, auth_method, db_auth) {
            field = field.prev();
        }
        field
    }

    /// Whether this field should be skipped based on context
    fn should_skip(
        field: &Self,
        db_type: &DatabaseType,
        auth_method: &AzureAuthMethod,
        db_auth: DbAuthMethod,
    ) -> bool {
        match field {
            // MongoDB / Redis have no compatible products.
            Self::Flavor => !db_type.is_sql(),
            Self::AuthSource => *db_type != DatabaseType::MongoDB,
            Self::DbAuth => !uses_tls(db_type),
            // The IAM sign-in's password is a token fetched when connecting.
            Self::Password if uses_tls(db_type) => db_auth == DbAuthMethod::GcpIam,
            Self::AzureAuth => *db_type != DatabaseType::Azure,
            // Azure CLI and the browser sign-in.
            Self::TenantId => {
                *db_type != DatabaseType::Azure
                    || !matches!(
                        auth_method,
                        AzureAuthMethod::Interactive | AzureAuthMethod::InteractiveBrowser
                    )
            }
            Self::ClientId => {
                *db_type != DatabaseType::Azure
                    || *auth_method != AzureAuthMethod::InteractiveBrowser
            }
            // Azure CLI needs neither; a managed identity only its client
            // ID, kept in the username; the browser sign-in only the
            // account (optional), kept in the username too.
            Self::Username => {
                *db_type == DatabaseType::Azure && *auth_method == AzureAuthMethod::Interactive
            }
            Self::Password => {
                *db_type == DatabaseType::Azure && *auth_method != AzureAuthMethod::Credentials
            }
            Self::SslMode | Self::SslCa => !shows_ssl(db_type),
            _ => false,
        }
    }
}

/// Whether the driver honours the SSL settings (PostgreSQL / MySQL).
pub fn uses_tls(db_type: &DatabaseType) -> bool {
    matches!(db_type, DatabaseType::Postgres | DatabaseType::MySQL)
}

/// Whether the form shows the SSL / TLS mode and CA: PostgreSQL / MySQL,
/// and MongoDB / Redis (TLS, `ConnectionConfig::nosql_tls`).
pub fn shows_ssl(db_type: &DatabaseType) -> bool {
    uses_tls(db_type) || !db_type.is_sql()
}

/// A driver's default user, cleared for the engines without one.
fn is_default_user(user: &str) -> bool {
    matches!(user.trim(), "postgres" | "root" | "sa")
}

/// A SQL driver's default database, cleared for MongoDB (every database).
fn is_default_database(db: &str) -> bool {
    matches!(db.trim(), "postgres" | "master" | "mysql")
}

/// Empty, or a host `set_db_type` fills in: replaceable by another default.
fn is_default_host(host: &str) -> bool {
    let h = host.trim();
    h.is_empty() || h == "localhost" || h == "servername.database.windows.net"
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
    pub azure_auth_method: AzureAuthMethod,
    /// Azure CLI's tenant (`common` by default), or the browser sign-in's:
    /// empty (auto) by default, its server's.
    pub tenant_id: String,
    /// Entra ID application (client) ID of an interactive browser sign-in;
    /// empty (auto): Microsoft's drivers'.
    pub azure_client_id: String,
    /// Pasted connection URL; for MongoDB, the connection string kept as
    /// `ConnectionConfig::uri` (its password moves to `password`).
    pub url: String,
    /// MongoDB `authSource`; empty = the URI's, else `admin`.
    pub auth_source: String,
    /// Compatible product; `None` = the bare engine.
    pub flavor: Option<Flavor>,
    pub ssl_mode: SslMode,
    /// CA certificate path; empty = system roots only.
    pub ssl_ca: String,
    /// Kept so that editing a connection doesn't drop its GUI colour.
    pub color: Option<[u8; 3]>,
    /// PostgreSQL / MySQL sign-in; kept while another engine is chosen.
    pub db_auth: DbAuthMethod,
    /// Kept so that editing a connection doesn't move it out of its folder.
    pub folder: Option<String>,
    /// Kept so that editing a connection doesn't show its hidden schemas.
    pub hidden_schemas: Vec<String>,
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
            azure_auth_method: AzureAuthMethod::default(),
            tenant_id: String::from("common"),
            azure_client_id: String::new(),
            url: String::new(),
            auth_source: String::new(),
            flavor: None,
            ssl_mode: SslMode::default(),
            ssl_ca: String::new(),
            color: None,
            db_auth: DbAuthMethod::default(),
            folder: None,
            hidden_schemas: Vec::new(),
        }
    }
}

impl NewConnectionState {
    /// Whether the connection signs in with Google Cloud IAM.
    pub fn gcp_iam(&self) -> bool {
        uses_tls(&self.db_type) && self.db_auth == DbAuthMethod::GcpIam
    }

    /// Switch between the password and the Google Cloud IAM sign-in; a
    /// driver's default user (no Google account) is cleared for the latter.
    pub fn cycle_db_auth(&mut self) {
        self.db_auth = match self.db_auth {
            DbAuthMethod::Password => DbAuthMethod::GcpIam,
            DbAuthMethod::GcpIam => DbAuthMethod::Password,
        };
        let u = self.username.trim();
        if self.db_auth == DbAuthMethod::GcpIam && (u == "postgres" || u == "root") {
            self.username.clear();
        }
    }

    /// Whether the form can be saved, else a French message.
    pub fn check(&self) -> Result<(), String> {
        if self.db_type == DatabaseType::MongoDB {
            let uri = self.url.trim();
            let scheme = uri.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
            if !uri.is_empty() && !matches!(scheme.as_deref(), Some("mongodb" | "mongodb+srv")) {
                return Err("L'URI MongoDB doit commencer par mongodb:// ou mongodb+srv://".into());
            }
            if uri.is_empty() && self.host.trim().is_empty() {
                return Err("Indique un hôte ou une URI MongoDB".into());
            }
        }
        if self.db_type == DatabaseType::Redis {
            let db = self.database.trim();
            if !db.is_empty() && db.parse::<u8>().map_or(true, |n| n > 15) {
                return Err("La base Redis doit être un nombre de 0 à 15".into());
            }
        }
        if self.gcp_iam() {
            crate::engine::db::gcp_iam::validate(&self.db_type, &self.username)?;
        }
        if self.db_type == DatabaseType::Azure
            && self.azure_auth_method == AzureAuthMethod::ManagedIdentity
        {
            crate::engine::db::azure::managed_identity_client_id(Some(&self.username))
                .map_err(|e| e.to_string())?;
        }
        if self.db_type == DatabaseType::Azure
            && self.azure_auth_method == AzureAuthMethod::InteractiveBrowser
        {
            crate::engine::db::entra_interactive::validate_interactive(
                &self.tenant_id,
                &self.azure_client_id,
            )?;
        }
        Ok(())
    }

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
            // A MongoDB URI carries its user: the field is ignored then.
            username: if self.username.is_empty() || self.mongo_uri() {
                None
            } else {
                Some(self.username.clone())
            },
            // The IAM sign-in's password is a token, fetched when connecting.
            password: if self.password.is_empty() || self.gcp_iam() {
                None
            } else {
                Some(self.password.clone())
            },
            database: self.database.clone(),
            azure_auth_method: if self.db_type == DatabaseType::Azure {
                Some(self.azure_auth_method.clone())
            } else {
                None
            },
            // Empty: Azure CLI's default tenant, the browser sign-in's
            // server's (auto).
            tenant_id: if self.db_type == DatabaseType::Azure
                && matches!(
                    self.azure_auth_method,
                    AzureAuthMethod::Interactive | AzureAuthMethod::InteractiveBrowser
                )
                && !self.tenant_id.trim().is_empty()
            {
                Some(self.tenant_id.trim().to_string())
            } else {
                None
            },
            azure_client_id: if self.db_type == DatabaseType::Azure
                && self.azure_auth_method == AzureAuthMethod::InteractiveBrowser
                && !self.azure_client_id.trim().is_empty()
            {
                Some(self.azure_client_id.trim().to_string())
            } else {
                None
            },
            color: self.color,
            ssl_mode: shows_ssl(&self.db_type).then_some(self.ssl_mode),
            ssl_ca: if shows_ssl(&self.db_type) && !self.ssl_ca.trim().is_empty() {
                Some(self.ssl_ca.trim().into())
            } else {
                None
            },
            flavor: self.flavor.filter(|f| f.driver() == self.db_type),
            // The default method is left out, as in older files.
            db_auth: self.gcp_iam().then_some(DbAuthMethod::GcpIam),
            folder: self.folder.clone(),
            hidden_schemas: self.hidden_schemas.clone(),
            // The connection string never keeps its password.
            uri: (self.db_type == DatabaseType::MongoDB && !self.url.trim().is_empty())
                .then(|| presets::split_uri_password(&self.url).0),
            auth_source: (self.db_type == DatabaseType::MongoDB
                && !self.auth_source.trim().is_empty())
            .then(|| self.auth_source.trim().to_string()),
        }
    }

    /// MongoDB with a connection string (its user is the URI's).
    pub fn mongo_uri(&self) -> bool {
        self.db_type == DatabaseType::MongoDB && !self.url.trim().is_empty()
    }

    /// `to_config`, once the password of a MongoDB URI typed in the form
    /// moved to the password field (unless one is set there).
    pub fn config_moving_password(&mut self) -> ConnectionConfig {
        if self.db_type == DatabaseType::MongoDB {
            let (uri, pw) = presets::split_uri_password(&self.url);
            if let Some(pw) = pw {
                if self.password.is_empty() {
                    self.password = pw;
                }
                self.url = uri;
            }
        }
        self.to_config()
    }

    /// Edit form for an existing connection.
    pub fn from_config(conn: &ConnectionConfig) -> Self {
        let azure_auth_method = conn
            .azure_auth_method
            .clone()
            .unwrap_or(AzureAuthMethod::Credentials);
        // Without one, the browser sign-in asks the server for its tenant:
        // the form's default (Azure CLI's) would be refused there.
        let tenant_id = conn.tenant_id.clone().unwrap_or_else(|| {
            if azure_auth_method == AzureAuthMethod::InteractiveBrowser {
                String::new()
            } else {
                "common".to_string()
            }
        });
        Self {
            name: conn.name.clone(),
            db_type: conn.db_type.clone(),
            host: conn.host.clone().unwrap_or_default(),
            port: conn.port.map(|p| p.to_string()).unwrap_or_default(),
            username: conn.username.clone().unwrap_or_default(),
            password: conn.password.clone().unwrap_or_default(),
            database: conn.database.clone(),
            azure_auth_method,
            tenant_id,
            azure_client_id: conn.azure_client_id.clone().unwrap_or_default(),
            active_field: ConnectionField::Name,
            cursor_position: conn.name.len(),
            // A MongoDB connection string is edited in the URL field.
            url: match conn.db_type {
                DatabaseType::MongoDB => conn.uri.clone().unwrap_or_default(),
                _ => String::new(),
            },
            auth_source: conn.auth_source.clone().unwrap_or_default(),
            flavor: conn.flavor,
            ssl_mode: conn.ssl_mode.unwrap_or_default(),
            ssl_ca: conn
                .ssl_ca
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            color: conn.color,
            db_auth: conn.effective_db_auth(),
            folder: conn.folder.clone(),
            hidden_schemas: conn.hidden_schemas.clone(),
        }
    }

    pub fn get_active_field_value(&self) -> &str {
        match self.active_field {
            ConnectionField::Name => &self.name,
            ConnectionField::Url => &self.url,
            ConnectionField::SslCa => &self.ssl_ca,
            // Cycled with ←/→, like the type.
            ConnectionField::Flavor | ConnectionField::SslMode | ConnectionField::DbAuth => "",
            ConnectionField::DbType => "",    // handled separately
            ConnectionField::AzureAuth => "", // handled separately (cycle)
            ConnectionField::TenantId => &self.tenant_id,
            ConnectionField::ClientId => &self.azure_client_id,
            ConnectionField::Host => &self.host,
            ConnectionField::Port => &self.port,
            ConnectionField::Username => &self.username,
            ConnectionField::Password => &self.password,
            ConnectionField::AuthSource => &self.auth_source,
            ConnectionField::Database => &self.database,
        }
    }

    pub fn get_active_field_mut(&mut self) -> Option<&mut String> {
        match self.active_field {
            ConnectionField::Name => Some(&mut self.name),
            ConnectionField::Url => Some(&mut self.url),
            ConnectionField::SslCa => Some(&mut self.ssl_ca),
            // Cycled with ←/→, like the type.
            ConnectionField::Flavor | ConnectionField::SslMode | ConnectionField::DbAuth => None,
            ConnectionField::DbType => None,    // handled separately
            ConnectionField::AzureAuth => None, // handled separately (cycle)
            ConnectionField::TenantId => Some(&mut self.tenant_id),
            ConnectionField::ClientId => Some(&mut self.azure_client_id),
            ConnectionField::Host => Some(&mut self.host),
            ConnectionField::Port => Some(&mut self.port),
            ConnectionField::Username => Some(&mut self.username),
            ConnectionField::Password => Some(&mut self.password),
            ConnectionField::AuthSource => Some(&mut self.auth_source),
            ConnectionField::Database => Some(&mut self.database),
        }
    }

    pub fn cycle_azure_auth_method(&mut self) {
        self.azure_auth_method = match self.azure_auth_method {
            AzureAuthMethod::Credentials => AzureAuthMethod::Interactive,
            AzureAuthMethod::Interactive => AzureAuthMethod::InteractiveBrowser,
            AzureAuthMethod::InteractiveBrowser => AzureAuthMethod::ManagedIdentity,
            AzureAuthMethod::ManagedIdentity => AzureAuthMethod::Credentials,
        };
        // The form's default tenant suits Azure CLI, not the browser sign-in
        // (which refuses it): start that one empty, its server's tenant.
        if self.azure_auth_method == AzureAuthMethod::InteractiveBrowser
            && self.tenant_id.trim().eq_ignore_ascii_case("common")
        {
            self.tenant_id.clear();
        }
    }

    pub fn cycle_db_type(&mut self) {
        self.set_db_type(match self.db_type {
            DatabaseType::Postgres => DatabaseType::MySQL,
            DatabaseType::MySQL => DatabaseType::SQLite,
            DatabaseType::SQLite => DatabaseType::SQLServer,
            DatabaseType::SQLServer => DatabaseType::Azure,
            DatabaseType::Azure => DatabaseType::MongoDB,
            DatabaseType::MongoDB => DatabaseType::Redis,
            DatabaseType::Redis => DatabaseType::Postgres,
        });
    }

    /// `cycle_db_type` backwards.
    pub fn cycle_db_type_back(&mut self) {
        self.set_db_type(match self.db_type {
            DatabaseType::Postgres => DatabaseType::Redis,
            DatabaseType::MySQL => DatabaseType::Postgres,
            DatabaseType::SQLite => DatabaseType::MySQL,
            DatabaseType::SQLServer => DatabaseType::SQLite,
            DatabaseType::Azure => DatabaseType::SQLServer,
            DatabaseType::MongoDB => DatabaseType::Azure,
            DatabaseType::Redis => DatabaseType::MongoDB,
        });
    }

    /// Next product (None = bare engine, then `Flavor::ALL`, then None
    /// again), applying its preset: driver, port, SSL mode, and the user
    /// when it is empty or a driver default. A typed host is never
    /// overwritten; a default one follows the driver like `cycle_db_type`.
    pub fn cycle_flavor(&mut self) {
        self.step_flavor(true);
    }

    /// `cycle_flavor` backwards: None, last product, …, first, None.
    pub fn cycle_flavor_back(&mut self) {
        self.step_flavor(false);
    }

    fn step_flavor(&mut self, forward: bool) {
        // Position in [None, ALL[0], …, ALL[n-1]], moved one step with wrap.
        let len = Flavor::ALL.len() + 1;
        let pos = self
            .flavor
            .and_then(|f| Flavor::ALL.iter().position(|x| *x == f))
            .map_or(0, |i| i + 1);
        let pos = if forward {
            (pos + 1) % len
        } else {
            (pos + len - 1) % len
        };
        let next = pos.checked_sub(1).map(|i| Flavor::ALL[i]);
        let Some(f) = next else {
            self.flavor = None;
            return;
        };
        let p = presets::preset(f);
        if p.db_type != self.db_type {
            let host = std::mem::take(&mut self.host);
            self.set_db_type(p.db_type);
            if !is_default_host(&host) {
                self.host = host;
            }
        }
        self.flavor = next;
        self.port = p.port.to_string();
        self.ssl_mode = p.ssl_mode;
        let u = self.username.trim();
        if u.is_empty() || u == "postgres" || u == "root" || u == "sa" {
            self.username = p.username.to_string();
        }
    }

    pub fn cycle_ssl_mode(&mut self) {
        self.step_ssl_mode(1);
    }

    pub fn cycle_ssl_mode_back(&mut self) {
        self.step_ssl_mode(SslMode::ALL.len() - 1);
    }

    fn step_ssl_mode(&mut self, step: usize) {
        let i = SslMode::ALL
            .iter()
            .position(|m| *m == self.ssl_mode)
            .unwrap_or(0);
        self.ssl_mode = SslMode::ALL[(i + step) % SslMode::ALL.len()];
    }

    /// Fill the form from `self.url` (see the SSL / password rules below);
    /// on error nothing changes.
    pub fn apply_url(&mut self) -> Result<(), String> {
        let p = presets::parse_url(&self.url)?;
        self.set_db_type(p.db_type);
        self.flavor = p.flavor;
        if let Some(h) = p.host {
            self.host = h;
        }
        if let Some(n) = p.port {
            self.port = n.to_string();
        }
        if let Some(u) = p.username {
            self.username = u;
        }
        if let Some(pw) = p.password {
            self.password = pw;
        }
        if let Some(d) = p.database {
            self.database = d;
        }
        // MongoDB: the connection string (hosts, options) stays in the
        // field, without its password.
        let uri = p.uri.unwrap_or_default();
        // The URL describes the whole connection: SSL settings it doesn't
        // mention go back to their defaults. A missing password keeps the
        // typed one (URLs are often shared without it).
        self.ssl_mode = p.ssl_mode.unwrap_or_default();
        self.ssl_ca = p
            .ssl_ca
            .map(|ca| ca.display().to_string())
            .unwrap_or_default();
        self.url = uri;
        Ok(())
    }

    /// Switch the driver and apply its default port / host (and user for
    /// SQL Server); a product of another driver is dropped.
    pub fn set_db_type(&mut self, db_type: DatabaseType) {
        if self.flavor.is_some_and(|f| f.driver() != db_type) {
            self.flavor = None;
        }
        self.db_type = db_type;
        match self.db_type {
            DatabaseType::Postgres => {
                self.port = String::from("5432");
                self.host = String::from("localhost");
            }
            DatabaseType::MySQL => {
                self.port = String::from("3306");
                self.host = String::from("localhost");
            }
            DatabaseType::SQLite => {
                self.port = String::new();
                self.host = String::new();
            }
            DatabaseType::SQLServer => {
                self.port = String::from("1433");
                self.username = String::from("sa");
                self.host = String::from("localhost");
            }
            DatabaseType::Azure => {
                self.port = String::from("1433");
                self.host = String::from("servername.database.windows.net");
            }
            DatabaseType::MongoDB => {
                self.port = String::from("27017");
                self.host = String::from("localhost");
                // Every database; no driver default user.
                if is_default_database(&self.database) {
                    self.database.clear();
                }
                if is_default_user(&self.username) {
                    self.username.clear();
                }
            }
            DatabaseType::Redis => {
                self.port = String::from("6379");
                self.host = String::from("localhost");
                if self.database.trim().parse::<u8>().map_or(true, |n| n > 15) {
                    self.database = String::from("0");
                }
                if is_default_user(&self.username) {
                    self.username.clear();
                }
            }
        };
    }
}

/// Main application state
pub struct AppState {
    pub config: AppConfig,
    pub active_panel: ActivePanel,
    /// Shared with the background loads of columns (`columns::request`).
    pub connection: Option<Arc<DatabaseConnection>>,
    pub current_connection_config: Option<ConnectionConfig>,

    // Connection list state
    pub selected_connection: usize,
    /// Folder under the cursor; `None`: the cursor is on `selected_connection`.
    pub selected_folder: Option<String>,
    /// Folders folded in the Connections panel (for the session).
    pub collapsed_folders: std::collections::HashSet<String>,
    pub connections_scroll: usize,
    pub folder_prompt: Option<crate::tui::folders::FolderPrompt>,
    /// Show the hidden schemas (greyed) in the Tables panel.
    pub show_hidden_schemas: bool,
    /// Configuration writes asked for (`persist_config`).
    #[cfg(test)]
    pub config_saves: usize,

    // Tables list state
    pub tables: Vec<String>,
    pub schemas: Vec<SchemaInfo>,
    pub selected_schema: usize,
    pub selected_table: usize,
    pub tables_scroll: usize,
    pub tables_filter: String,       // Filter text for tables
    pub tables_filter_active: bool,  // Whether filter input is active
    pub tables_filter_cursor: usize, // Cursor position in tables filter

    // Query tabs state
    pub query_tabs: QueryTabsState,

    // Text selection state
    pub selection_start: Option<usize>, // Selection anchor position
    pub selection_end: Option<usize>,   // Selection end position (cursor)

    // Panel sizes (percentages)
    pub sidebar_width: u16,       // Width of sidebar (default: 25)
    pub query_editor_height: u16, // Height of query editor (default: 40)

    // Autocompletion state
    pub completion_suggestions: Vec<Suggestion>,
    pub completion_selected: usize,
    pub show_completion: bool,
    /// Completion asked for (Ctrl+Space) and not dismissed: refreshed when
    /// the columns it waits for arrive.
    pub completion_wanted: bool,
    /// Tab, cursor and text the completion was asked for: columns arriving
    /// after the user moved on don't open it.
    pub completion_anchor: Option<(usize, usize, String)>,
    /// Columns of the cited tables, for completion.
    pub completion_columns: ColumnStore,
    pub known_columns: Vec<String>, // Columns from last query result

    // Table column cache for autocompletion
    pub table_cache: TableCache,
    #[allow(dead_code)]
    pub fetch_queue: FetchQueue,

    // Results state
    pub query_result: Option<QueryResult>,
    /// The SQL that produced `query_result` (`None` for a transaction block).
    pub result_sql: Option<String>,
    pub results_scroll: usize,
    pub results_scroll_x: usize,             // Horizontal scroll offset
    pub results_visible_height: Cell<usize>, // Updated each frame by render
    pub selected_row: usize,
    pub results_filter: String,       // Filter text for results
    pub results_filter_active: bool,  // Whether filter input is active
    pub results_filter_cursor: usize, // Cursor position in results filter

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

    // Schema modification state
    pub schema_action: Option<SchemaAction>,
    pub schema_table_name: Option<String>,
    pub schema_field_index: usize,
    pub schema_cursor_pos: usize,
    /// Pending operation for column selection: "view", "modify", "drop", "rename"
    pub schema_pending_operation: Option<String>,

    // Cached column widths for results (computed once when results change)
    pub cached_col_widths: Vec<u16>,

    // Export/Import state
    pub export_state: Option<ExportState>,
    pub import_state: Option<ImportState>,
    pub batch_export_state: Option<BatchExportState>,
    pub batch_import_state: Option<BatchImportState>,
    pub batch_truncate_state: Option<BatchTruncateState>,
    /// Table name pending truncation confirmation
    pub truncate_table_name: Option<String>,

    // Status
    pub status_message: String,
    pub is_loading: bool,
    pub is_connecting: bool,
    pub connection_error: Option<String>,
    /// The connection being opened in the background (Esc cancels it).
    pub pending_connect: Option<crate::tui::connect::PendingConnect>,
    /// `(connection, url)` of the browser sign-in waiting for the user.
    pub sign_in: Option<(String, String)>,
    /// Keyring warning, shown with the outcome of the connection being
    /// opened.
    pub sign_in_warning: Option<String>,
    /// The engine's sign-in interactions (`connect::listen`).
    pub interactions:
        Option<std::sync::mpsc::Receiver<crate::engine::db::entra_interactive::Interaction>>,

    /// MongoDB / Redis state: key tree, editable result, dialogs.
    pub nosql: crate::tui::nosql::NoSqlUi,

    // Debug mode
    pub debug_mode: bool,
    // Disable all animations
    pub no_animations: bool,

    // App control
    pub should_quit: bool,
}

impl AppState {
    pub fn new(config: AppConfig, debug_mode: bool, no_animations: bool) -> Self {
        Self {
            config,
            active_panel: ActivePanel::Connections,
            connection: None,
            current_connection_config: None,
            selected_connection: 0,
            selected_folder: None,
            collapsed_folders: Default::default(),
            connections_scroll: 0,
            folder_prompt: None,
            show_hidden_schemas: false,
            #[cfg(test)]
            config_saves: 0,
            tables: Vec::new(),
            schemas: Vec::new(),
            selected_schema: 0,
            selected_table: 0,
            tables_scroll: 0,
            tables_filter: String::new(),
            tables_filter_active: false,
            tables_filter_cursor: 0,
            query_tabs: QueryTabsState::load().unwrap_or_default(),
            selection_start: None,
            selection_end: None,
            sidebar_width: 25,
            query_editor_height: 40,
            completion_suggestions: Vec::new(),
            completion_selected: 0,
            show_completion: false,
            completion_wanted: false,
            completion_anchor: None,
            completion_columns: ColumnStore::default(),
            known_columns: Vec::new(),
            table_cache: TableCache::default(),
            fetch_queue: FetchQueue::default(),
            query_result: None,
            result_sql: None,
            results_scroll: 0,
            results_scroll_x: 0,
            results_visible_height: Cell::new(0),
            selected_row: 0,
            results_filter: String::new(),
            results_filter_active: false,
            results_filter_cursor: 0,
            editing_row: None,
            original_editing_row: None,
            editing_table_name: None,
            editing_column: 0,
            editing_cursor: 0,
            system_columns: Vec::new(),
            dialog_mode: DialogMode::None,
            new_connection: NewConnectionState::default(),
            editing_connection_index: None,
            schema_action: None,
            schema_table_name: None,
            schema_field_index: 0,
            schema_cursor_pos: 0,
            schema_pending_operation: None,
            cached_col_widths: Vec::new(),
            export_state: None,
            import_state: None,
            batch_export_state: None,
            batch_import_state: None,
            batch_truncate_state: None,
            truncate_table_name: None,
            status_message: String::from("Press ? for help"),
            is_loading: false,
            is_connecting: false,
            connection_error: None,
            pending_connect: None,
            sign_in: None,
            sign_in_warning: None,
            interactions: None,
            nosql: Default::default(),
            debug_mode,
            no_animations,
            should_quit: false,
        }
    }

    pub fn open_new_connection_dialog(&mut self) {
        self.new_connection = NewConnectionState {
            // Created in the folder of the cursor.
            folder: self.cursor_folder(),
            ..Default::default()
        };
        self.editing_connection_index = None;
        self.dialog_mode = DialogMode::NewConnection;
    }

    pub fn open_edit_connection_dialog(&mut self, index: usize) {
        if let Some(conn) = self.config.connections.get(index) {
            self.new_connection = NewConnectionState::from_config(conn);
            self.editing_connection_index = Some(index);
            self.dialog_mode = DialogMode::EditConnection;
        }
    }

    pub fn close_dialog(&mut self) {
        self.dialog_mode = DialogMode::None;
        self.editing_connection_index = None;
        self.schema_action = None;
        self.schema_table_name = None;
        self.schema_field_index = 0;
        self.schema_cursor_pos = 0;
        self.schema_pending_operation = None;
        self.export_state = None;
        self.import_state = None;
        self.batch_export_state = None;
        self.batch_import_state = None;
        self.folder_prompt = None;
        self.nosql.close_dialogs();
    }

    pub fn is_dialog_open(&self) -> bool {
        self.dialog_mode != DialogMode::None
    }

    /// Open the schema modification dialog for the selected table
    pub fn open_schema_dialog(&mut self) {
        if let Some(table_name) = self.get_selected_table_full_name() {
            self.schema_table_name = Some(table_name);
            self.schema_action = None;
            self.schema_field_index = 0;
            self.schema_cursor_pos = 0;
            self.schema_pending_operation = None;
            self.dialog_mode = DialogMode::SchemaModify;
        }
    }

    /// Open schema dialog with a specific action
    pub fn open_schema_action(&mut self, action: SchemaAction) {
        self.schema_action = Some(action);
        self.schema_field_index = 0;
        self.schema_cursor_pos = 0;
    }

    /// Open the export dialog for current results
    pub fn open_export_dialog(&mut self) {
        if let Some(ref result) = self.query_result {
            let table_name = self.extract_table_from_query();
            self.export_state = Some(ExportState::new(table_name, result));
            self.dialog_mode = DialogMode::Export;
        }
    }

    /// Open the import dialog
    pub fn open_import_dialog(&mut self) {
        let table_name = self.extract_table_from_query();
        self.import_state = Some(ImportState::new(table_name));
        self.dialog_mode = DialogMode::Import;
    }

    /// Open the batch export dialog
    pub fn open_batch_export_dialog(&mut self) {
        self.batch_export_state = Some(BatchExportState::new(&self.schemas));
        self.dialog_mode = DialogMode::BatchExport;
    }

    /// Open the batch import dialog
    pub fn open_batch_import_dialog(&mut self) {
        let mut batch_state = BatchImportState::new(&self.schemas);
        batch_state.auto_select_matching_files();
        self.batch_import_state = Some(batch_state);
        self.dialog_mode = DialogMode::BatchImport;
    }

    /// Open delete row confirmation dialog
    pub fn open_delete_row_confirm(&mut self) {
        if self.query_result.is_some() {
            self.editing_table_name = self.editable_result_table();
            if self.editing_table_name.is_none() {
                self.set_status(READ_ONLY_RESULT);
                return;
            }
            self.dialog_mode = DialogMode::DeleteRowConfirm;
        }
    }

    /// Open truncate table confirmation dialog
    pub fn open_truncate_confirm(&mut self) {
        if let Some(table_name) = self.get_selected_table_full_name() {
            self.truncate_table_name = Some(table_name);
            self.dialog_mode = DialogMode::TruncateConfirm;
        }
    }

    /// Open batch truncate dialog
    pub fn open_batch_truncate_dialog(&mut self) {
        self.batch_truncate_state = Some(BatchTruncateState::new(&self.schemas));
        self.dialog_mode = DialogMode::BatchTruncate;
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

    /// Update known columns from query result (for autocompletion)
    pub fn update_known_columns(&mut self) {
        if let Some(ref result) = self.query_result {
            self.known_columns = result.columns.iter().map(|c| c.name.clone()).collect();
        }
    }

    /// Compute and cache column widths for the results table.
    /// Call this once when query_result changes, not every frame.
    pub fn compute_col_widths(&mut self) {
        if let Some(ref result) = self.query_result {
            self.cached_col_widths = result
                .columns
                .iter()
                .enumerate()
                .map(|(col_idx, col)| {
                    let mut max_len = col.name.len();
                    // Sample up to 200 rows for performance
                    for row in result.rows.iter().take(200) {
                        if let Some(cell) = row.get(col_idx) {
                            max_len = max_len.max(cell.len());
                        }
                    }
                    // Cap at 30 chars max, minimum 25
                    (max_len.min(30).max(25)) as u16
                })
                .collect();
        } else {
            self.cached_col_widths.clear();
        }
    }

    /// Known tables for completion: `schema.table`, but the hidden schemas'.
    pub fn get_known_tables(&self) -> Vec<String> {
        self.schemas
            .iter()
            .filter(|schema| !self.is_schema_hidden(&schema.name))
            .flat_map(|schema| {
                schema
                    .tables
                    .iter()
                    .map(move |table| format!("{}.{}", schema.name, table))
            })
            .collect()
    }

    /// Suggestions for the cursor, from the tables the statement cites
    /// (their missing columns are loaded in the background).
    fn compute_completions(&mut self) -> Vec<Suggestion> {
        if crate::tui::nosql::engine(self).is_some() {
            return crate::tui::nosql::completions(self);
        }

        let query = self.query_input().to_string();
        let cursor = self.cursor_position();
        let tables = self.get_known_tables();
        let store = &self.completion_columns;
        let columns = |t: &TableRef| store.get(t);
        let cx = completion::Context {
            quotes: self.get_quote_chars(),
            tables: &tables,
            columns: &columns,
            fallback_columns: &self.known_columns,
        };
        let out = completion::complete(&query, cursor, &cx);
        for table in &out.missing {
            columns::request(self, table);
        }
        out.items
    }

    /// Update completion suggestions based on current input
    pub fn update_completions(&mut self) {
        self.completion_suggestions = self.compute_completions();
        self.completion_selected = 0;
        self.show_completion = !self.completion_suggestions.is_empty();
        self.completion_wanted = true;
        self.completion_anchor = Some(self.completion_position());
    }

    /// Where the editor is: tab, cursor and text.
    fn completion_position(&self) -> (usize, usize, String) {
        (
            self.query_tabs.active_tab,
            self.cursor_position(),
            self.query_input().to_string(),
        )
    }

    /// Whether the user is still where the completion was asked for (the
    /// editor focused, same tab, cursor and text).
    pub fn completion_still_wanted(&self) -> bool {
        self.completion_wanted
            && self.active_panel == ActivePanel::QueryEditor
            && !self.is_dialog_open()
            && self.completion_anchor.as_ref() == Some(&self.completion_position())
    }

    /// Recompute the suggestions asked for (columns arrived), keeping the
    /// selected one when it is still offered.
    pub fn refresh_completions(&mut self) {
        let selected = self
            .completion_suggestions
            .get(self.completion_selected)
            .map(|s| s.label.clone());
        self.completion_suggestions = self.compute_completions();
        self.completion_selected = selected
            .and_then(|l| {
                self.completion_suggestions
                    .iter()
                    .position(|s| s.label == l)
            })
            .unwrap_or(0);
        self.show_completion = !self.completion_suggestions.is_empty();
    }

    /// Apply the selected completion: it replaces the token before the cursor.
    pub fn apply_completion(&mut self) {
        if !self.show_completion || self.completion_suggestions.is_empty() {
            return;
        }

        let insert = self.completion_suggestions[self.completion_selected]
            .insert
            .clone();
        let query = self.query_input().to_string();
        let cursor = self.cursor_position().min(query.len());
        // Same token the engine completed (quotes in strings / comments
        // ignored; MongoDB / Redis: the NoSQL engine's span).
        let token_start = match crate::tui::nosql::engine(self) {
            Some(db) => crate::engine::nosql::completion_start(&db, &query, cursor),
            None => crate::engine::sql::completion::token_start(&query, cursor),
        };

        let new_query = format!("{}{}{}", &query[..token_start], insert, &query[cursor..]);
        let new_cursor = token_start + insert.len();

        self.set_query(new_query);
        self.set_cursor_position(new_cursor);
        self.hide_completion();
    }

    /// Hide completion popup
    pub fn hide_completion(&mut self) {
        self.show_completion = false;
        self.completion_wanted = false;
        self.completion_anchor = None;
        self.completion_suggestions.clear();
        self.completion_selected = 0;
    }

    /// Navigate completion suggestions
    pub fn completion_next(&mut self) {
        if !self.completion_suggestions.is_empty() {
            self.completion_selected =
                (self.completion_selected + 1) % self.completion_suggestions.len();
        }
    }

    pub fn completion_prev(&mut self) {
        if !self.completion_suggestions.is_empty() {
            self.completion_selected = self
                .completion_selected
                .checked_sub(1)
                .unwrap_or(self.completion_suggestions.len() - 1);
        }
    }

    pub fn next_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Connections => ActivePanel::Tables,
            ActivePanel::Tables => ActivePanel::QueryEditor,
            ActivePanel::QueryEditor => {
                if self.should_show_results() {
                    ActivePanel::Results
                } else {
                    ActivePanel::Connections
                }
            }
            ActivePanel::Results => ActivePanel::Connections,
        };
    }

    pub fn prev_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Connections => {
                if self.should_show_results() {
                    ActivePanel::Results
                } else {
                    ActivePanel::QueryEditor
                }
            }
            ActivePanel::Tables => ActivePanel::Connections,
            ActivePanel::QueryEditor => ActivePanel::Tables,
            ActivePanel::Results => ActivePanel::QueryEditor,
        };
    }

    pub fn select_next(&mut self) {
        match self.active_panel {
            ActivePanel::Connections => self.move_connection_cursor(true),
            ActivePanel::Tables => {
                self.navigate_tables(true);
            }
            ActivePanel::Results => {
                if let Some(ref result) = self.query_result {
                    if !result.rows.is_empty() {
                        self.selected_row = (self.selected_row + 1) % result.rows.len();
                        self.adjust_results_scroll();
                    }
                }
            }
            _ => {}
        }
    }

    pub fn select_prev(&mut self) {
        match self.active_panel {
            ActivePanel::Connections => self.move_connection_cursor(false),
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
                        self.adjust_results_scroll();
                    }
                }
            }
            _ => {}
        }
    }

    /// Keep results_scroll in sync with selected_row
    fn adjust_results_scroll(&mut self) {
        let vh = self.results_visible_height.get();
        if vh == 0 {
            return;
        }
        if self.selected_row < self.results_scroll {
            self.results_scroll = self.selected_row;
        } else if self.selected_row >= self.results_scroll + vh {
            self.results_scroll = self.selected_row - vh + 1;
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

    /// Calculate the visual index of the currently selected item in the filtered tables panel
    #[allow(dead_code)]
    fn get_selected_visual_index(&self) -> usize {
        let filtered = self.get_filtered_schemas();
        let mut visual_idx = 0;
        for (schema_idx, schema, filtered_tables) in &filtered {
            if *schema_idx == self.selected_schema && self.selected_table == 0 {
                return visual_idx;
            }
            visual_idx += 1; // schema header
            if schema.expanded {
                for (table_idx, _) in filtered_tables {
                    if *schema_idx == self.selected_schema && *table_idx + 1 == self.selected_table
                    {
                        return visual_idx;
                    }
                    visual_idx += 1;
                }
            }
        }
        visual_idx
    }

    /// Build a flat list of navigable items from filtered schemas.
    /// Each item is (schema_idx, table_original_idx) where table_original_idx is 0 for schema headers
    /// and original_table_index + 1 for tables.
    pub(crate) fn build_filtered_nav_items(&self) -> Vec<(usize, usize)> {
        let filtered = self.get_filtered_schemas();
        let mut items = Vec::new();
        for (schema_idx, schema, filtered_tables) in &filtered {
            items.push((*schema_idx, 0)); // schema header
            if schema.expanded {
                for (table_idx, _) in filtered_tables {
                    items.push((*schema_idx, *table_idx + 1));
                }
            }
        }
        items
    }

    /// Navigate through tables panel (schemas and tables), respecting the current filter
    fn navigate_tables(&mut self, forward: bool) {
        let nav_items = self.build_filtered_nav_items();
        if nav_items.is_empty() {
            return;
        }

        // Find current position in the nav items
        let current_pos = nav_items
            .iter()
            .position(|(s, t)| *s == self.selected_schema && *t == self.selected_table);

        let new_pos = if let Some(pos) = current_pos {
            if forward {
                (pos + 1) % nav_items.len()
            } else {
                pos.checked_sub(1).unwrap_or(nav_items.len() - 1)
            }
        } else {
            // Current selection not in filtered list, jump to first item
            0
        };

        let (new_schema, new_table) = nav_items[new_pos];
        self.selected_schema = new_schema;
        self.selected_table = new_table;

        // Update scroll to keep selection visible
        let visual_idx = new_pos;
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

    /// The selected table of the tables panel, unquoted.
    pub fn selected_table_ref(&self) -> Option<TableRef> {
        let schema = self.schemas.get(self.selected_schema)?;
        let table = schema.tables.get(self.selected_table.checked_sub(1)?)?;
        schema.expanded.then(|| TableRef {
            schema: Some(schema.name.clone()),
            name: table.clone(),
            alias: None,
        })
    }

    /// Get quote characters for the current database type
    pub fn get_quote_chars(&self) -> (char, char) {
        self.current_connection_config
            .as_ref()
            .map(|c| crate::engine::sql::statements::quote_chars(&c.db_type))
            .unwrap_or(('"', '"')) // Default to double quotes
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
                let Some(table) = self.editable_result_table() else {
                    self.set_status(READ_ONLY_RESULT);
                    return;
                };
                self.editing_row = Some(row.iter().map(|c| cell_to_edit_text(c)).collect());
                self.original_editing_row = Some(row.clone());
                self.editing_table_name = Some(table);
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
            self.system_columns = crate::engine::ops::rows::detect_system_columns(&result.columns);

            let Some(table) = self.result_sql.as_deref().and_then(single_table_source) else {
                self.set_status("Cannot add a row: the result does not read a single table");
                return;
            };
            self.editing_row = Some(empty_row);
            self.original_editing_row = None; // No original for new rows
            self.editing_table_name = Some(table);

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

    /// Table whose rows the current result shows, when they can be updated
    /// or deleted (`ops::query::editable_table`: a single table with its
    /// whole primary key in the result).
    fn editable_result_table(&self) -> Option<String> {
        let sql = self.result_sql.as_deref()?;
        crate::engine::ops::query::editable_table(sql, self.query_result.as_ref()?)
    }

    /// Default table name for export/import: the table the last result (or
    /// the editor's query) reads from.
    fn extract_table_from_query(&self) -> Option<String> {
        let sql = self.result_sql.as_deref().unwrap_or(self.query_input());
        crate::engine::sql::statements::extract_table_from_query(sql)
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

    // ========== Text Selection Methods ==========

    /// Check if there's an active selection
    pub fn has_selection(&self) -> bool {
        self.selection_start.is_some() && self.selection_end.is_some()
    }

    /// Get the selection range (start, end) normalized so start <= end
    pub fn get_selection_range(&self) -> Option<(usize, usize)> {
        match (self.selection_start, self.selection_end) {
            (Some(start), Some(end)) => {
                if start <= end {
                    Some((start, end))
                } else {
                    Some((end, start))
                }
            }
            _ => None,
        }
    }

    /// Get the selected text
    pub fn get_selected_text(&self) -> Option<String> {
        if let Some((start, end)) = self.get_selection_range() {
            let query = self.query_input();
            if end <= query.len() {
                return Some(query[start..end].to_string());
            }
        }
        None
    }

    /// Start a new selection at the current cursor position
    pub fn start_selection(&mut self) {
        let cursor = self.cursor_position();
        self.selection_start = Some(cursor);
        self.selection_end = Some(cursor);
    }

    /// Extend the selection to the current cursor position
    pub fn extend_selection(&mut self, new_pos: usize) {
        if self.selection_start.is_some() {
            self.selection_end = Some(new_pos);
        }
    }

    /// Clear the selection
    pub fn clear_selection(&mut self) {
        self.selection_start = None;
        self.selection_end = None;
    }

    /// Delete the selected text and return it
    pub fn delete_selection(&mut self) -> Option<String> {
        if let Some((start, end)) = self.get_selection_range() {
            let selected = self.get_selected_text();
            let query = self.query_input().to_string();
            if end <= query.len() {
                let new_query = format!("{}{}", &query[..start], &query[end..]);
                self.set_query(new_query);
                self.set_cursor_position(start);
            }
            self.clear_selection();
            return selected;
        }
        None
    }

    /// Select all text in the query editor
    pub fn select_all(&mut self) {
        self.selection_start = Some(0);
        self.selection_end = Some(self.query_input().len());
    }

    // ========== Filter Methods ==========

    /// Get filtered tables based on current filter, the hidden schemas left
    /// out unless `show_hidden_schemas`.
    pub fn get_filtered_schemas(&self) -> Vec<(usize, &SchemaInfo, Vec<(usize, &String)>)> {
        let filter = self.tables_filter.to_lowercase();
        let shown =
            |schema: &&SchemaInfo| self.show_hidden_schemas || !self.is_schema_hidden(&schema.name);

        if filter.is_empty() {
            // No filter, return all schemas with all tables
            return self
                .schemas
                .iter()
                .enumerate()
                .filter(|(_, schema)| shown(schema))
                .map(|(idx, schema)| {
                    let tables: Vec<(usize, &String)> = schema.tables.iter().enumerate().collect();
                    (idx, schema, tables)
                })
                .collect();
        }

        // Filter schemas and tables
        self.schemas
            .iter()
            .enumerate()
            .filter(|(_, schema)| shown(schema))
            .filter_map(|(schema_idx, schema)| {
                // Check if schema name matches
                let schema_matches = schema.name.to_lowercase().contains(&filter);

                // Filter tables
                let filtered_tables: Vec<(usize, &String)> = schema
                    .tables
                    .iter()
                    .enumerate()
                    .filter(|(_, table)| table.to_lowercase().contains(&filter))
                    .collect();

                if schema_matches || !filtered_tables.is_empty() {
                    Some((schema_idx, schema, filtered_tables))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get filtered results based on current filter
    pub fn get_filtered_results(&self) -> Option<Vec<(usize, &Vec<String>)>> {
        let result = self.query_result.as_ref()?;
        let filter = self.results_filter.to_lowercase();

        if filter.is_empty() {
            // No filter, return all rows
            return Some(result.rows.iter().enumerate().collect());
        }

        // Filter rows that contain the search term in any column
        Some(
            result
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.iter().any(|cell| cell.to_lowercase().contains(&filter)))
                .collect(),
        )
    }

    /// Check if results panel should be visible
    pub fn should_show_results(&self) -> bool {
        self.query_result.is_some() || self.connection_error.is_some() || self.is_connecting
    }

    // ========== Panel Resize Methods ==========

    /// Adjust sidebar width
    pub fn adjust_sidebar_width(&mut self, delta: i16) {
        let new_width = (self.sidebar_width as i16 + delta).clamp(15, 50) as u16;
        self.sidebar_width = new_width;
    }

    /// Adjust query editor height
    pub fn adjust_query_editor_height(&mut self, delta: i16) {
        let new_height = (self.query_editor_height as i16 + delta).clamp(20, 80) as u16;
        self.query_editor_height = new_height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_form_cycles_presets_and_ssl_and_keeps_them() {
        let mut nc = NewConnectionState::default();
        nc.cycle_flavor(); // Aucun -> MariaDB
        assert_eq!(nc.flavor, Some(Flavor::MariaDb));
        assert_eq!(nc.db_type, DatabaseType::MySQL);
        assert_eq!(nc.port, "3306");
        nc.cycle_ssl_mode();
        let c = nc.to_config();
        assert_eq!(c.flavor, Some(Flavor::MariaDb));
        assert_eq!(c.ssl_mode, Some(SslMode::Require)); // Prefer -> Require
    }

    #[test]
    fn cycling_past_the_last_flavor_returns_to_none() {
        let mut nc = NewConnectionState::default();
        for _ in 0..Flavor::ALL.len() {
            nc.cycle_flavor();
        }
        assert_eq!(nc.flavor, Flavor::ALL.last().copied());
        nc.cycle_flavor();
        assert_eq!(nc.flavor, None);
    }

    #[test]
    fn cycling_presets_keeps_a_typed_host() {
        let mut nc = NewConnectionState {
            host: "db.mine.example".into(),
            ..Default::default()
        };
        for _ in 0..=Flavor::ALL.len() {
            nc.cycle_flavor();
            assert_eq!(nc.host, "db.mine.example", "{:?}", nc.flavor);
        }
        // A default host still follows the driver.
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::SQLite);
        nc.cycle_flavor(); // MariaDB
        assert_eq!(nc.host, "localhost");
    }

    #[test]
    fn tui_url_field_fills_the_form() {
        let mut nc = NewConnectionState {
            url: "mysql://u:p%23w@aws.connect.psdb.cloud/app?ssl-mode=VERIFY_IDENTITY".into(),
            ..Default::default()
        };
        nc.apply_url().unwrap();
        assert_eq!(nc.password, "p#w");
        assert_eq!(nc.flavor, Some(Flavor::PlanetScale));
        assert_eq!(nc.ssl_mode, SslMode::VerifyFull);
        assert_eq!(nc.port, "3306");
        assert!(nc.url.is_empty());

        nc.url = "ftp://x".into();
        assert!(nc.apply_url().is_err());
        assert_eq!(nc.host, "aws.connect.psdb.cloud"); // untouched
    }

    #[test]
    fn pasted_url_resets_ssl_it_does_not_mention() {
        let mut nc = NewConnectionState {
            ssl_mode: SslMode::Require,
            ssl_ca: "/etc/ca.pem".into(),
            url: "postgres://u:p@localhost/db".into(),
            ..Default::default()
        };
        nc.apply_url().unwrap();
        assert_eq!(nc.ssl_mode, SslMode::Prefer);
        assert!(nc.ssl_ca.is_empty());
    }

    #[test]
    fn ssl_fields_are_skipped_for_other_engines() {
        let t = DatabaseType::SQLServer;
        let a = AzureAuthMethod::Credentials;
        assert_ne!(
            ConnectionField::Database.next_for(&t, &a, DbAuthMethod::Password),
            ConnectionField::SslMode
        );
        assert_eq!(
            ConnectionField::Database.next_for(&DatabaseType::Postgres, &a, DbAuthMethod::Password),
            ConnectionField::SslMode
        );
    }

    #[test]
    fn azure_fields_follow_the_auth_method() {
        let t = DatabaseType::Azure;
        let next = |f: ConnectionField, a| f.next_for(&t, &a, DbAuthMethod::Password);
        // Managed identity: client ID (username), no password.
        let mi = AzureAuthMethod::ManagedIdentity;
        assert_eq!(
            next(ConnectionField::Port, mi.clone()),
            ConnectionField::Username
        );
        assert_eq!(
            next(ConnectionField::Username, mi),
            ConnectionField::Database
        );
        // Azure AD (CLI): neither.
        let cli = AzureAuthMethod::Interactive;
        assert_eq!(next(ConnectionField::Port, cli), ConnectionField::Database);
        // SQL auth: both.
        let sql = AzureAuthMethod::Credentials;
        assert_eq!(
            next(ConnectionField::Username, sql),
            ConnectionField::Password
        );
        // Other engines are untouched.
        assert_eq!(
            ConnectionField::Port.next_for(
                &DatabaseType::SQLServer,
                &AzureAuthMethod::Interactive,
                DbAuthMethod::Password
            ),
            ConnectionField::Username
        );
    }

    #[test]
    fn managed_identity_client_id_round_trips_and_is_checked() {
        let id = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        let conn = ConnectionConfig {
            db_type: DatabaseType::Azure,
            host: Some("srv.database.chinacloudapi.cn".into()),
            username: Some(id.into()),
            azure_auth_method: Some(AzureAuthMethod::ManagedIdentity),
            ..Default::default()
        };
        let mut nc = NewConnectionState::from_config(&conn);
        assert!(nc.check().is_ok());
        let c = nc.to_config();
        assert_eq!(c.username.as_deref(), Some(id));
        assert_eq!(c.azure_auth_method, Some(AzureAuthMethod::ManagedIdentity));
        assert_eq!(c.host, conn.host);

        nc.username = "sa".into();
        assert!(nc.check().unwrap_err().contains("n'est pas un ID client"));
        nc.username.clear();
        assert!(nc.check().is_ok());
        nc.username = "sa".into();
        nc.azure_auth_method = AzureAuthMethod::Credentials;
        assert!(nc.check().is_ok());
    }

    #[test]
    fn editing_keeps_ssl_flavor_and_colour() {
        let conn = ConnectionConfig {
            ssl_mode: Some(SslMode::VerifyCa),
            ssl_ca: Some("/etc/ca.pem".into()),
            flavor: Some(Flavor::Neon),
            color: Some([1, 2, 3]),
            ..Default::default()
        };
        let c = NewConnectionState::from_config(&conn).to_config();
        assert_eq!(c.ssl_mode, Some(SslMode::VerifyCa));
        assert_eq!(c.ssl_ca, Some("/etc/ca.pem".into()));
        assert_eq!(c.flavor, Some(Flavor::Neon));
        assert_eq!(c.color, Some([1, 2, 3]));
    }

    #[test]
    fn editing_keeps_the_interactive_browser_target() {
        let conn = ConnectionConfig {
            db_type: DatabaseType::Azure,
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: Some("contoso.com".into()),
            azure_client_id: Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into()),
            username: Some("jane@contoso.com".into()),
            ..Default::default()
        };
        let c = NewConnectionState::from_config(&conn).to_config();
        assert_eq!(c.azure_auth_method, conn.azure_auth_method);
        assert_eq!(c.tenant_id, conn.tenant_id);
        assert_eq!(c.azure_client_id, conn.azure_client_id);
        assert_eq!(c.username, conn.username);
        // Only that method keeps the client ID.
        let mut nc = NewConnectionState::from_config(&conn);
        nc.azure_auth_method = AzureAuthMethod::Interactive;
        assert_eq!(nc.to_config().azure_client_id, None);
    }

    #[test]
    fn editing_an_interactive_browser_connection_without_settings_keeps_none() {
        let conn = ConnectionConfig {
            db_type: DatabaseType::Azure,
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            ..Default::default()
        };
        let nc = NewConnectionState::from_config(&conn);
        // Auto, not Azure CLI's `common` (refused for this method).
        assert_eq!(
            (nc.tenant_id.as_str(), nc.azure_client_id.as_str()),
            ("", "")
        );
        assert!(nc.check().is_ok());
        let c = nc.to_config();
        assert_eq!((c.tenant_id, c.azure_client_id), (None, None));
        // Azure CLI keeps its default.
        let cli = ConnectionConfig {
            azure_auth_method: Some(AzureAuthMethod::Interactive),
            ..conn
        };
        assert_eq!(NewConnectionState::from_config(&cli).tenant_id, "common");
    }

    #[test]
    fn interactive_browser_asks_for_tenant_app_and_account() {
        let t = DatabaseType::Azure;
        let browser = AzureAuthMethod::InteractiveBrowser;
        let next = |f: ConnectionField| f.next_for(&t, &browser, DbAuthMethod::Password);
        assert_eq!(next(ConnectionField::AzureAuth), ConnectionField::TenantId);
        assert_eq!(next(ConnectionField::TenantId), ConnectionField::ClientId);
        assert_eq!(next(ConnectionField::ClientId), ConnectionField::Host);
        // The account (username), no password.
        assert_eq!(next(ConnectionField::Port), ConnectionField::Username);
        assert_eq!(next(ConnectionField::Username), ConnectionField::Database);
        assert_eq!(
            ConnectionField::Host.prev_for(&t, &browser, DbAuthMethod::Password),
            ConnectionField::ClientId
        );
        // Azure CLI: the tenant only.
        let cli = AzureAuthMethod::Interactive;
        assert_eq!(
            ConnectionField::TenantId.next_for(&t, &cli, DbAuthMethod::Password),
            ConnectionField::Host
        );
        assert_eq!(
            ConnectionField::Host.prev_for(&t, &cli, DbAuthMethod::Password),
            ConnectionField::TenantId
        );
        // Other engines never stop there.
        assert_eq!(
            ConnectionField::DbType.next_for(
                &DatabaseType::SQLServer,
                &browser,
                DbAuthMethod::Password
            ),
            ConnectionField::Host
        );
    }

    #[test]
    fn every_azure_method_can_be_chosen() {
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::Azure);
        let mut seen = Vec::new();
        for _ in 0..4 {
            nc.cycle_azure_auth_method();
            seen.push(nc.azure_auth_method.clone());
        }
        assert_eq!(
            seen,
            [
                AzureAuthMethod::Interactive,
                AzureAuthMethod::InteractiveBrowser,
                AzureAuthMethod::ManagedIdentity,
                AzureAuthMethod::Credentials,
            ]
        );
    }

    #[test]
    fn the_browser_method_does_not_keep_the_common_tenant() {
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::Azure);
        assert_eq!(nc.tenant_id, "common");
        nc.azure_auth_method = AzureAuthMethod::Interactive;
        nc.cycle_azure_auth_method();
        assert_eq!(nc.azure_auth_method, AzureAuthMethod::InteractiveBrowser);
        assert_eq!(nc.tenant_id, "", "refused for this method");
        assert!(nc.check().is_ok(), "empty: the server's tenant");
        // A tenant of one's own is kept, `organizations` included.
        for tenant in ["contoso.com", "organizations"] {
            nc.tenant_id = tenant.into();
            nc.azure_auth_method = AzureAuthMethod::Interactive;
            nc.cycle_azure_auth_method();
            assert_eq!(nc.tenant_id, tenant);
            assert!(nc.check().is_ok(), "{tenant}");
        }
    }

    #[test]
    fn interactive_browser_is_checked_and_its_client_id_typed() {
        let client = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::Azure);
        nc.azure_auth_method = AzureAuthMethod::InteractiveBrowser;
        assert!(nc.check().unwrap_err().contains("pas « common »"));
        // Both optional: the server's tenant, Microsoft's application.
        nc.tenant_id = " ".into();
        assert!(nc.check().is_ok());
        assert_eq!(nc.to_config().tenant_id, None);
        nc.tenant_id = "organizations".into();
        assert!(nc.check().is_ok());
        nc.tenant_id = " contoso.com ".into();
        assert_eq!(nc.to_config().tenant_id.as_deref(), Some("contoso.com"));
        // The application is optional (Microsoft's), a typed one a GUID.
        assert!(nc.check().is_ok());
        nc.azure_client_id = "my-app".into();
        assert_eq!(
            nc.check().unwrap_err(),
            "L'ID client de l'application doit être un GUID"
        );
        nc.azure_client_id.clear();
        nc.active_field = ConnectionField::ClientId;
        nc.get_active_field_mut().unwrap().push_str(client);
        assert_eq!(nc.get_active_field_value(), client);
        assert!(nc.check().is_ok());
        assert_eq!(nc.to_config().azure_client_id.as_deref(), Some(client));
        // Other methods don't check it.
        nc.azure_client_id.clear();
        nc.azure_auth_method = AzureAuthMethod::Interactive;
        assert!(nc.check().is_ok());
    }

    #[test]
    fn gcp_iam_fields_follow_the_db_auth() {
        let a = AzureAuthMethod::Credentials;
        let iam = DbAuthMethod::GcpIam;
        for t in [DatabaseType::Postgres, DatabaseType::MySQL] {
            let pw = DbAuthMethod::Password;
            // The choice comes after the port, before the user.
            assert_eq!(
                ConnectionField::Port.next_for(&t, &a, pw),
                ConnectionField::DbAuth
            );
            assert_eq!(
                ConnectionField::DbAuth.next_for(&t, &a, pw),
                ConnectionField::Username
            );
            assert_eq!(
                ConnectionField::Username.next_for(&t, &a, pw),
                ConnectionField::Password
            );
            // IAM: the Google account (username), no password.
            assert_eq!(
                ConnectionField::Username.next_for(&t, &a, iam),
                ConnectionField::Database
            );
            assert_eq!(
                ConnectionField::Database.prev_for(&t, &a, iam),
                ConnectionField::Username
            );
        }
        // Other engines never stop there, whatever the form holds.
        for t in [DatabaseType::SQLServer, DatabaseType::SQLite] {
            assert_eq!(
                ConnectionField::Port.next_for(&t, &a, iam),
                ConnectionField::Username
            );
        }
        assert_eq!(
            ConnectionField::Username.next_for(&DatabaseType::SQLServer, &a, iam),
            ConnectionField::Password
        );
    }

    #[test]
    fn gcp_iam_needs_an_account_and_keeps_no_password() {
        let mut nc = NewConnectionState {
            password: "old secret".into(),
            ..Default::default()
        };
        nc.cycle_db_auth();
        assert_eq!(nc.db_auth, DbAuthMethod::GcpIam);
        // The driver's default user is no Google account.
        assert_eq!(nc.username, "");
        assert_eq!(nc.check().unwrap_err(), "Indique ton compte Google");
        nc.username = "jane@example.com".into();
        assert!(nc.check().is_ok());
        let c = nc.to_config();
        assert_eq!(c.db_auth, Some(DbAuthMethod::GcpIam));
        assert_eq!(c.username.as_deref(), Some("jane@example.com"));
        assert_eq!(c.password, None);

        // Editing keeps it.
        let back = NewConnectionState::from_config(&c);
        assert_eq!(back.db_auth, DbAuthMethod::GcpIam);
        assert_eq!(back.to_config(), c);

        // The password again: nothing checked, the default method left out.
        nc.cycle_db_auth();
        assert_eq!(nc.db_auth, DbAuthMethod::Password);
        assert_eq!(nc.username, "jane@example.com", "a typed user stays");
        nc.username.clear();
        assert!(nc.check().is_ok());
        let c = nc.to_config();
        assert_eq!(c.db_auth, None);
        assert_eq!(c.password.as_deref(), Some("old secret"));
    }

    #[test]
    fn gcp_iam_is_kept_for_postgres_and_mysql_only() {
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::MySQL);
        nc.username = "root".into();
        nc.cycle_db_auth();
        assert_eq!(nc.username, "");
        nc.username = "jane@example.com".into();
        assert_eq!(nc.to_config().db_auth, Some(DbAuthMethod::GcpIam));
        nc.set_db_type(DatabaseType::SQLServer);
        nc.username.clear();
        assert!(nc.check().is_ok());
        assert_eq!(nc.to_config().db_auth, None);
    }

    #[test]
    fn the_type_cycles_through_mongodb_and_redis_both_ways() {
        let mut nc = NewConnectionState::default();
        let mut seen = Vec::new();
        for _ in 0..7 {
            nc.cycle_db_type();
            seen.push(nc.db_type.clone());
        }
        use DatabaseType::*;
        assert_eq!(
            seen,
            [MySQL, SQLite, SQLServer, Azure, MongoDB, Redis, Postgres]
        );
        nc.cycle_db_type_back();
        assert_eq!(nc.db_type, Redis);
        assert_eq!((nc.port.as_str(), nc.database.as_str()), ("6379", "0"));
        assert_eq!(nc.username, "", "no driver default user");
        nc.cycle_db_type_back();
        assert_eq!(nc.db_type, MongoDB);
        assert_eq!(nc.port, "27017");
    }

    #[test]
    fn nosql_forms_ask_for_their_own_fields() {
        let a = AzureAuthMethod::Credentials;
        let pw = DbAuthMethod::Password;
        let chain = |t: &DatabaseType| {
            let mut fields = vec![ConnectionField::Name];
            loop {
                let next = fields.last().unwrap().next_for(t, &a, pw);
                if next == ConnectionField::Name {
                    break fields;
                }
                fields.push(next);
            }
        };
        use ConnectionField::*;
        assert_eq!(
            chain(&DatabaseType::MongoDB),
            [
                Name, Url, DbType, Host, Port, Username, Password, AuthSource, Database, SslMode,
                SslCa
            ]
        );
        assert_eq!(
            chain(&DatabaseType::Redis),
            [Name, Url, DbType, Host, Port, Username, Password, Database, SslMode, SslCa]
        );
        // SQL engines never stop on the authentication database.
        assert!(!chain(&DatabaseType::Postgres).contains(&AuthSource));
        assert_eq!(
            ConnectionField::Database.prev_for(&DatabaseType::MongoDB, &a, pw),
            AuthSource
        );
    }

    #[test]
    fn a_mongodb_uri_fills_the_form_and_keeps_no_password() {
        let mut nc = NewConnectionState {
            url: "mongodb+srv://ann:p%40ss@cluster0.example.net/shop?retryWrites=true".into(),
            ..Default::default()
        };
        nc.apply_url().unwrap();
        assert_eq!(nc.db_type, DatabaseType::MongoDB);
        assert_eq!(nc.password, "p@ss");
        assert_eq!(nc.username, "ann");
        assert!(!nc.url.contains("p%40ss"), "{}", nc.url);
        assert!(
            nc.url.starts_with("mongodb+srv://ann@cluster0"),
            "{}",
            nc.url
        );
        nc.auth_source = " admin ".into();
        assert!(nc.check().is_ok());
        let c = nc.to_config();
        assert_eq!(c.uri.as_deref(), Some(nc.url.as_str()));
        assert_eq!(c.auth_source.as_deref(), Some("admin"));
        assert_eq!(c.password.as_deref(), Some("p@ss"));
        // Editing shows the URI again, unchanged once saved.
        let back = NewConnectionState::from_config(&c);
        assert_eq!(back.url, nc.url);
        assert_eq!(back.auth_source, "admin");
        assert_eq!(back.to_config(), c);
    }

    #[test]
    fn a_mongodb_uri_never_sends_the_username_field() {
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::MongoDB);
        nc.username = "stale".into();
        nc.url = "mongodb://db1:27017/".into();
        assert!(nc.mongo_uri());
        assert_eq!(nc.to_config().username, None);
        // Without a URI, the field is the user.
        nc.url.clear();
        nc.host = "db1".into();
        assert_eq!(nc.to_config().username.as_deref(), Some("stale"));
        // A saved connection with a stale username: dropped, then stable.
        let saved = ConnectionConfig {
            name: "m".into(),
            db_type: DatabaseType::MongoDB,
            uri: Some("mongodb://app@db1:27017/".into()),
            username: Some("postgres".into()),
            password: Some("pw".into()),
            database: String::new(),
            ..Default::default()
        };
        let c = NewConnectionState::from_config(&saved).to_config();
        assert_eq!(c.username, None);
        assert_eq!((&c.uri, &c.password), (&saved.uri, &saved.password));
        assert_eq!(NewConnectionState::from_config(&c).to_config(), c);
    }

    #[test]
    fn a_typed_uri_password_moves_to_the_password_field() {
        let mut nc = NewConnectionState::default();
        nc.set_db_type(DatabaseType::MongoDB);
        nc.url = "mongodb://ann:secret@db1:27017,db2:27017/?replicaSet=rs0".into();
        let c = nc.config_moving_password();
        assert_eq!(c.password.as_deref(), Some("secret"));
        assert_eq!(
            c.uri.as_deref(),
            Some("mongodb://ann@db1:27017,db2:27017/?replicaSet=rs0")
        );
        // Not a MongoDB scheme: refused.
        nc.url = "redis://x".into();
        assert!(nc.check().unwrap_err().contains("mongodb://"));
        // Neither URI nor host.
        nc.url.clear();
        nc.host.clear();
        assert!(nc.check().is_err());
    }

    #[test]
    fn redis_keeps_its_database_and_tls() {
        let mut nc = NewConnectionState {
            url: "rediss://default:pw@cache.example.com:6380/3".into(),
            ..Default::default()
        };
        nc.apply_url().unwrap();
        assert_eq!(nc.db_type, DatabaseType::Redis);
        assert_eq!(nc.database, "3");
        assert_eq!(nc.ssl_mode, SslMode::VerifyFull);
        assert!(nc.url.is_empty(), "no URI kept for Redis");
        let c = nc.to_config();
        assert_eq!(c.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!((&c.uri, &c.auth_source), (&None, &None));
        assert_eq!(
            NewConnectionState::from_config(&c).to_config().database,
            "3"
        );
        nc.database = "16".into();
        assert!(nc.check().unwrap_err().contains("0 à 15"));
    }

    #[test]
    fn changing_driver_drops_the_flavor_and_ssl() {
        let mut nc = NewConnectionState::default();
        nc.cycle_flavor(); // MariaDB (MySQL)
        nc.cycle_db_type(); // SQLite
        let c = nc.to_config();
        assert_eq!((c.flavor, c.ssl_mode, c.ssl_ca), (None, None, None));
    }
}
