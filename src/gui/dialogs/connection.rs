//! New / edit connection form.

use egui::{Key, Modifiers, Sense, Stroke, StrokeKind};
use egui_phosphor::regular as icon;

use crate::engine::config::folders;
use crate::engine::db::azure::{self, AzureCloud};
use crate::engine::db::{entra_interactive, gcp_iam};
use crate::engine::models::{
    AzureAuthMethod, ConnectionConfig, DatabaseType, DbAuthMethod, Flavor, SslMode,
};
use crate::engine::presets;
use crate::gui::app::App;
use crate::gui::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use crate::gui::popup;
use crate::gui::sign_in;
use crate::gui::theme::{self, CONNECTION_COLORS, ERROR};

const DB_TYPES: [DatabaseType; 7] = [
    DatabaseType::Postgres,
    DatabaseType::MySQL,
    DatabaseType::SQLite,
    DatabaseType::SQLServer,
    DatabaseType::Azure,
    DatabaseType::MongoDB,
    DatabaseType::Redis,
];

/// Databases a new MongoDB connection hides (the server's own).
pub const MONGO_HIDDEN: [&str; 2] = ["config", "local"];

/// Help of the MongoDB form for the compatible services.
const MONGO_SERVICES_HELP: &str = "Cosmos DB (API MongoDB) et DocumentDB : collez la chaîne de \
     connexion fournie (DocumentDB : CA global-bundle.pem)";

/// TLS of a MongoDB / Redis connection, as the form offers it (stored in
/// `ssl_mode`, see `ConnectionConfig::nosql_tls`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoSqlTls {
    Off,
    On,
    /// Encrypted, certificate not verified.
    Insecure,
}

impl NoSqlTls {
    pub const ALL: [NoSqlTls; 3] = [NoSqlTls::Off, NoSqlTls::On, NoSqlTls::Insecure];

    pub fn from_mode(mode: SslMode) -> Self {
        match mode {
            SslMode::Disable | SslMode::Prefer => Self::Off,
            SslMode::Require => Self::Insecure,
            SslMode::VerifyCa | SslMode::VerifyFull => Self::On,
        }
    }

    pub fn mode(self) -> SslMode {
        match self {
            Self::Off => SslMode::Disable,
            Self::On => SslMode::VerifyFull,
            Self::Insecure => SslMode::Require,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Désactivé",
            Self::On => "Activé",
            Self::Insecure => "Activé sans vérifier le certificat",
        }
    }
}

/// Whether `uri` is a MongoDB connection string.
fn is_mongo_uri(uri: &str) -> bool {
    let lower = uri.trim().to_ascii_lowercase();
    lower.starts_with("mongodb://") || lower.starts_with("mongodb+srv://")
}

const AZURE_METHODS: [AzureAuthMethod; 4] = [
    AzureAuthMethod::Credentials,
    AzureAuthMethod::Interactive,
    AzureAuthMethod::InteractiveBrowser,
    AzureAuthMethod::ManagedIdentity,
];

const DB_AUTH_METHODS: [DbAuthMethod; 2] = [DbAuthMethod::Password, DbAuthMethod::GcpIam];

/// Shown by the SSL mode when Google Cloud IAM raises it.
const IAM_SSL_NOTE: &str = "IAM : au moins Obligatoire";

/// The username field of an Azure managed identity connection.
const MANAGED_IDENTITY_LABEL: &str = "ID client de l'identité (vide = identité système)";

/// The README section on the interactive browser sign-in (its advanced
/// options: the organization's own tenant or application).
const ENTRA_APP_DOC: &str = "https://github.com/Ajustor/storingUnicorns#azure-ad-interactif";

/// A field of the authentication part of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthField {
    /// `tenant_id`; optional for the browser sign-in (asked from the
    /// server).
    Tenant,
    /// `azure_client_id`: the organization's Entra ID application, optional
    /// (Microsoft's drivers' by default).
    AppClientId,
    /// `username` as the Entra ID account (login hint), optional.
    Account,
    /// `username` as the client ID of a user-assigned managed identity.
    IdentityClientId,
    /// `username` as the Google account of a Cloud SQL IAM sign-in.
    GoogleAccount,
    User,
    Password,
}

/// Whether `db_type` offers the « Authentification » choice of
/// `DbAuthMethod` (Cloud SQL's engines).
fn offers_db_auth(db_type: &DatabaseType) -> bool {
    matches!(db_type, DatabaseType::Postgres | DatabaseType::MySQL)
}

/// The authentication fields of `db_type` (and of Azure `method`, or of
/// PostgreSQL / MySQL `db_auth`), in order.
fn auth_fields(
    db_type: &DatabaseType,
    method: &AzureAuthMethod,
    db_auth: DbAuthMethod,
) -> &'static [AuthField] {
    use AuthField::*;
    if offers_db_auth(db_type) && db_auth == DbAuthMethod::GcpIam {
        return &[GoogleAccount];
    }
    if *db_type != DatabaseType::Azure {
        return &[User, Password];
    }
    match method {
        AzureAuthMethod::Credentials => &[User, Password],
        AzureAuthMethod::Interactive => &[Tenant],
        AzureAuthMethod::InteractiveBrowser => &[Account],
        AzureAuthMethod::ManagedIdentity => &[IdentityClientId],
    }
}

/// The authentication fields folded under « Options avancées », in order:
/// the browser sign-in's tenant and application, which it finds by itself
/// (the server's tenant, Microsoft's drivers' application).
fn advanced_fields(db_type: &DatabaseType, method: &AzureAuthMethod) -> &'static [AuthField] {
    if *db_type == DatabaseType::Azure && *method == AzureAuthMethod::InteractiveBrowser {
        &[AuthField::Tenant, AuthField::AppClientId]
    } else {
        &[]
    }
}

/// How « Options avancées » is made to show this frame: `Some(open)` sets
/// it, `None` leaves it as the user folded it. `shown` is `None` until it
/// first shows in the form, then whether it showed on the previous frame;
/// `filled` whether a tenant or an application is typed in; `valid` whether
/// they pass `validate_interactive`.
fn advanced_forced(shown: Option<bool>, filled: bool, valid: bool) -> Option<bool> {
    match shown {
        // The error under the form names one of its fields.
        _ if !valid => Some(true),
        // Its first showing: egui still holds the previous form's state.
        None => Some(filled),
        // The browser sign-in chosen again, with a tenant or an application
        // typed in meanwhile (for Azure CLI, say): they are in use.
        Some(false) if filled => Some(true),
        _ => None,
    }
}

/// The database user Cloud SQL expects for the Google `account`, once one
/// is typed.
fn iam_user_hint(db_type: &DatabaseType, account: &str) -> Option<String> {
    (!account.trim().is_empty()).then(|| {
        format!(
            "utilisateur base : {}",
            gcp_iam::database_user(db_type, account)
        )
    })
}

/// The note by the SSL mode when the IAM sign-in raises it to `Require`.
fn iam_ssl_note(db_auth: DbAuthMethod, ssl_mode: SslMode) -> Option<&'static str> {
    (db_auth == DbAuthMethod::GcpIam && matches!(ssl_mode, SslMode::Disable | SslMode::Prefer))
        .then_some(IAM_SSL_NOTE)
}

fn default_port(t: &DatabaseType) -> &'static str {
    match t {
        DatabaseType::Postgres => "5432",
        DatabaseType::MySQL => "3306",
        DatabaseType::SQLite => "",
        DatabaseType::SQLServer | DatabaseType::Azure => "1433",
        DatabaseType::MongoDB => "27017",
        DatabaseType::Redis => "6379",
    }
}

/// Whether two configs reach the same database the same way (everything but
/// the name, the colour and the flavor is equal): an open session stays
/// valid.
pub fn same_target(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    a.db_type == b.db_type
        && a.host == b.host
        && a.port == b.port
        && a.username == b.username
        && a.password == b.password
        && a.database == b.database
        && a.azure_auth_method == b.azure_auth_method
        && a.tenant_id == b.tenant_id
        && a.azure_client_id == b.azure_client_id
        && a.ssl_mode == b.ssl_mode
        && a.ssl_ca == b.ssl_ca
        && a.effective_db_auth() == b.effective_db_auth()
        && a.uri == b.uri
        && a.auth_source == b.auth_source
}

pub struct ConnectionForm {
    /// Name of the edited connection; `None` when creating.
    pub original_name: Option<String>,
    pub name: String,
    pub db_type: DatabaseType,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub database: String,
    pub azure_auth: AzureAuthMethod,
    pub tenant_id: String,
    /// Entra ID application (client) ID of an interactive browser sign-in.
    pub azure_client_id: String,
    /// Whether « Options avancées » showed on the previous frame; `None`
    /// until it first shows (see `advanced_force`).
    advanced_shown: Option<bool>,
    pub color: Option<[u8; 3]>,
    /// Compatible product; `None` = the bare engine.
    pub flavor: Option<Flavor>,
    pub ssl_mode: SslMode,
    /// CA certificate path; empty = system roots only.
    pub ssl_ca: String,
    /// PostgreSQL / MySQL sign-in; kept while another engine is chosen.
    pub db_auth: DbAuthMethod,
    /// Pasted connection URL, consumed by "Remplir".
    pub url: String,
    pub url_error: Option<String>,
    pub error: Option<String>,
    pub testing: bool,
    pub test_result: Option<Result<(), String>>,
    /// The configs tested since the form opened, the running test's last.
    tested: Vec<ConnectionConfig>,
    /// Folder of the connection (`/`-separated path); empty = the root.
    pub folder: String,
    /// The existing folders, offered by « Dossier ».
    pub known_folders: Vec<String>,
    /// Schemas hidden from the explorer, Ctrl+N and completion.
    pub hidden_schemas: Vec<String>,
    /// The connection's schemas, when it is open (« Schémas affichés »).
    pub schemas: Option<Vec<String>>,
    /// MongoDB: connect with the connection string (`uri`) rather than
    /// host / port.
    pub mongo_uri_mode: bool,
    /// MongoDB connection string, never with its password (moved to
    /// `password` as soon as it is typed or pasted).
    pub uri: String,
    /// MongoDB authentication database; empty = the URI's, else `admin`.
    pub auth_source: String,
}

/// What the form asks for (closing it is the modal's `Cancel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormAction {
    Save,
    Test,
    /// Stop the running test (its browser sign-in).
    CancelTest,
}

/// Id of the « Coller une URL » field, where Enter means « Remplir ».
const URL_FIELD: &str = "conn_url";

impl ConnectionForm {
    /// `Some(config)` edits that connection; `None` creates a new one.
    pub fn new(edit: Option<ConnectionConfig>) -> Self {
        let editing = edit.is_some();
        let c = edit.unwrap_or_else(|| ConnectionConfig {
            name: String::new(),
            ..Default::default()
        });
        Self {
            original_name: editing.then(|| c.name.clone()),
            port: c.port.map(|p| p.to_string()).unwrap_or_default(),
            host: c.host.unwrap_or_default(),
            username: c.username.unwrap_or_default(),
            password: c.password.unwrap_or_default(),
            azure_auth: c.azure_auth_method.unwrap_or_default(),
            tenant_id: c.tenant_id.unwrap_or_default(),
            azure_client_id: c.azure_client_id.unwrap_or_default(),
            advanced_shown: None,
            name: c.name,
            db_type: c.db_type,
            database: c.database,
            color: c.color,
            flavor: c.flavor,
            ssl_mode: c.ssl_mode.unwrap_or_default(),
            ssl_ca: c
                .ssl_ca
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            db_auth: c.db_auth.unwrap_or_default(),
            url: String::new(),
            url_error: None,
            error: None,
            testing: false,
            test_result: None,
            tested: Vec::new(),
            folder: c.folder.unwrap_or_default(),
            known_folders: Vec::new(),
            hidden_schemas: c.hidden_schemas,
            schemas: None,
            mongo_uri_mode: c.uri.is_some(),
            uri: c.uri.unwrap_or_default(),
            auth_source: c.auth_source.unwrap_or_default(),
        }
    }

    /// A MongoDB / Redis connection.
    fn nosql(&self) -> bool {
        !self.db_type.is_sql()
    }

    /// Move the password of the typed / pasted URI to the password field:
    /// the URI saved never holds it.
    pub fn take_uri_password(&mut self) {
        if let (uri, Some(password)) = presets::split_uri_password(&self.uri) {
            self.uri = uri;
            self.password = password;
        }
    }

    /// The existing folders to choose from, and the connection's schemas
    /// when it is open.
    pub fn with_context(
        mut self,
        known_folders: Vec<String>,
        schemas: Option<Vec<String>>,
    ) -> Self {
        self.known_folders = known_folders;
        self.schemas = schemas;
        self
    }

    /// The folder typed in, normalized (`None`: the root).
    fn folder(&self) -> Result<Option<String>, String> {
        if self.folder.trim().is_empty() {
            Ok(None)
        } else {
            folders::normalize(&self.folder).map(Some)
        }
    }

    /// Show or hide `schema`.
    fn set_shown(&mut self, schema: &str, shown: bool) {
        self.hidden_schemas.retain(|s| s != schema);
        if !shown {
            self.hidden_schemas.push(schema.to_string());
        }
    }

    /// `config` (this form's, validated) is being tested.
    pub fn start_test(&mut self, config: ConnectionConfig) {
        self.error = None;
        self.testing = true;
        self.test_result = None;
        self.tested.push(config);
    }

    /// The tested configs whose sign-in none of the `remaining` (saved)
    /// connections uses, once: a test of a connection then not saved, or
    /// saved with another tenant / application / account, would otherwise
    /// leave its refresh token in the keyring, with nothing to forget it.
    pub fn orphaned_tests(&mut self, remaining: &[ConnectionConfig]) -> Vec<ConnectionConfig> {
        std::mem::take(&mut self.tested)
            .into_iter()
            .filter(|c| entra_interactive::orphaned(c, remaining))
            .collect()
    }

    /// Name of the connection the running test opens: its browser sign-in
    /// is reported under that name, whatever the name field says since.
    pub fn testing_name(&self) -> Option<&str> {
        self.tested
            .last()
            .filter(|_| self.testing)
            .map(|c| c.name.as_str())
    }

    pub fn set_db_type(&mut self, t: DatabaseType) {
        if self.port.is_empty() || self.port == default_port(&self.db_type) {
            self.port = default_port(&t).to_string();
        }
        if self.flavor.is_some_and(|f| f.driver() != t) {
            self.flavor = None;
        }
        // A new MongoDB connection hides the server's own databases.
        let pre_hidden = self
            .hidden_schemas
            .iter()
            .map(String::as_str)
            .eq(MONGO_HIDDEN);
        if self.original_name.is_none() {
            if t == DatabaseType::MongoDB && self.hidden_schemas.is_empty() {
                self.hidden_schemas = MONGO_HIDDEN.map(String::from).to_vec();
            } else if t != DatabaseType::MongoDB && pre_hidden {
                self.hidden_schemas.clear();
            }
        }
        // A database index for Redis, a default database (or none) for
        // MongoDB: what was typed for another engine rarely fits.
        let untouched = self.database.trim().is_empty()
            || self.database == ConnectionConfig::default().database;
        if t == DatabaseType::Redis && self.database.trim().parse::<u8>().is_err() {
            self.database = "0".into();
        } else if (t == DatabaseType::MongoDB && untouched)
            || (self.db_type == DatabaseType::Redis && t != DatabaseType::Redis)
        {
            self.database.clear();
        }
        self.db_type = t;
    }

    /// How « Options avancées » is made to show this frame, if it shows
    /// (see `advanced_forced`): nothing in use or wrong in it stays hidden.
    /// Called once a frame, as it records whether it shows.
    fn advanced_force(&mut self) -> Option<bool> {
        if advanced_fields(&self.db_type, &self.azure_auth).is_empty() {
            if self.advanced_shown.is_some() {
                self.advanced_shown = Some(false);
            }
            return None;
        }
        let filled = !self.tenant_id.trim().is_empty() || !self.azure_client_id.trim().is_empty();
        let valid =
            entra_interactive::validate_interactive(&self.tenant_id, &self.azure_client_id).is_ok();
        let forced = advanced_forced(self.advanced_shown, filled, valid);
        self.advanced_shown = Some(true);
        forced
    }

    fn uses_server(&self) -> bool {
        self.db_type != DatabaseType::SQLite
    }

    /// Whether the driver honours the SSL settings (PostgreSQL / MySQL,
    /// MongoDB / Redis through `nosql_tls`).
    fn uses_tls(&self) -> bool {
        matches!(
            self.db_type,
            DatabaseType::Postgres
                | DatabaseType::MySQL
                | DatabaseType::MongoDB
                | DatabaseType::Redis
        )
    }

    /// Whether the « Authentification » choice shows (PostgreSQL / MySQL).
    fn offers_db_auth(&self) -> bool {
        offers_db_auth(&self.db_type)
    }

    /// Whether the connection signs in with Google Cloud IAM.
    fn gcp_iam(&self) -> bool {
        self.offers_db_auth() && self.db_auth == DbAuthMethod::GcpIam
    }

    /// Choose how PostgreSQL / MySQL signs in; a driver's default user
    /// (no Google account) is cleared for the IAM sign-in.
    pub fn set_db_auth(&mut self, method: DbAuthMethod) {
        self.db_auth = method;
        let u = self.username.trim();
        if method == DbAuthMethod::GcpIam && (u == "postgres" || u == "root") {
            self.username.clear();
        }
    }

    /// Choose a product (None = bare engine): driver, port and SSL mode;
    /// the user only when empty or still a driver default; the host is
    /// never overwritten.
    pub fn apply_preset(&mut self, flavor: Option<Flavor>) {
        self.flavor = flavor;
        let Some(f) = flavor else { return };
        let p = presets::preset(f);
        self.set_db_type(p.db_type);
        self.port = p.port.to_string();
        self.ssl_mode = p.ssl_mode;
        let u = self.username.trim();
        if u.is_empty() || u == "postgres" || u == "root" {
            self.username = p.username.to_string();
        }
    }

    /// Fill the form from `self.url` (see the SSL / password rules below);
    /// on error nothing changes.
    pub fn apply_url(&mut self) -> Result<(), String> {
        let p = presets::parse_url(&self.url)?;
        self.set_db_type(p.db_type);
        self.flavor = p.flavor;
        // MongoDB: the whole string (hosts, options), without its password.
        if let Some(uri) = p.uri {
            self.uri = uri;
            self.mongo_uri_mode = true;
        }
        if let Some(h) = p.host {
            self.host = h;
        }
        self.port = p
            .port
            .map(|n| n.to_string())
            .unwrap_or_else(|| default_port(&self.db_type).to_string());
        if let Some(u) = p.username {
            self.username = u;
        }
        if let Some(pw) = p.password {
            self.password = pw;
        }
        if let Some(d) = p.database {
            self.database = d;
        }
        // The URL describes the whole connection: SSL settings it doesn't
        // mention go back to their defaults. A missing password keeps the
        // typed one (URLs are often shared without it).
        self.ssl_mode = p.ssl_mode.unwrap_or_default();
        self.ssl_ca = p
            .ssl_ca
            .map(|ca| ca.display().to_string())
            .unwrap_or_default();
        self.url.clear();
        Ok(())
    }

    /// Build the config, or a French error message for the form.
    pub fn validate(&self, existing: &[ConnectionConfig]) -> Result<ConnectionConfig, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Le nom est obligatoire".into());
        }
        if self.original_name.as_deref() != Some(name) && existing.iter().any(|c| c.name == name) {
            return Err("Une connexion porte déjà ce nom".into());
        }
        let server = self.uses_server();
        // MongoDB: no default database = every database.
        if self.database.trim().is_empty() && !self.nosql() {
            return Err(if server {
                "La base est obligatoire"
            } else {
                "Le fichier est obligatoire"
            }
            .into());
        }
        let mongo = self.db_type == DatabaseType::MongoDB;
        let database = match self.db_type {
            DatabaseType::Redis => {
                let db = match self.database.trim() {
                    "" => "0",
                    d => d,
                };
                if !db.parse::<u8>().is_ok_and(|n| n <= 15) {
                    return Err("La base Redis est un numéro de 0 à 15".into());
                }
                db.to_string()
            }
            _ => self.database.trim().to_string(),
        };
        // The URI saved never holds the password (see `take_uri_password`).
        let (uri, uri_password) = presets::split_uri_password(&self.uri);
        let uri = (mongo && self.mongo_uri_mode).then_some(uri);
        if let Some(uri) = &uri {
            if !is_mongo_uri(uri) {
                return Err("URI MongoDB attendue : mongodb://… ou mongodb+srv://…".into());
            }
        }
        let port = if server && !self.port.trim().is_empty() {
            Some(
                self.port
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "Le port doit être un nombre".to_string())?,
            )
        } else {
            None
        };
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let azure = self.db_type == DatabaseType::Azure;
        let browser = azure && self.azure_auth == AzureAuthMethod::InteractiveBrowser;
        if azure && self.azure_auth == AzureAuthMethod::ManagedIdentity {
            azure::managed_identity_client_id(Some(&self.username)).map_err(|e| e.to_string())?;
        }
        if browser {
            entra_interactive::validate_interactive(&self.tenant_id, &self.azure_client_id)?;
        }
        let iam = self.gcp_iam();
        if iam {
            gcp_iam::validate(&self.db_type, &self.username)?;
        }
        let folder = self.folder()?;
        Ok(ConnectionConfig {
            name: name.to_string(),
            db_type: self.db_type.clone(),
            host: if server { opt(&self.host) } else { None },
            port,
            username: if server { opt(&self.username) } else { None },
            // The IAM sign-in's password is a token, fetched when connecting.
            password: match uri_password.filter(|_| uri.is_some()) {
                Some(p) => Some(p),
                None if server && !iam && !self.password.is_empty() => Some(self.password.clone()),
                None => None,
            },
            database,
            azure_auth_method: azure.then(|| self.azure_auth.clone()),
            tenant_id: if azure { opt(&self.tenant_id) } else { None },
            // Only that method uses an application of its own.
            azure_client_id: if browser {
                opt(&self.azure_client_id)
            } else {
                None
            },
            color: self.color,
            // MongoDB / Redis without TLS: nothing written.
            ssl_mode: (self.uses_tls()
                && !(self.nosql() && NoSqlTls::from_mode(self.ssl_mode) == NoSqlTls::Off))
                .then_some(self.ssl_mode),
            ssl_ca: if self.uses_tls() {
                opt(&self.ssl_ca).map(Into::into)
            } else {
                None
            },
            flavor: self.flavor.filter(|f| f.driver() == self.db_type),
            // The default method is left out, as in older files.
            db_auth: iam.then_some(DbAuthMethod::GcpIam),
            folder,
            uri,
            auth_source: if mongo { opt(&self.auth_source) } else { None },
            hidden_schemas: self.hidden_schemas.clone(),
        })
    }

    /// Draw the form in the standard modal; `sign_in` is the URL of the
    /// browser sign-in the test waits for (`testing_name`'s; the modal hides
    /// the banner). Returns what the user asked for.
    pub fn ui(&mut self, ui: &mut egui::Ui, sign_in: Option<&str>) -> ModalResponse<FormAction> {
        let title = if self.original_name.is_some() {
            "Modifier la connexion"
        } else {
            "Nouvelle connexion"
        };
        let waiting = sign_in.is_some() && self.testing_name().is_some();
        let message = self.message(waiting);
        let buttons = self.buttons();
        let mut cancel_test = false;
        let response = modal::show(
            ui,
            ModalSpec::new(title, ModalSize::Medium).icon(icon::DATABASE),
            |ui| cancel_test = self.body(ui, sign_in),
            message.as_ref().map(|(kind, text)| (*kind, text.as_str())),
            &buttons,
        );
        match response {
            ModalResponse::None if cancel_test => ModalResponse::Button(FormAction::CancelTest),
            response => response,
        }
    }

    /// Footer buttons (« Annuler » is the modal's own).
    fn buttons(&self) -> Vec<ModalButton<FormAction>> {
        let test = ModalButton::secondary("Tester", FormAction::Test).enabled(!self.testing);
        vec![
            if self.testing {
                test.hint("Test en cours…")
            } else {
                test
            },
            ModalButton::primary("Enregistrer", FormAction::Save),
        ]
    }

    /// The message line: the form's error, else the test's outcome or
    /// progress (`waiting`: the body shows the browser sign-in instead).
    fn message(&self, waiting: bool) -> Option<(MessageKind, String)> {
        if let Some(e) = &self.error {
            return Some((MessageKind::Error, e.clone()));
        }
        match &self.test_result {
            Some(Ok(())) => Some((MessageKind::Success, "Connexion réussie".into())),
            Some(Err(e)) => Some((MessageKind::Error, e.clone())),
            None if self.testing && !waiting => {
                Some((MessageKind::Info, "Test de la connexion en cours…".into()))
            }
            None => None,
        }
    }

    /// The fields, then the browser sign-in the test waits for; whether
    /// its « Annuler » was clicked.
    fn body(&mut self, ui: &mut egui::Ui, sign_in: Option<&str>) -> bool {
        egui::Grid::new("conn_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Nom");
                ui.text_edit_singleline(&mut self.name);
                ui.end_row();

                ui.label("Couleur");
                self.color_row(ui);
                ui.end_row();

                ui.label("Dossier");
                self.folder_row(ui);
                ui.end_row();

                ui.label("Coller une URL");
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.url)
                            .id(egui::Id::new(URL_FIELD))
                            .hint_text("postgresql://user:pass@host:5432/db?sslmode=require"),
                    );
                    // Enter here fills the form, it doesn't save it.
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if enter {
                        ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    }
                    let filled = !self.url.trim().is_empty();
                    let clicked = ui
                        .add_enabled(filled, egui::Button::new("Remplir"))
                        .clicked();
                    if filled && (clicked || enter) {
                        self.url_error = self.apply_url().err();
                    }
                });
                ui.end_row();
                if let Some(e) = &self.url_error {
                    ui.label("");
                    ui.colored_label(ERROR, e);
                    ui.end_row();
                }

                // No compatible products for MongoDB / Redis.
                if !self.nosql() {
                    ui.label("Modèle");
                    let mut flavor = self.flavor;
                    let labels = std::iter::once("Aucun".to_string())
                        .chain(Flavor::ALL.iter().map(|f| f.to_string()));
                    egui::ComboBox::from_id_salt("flavor")
                        .width(popup::combo_width(ui, labels))
                        .selected_text(flavor.map_or("Aucun".to_string(), |f| f.to_string()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut flavor, None, "Aucun");
                            for f in Flavor::ALL {
                                ui.selectable_value(&mut flavor, Some(f), f.to_string());
                            }
                        });
                    if flavor != self.flavor {
                        self.apply_preset(flavor);
                    }
                    ui.end_row();
                }

                ui.label("Type");
                let mut t = self.db_type.clone();
                egui::ComboBox::from_id_salt("db_type")
                    .width(popup::combo_width(
                        ui,
                        DB_TYPES.iter().map(|d| d.to_string()),
                    ))
                    .selected_text(t.to_string())
                    .show_ui(ui, |ui| {
                        for d in DB_TYPES {
                            let label = d.to_string();
                            ui.selectable_value(&mut t, d, label);
                        }
                    });
                if t != self.db_type {
                    self.set_db_type(t);
                }
                ui.end_row();

                if self.db_type == DatabaseType::MongoDB {
                    self.mongo_fields(ui);
                } else if self.db_type == DatabaseType::Redis {
                    self.redis_fields(ui);
                } else if self.uses_server() {
                    self.server_fields(ui);
                } else {
                    ui.label("Fichier");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.database);
                        if ui.button(egui_phosphor::regular::FOLDER_OPEN).clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("SQLite", &["db", "sqlite", "sqlite3"])
                                .pick_file()
                            {
                                self.database = p.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                }

                ui.label(if self.nosql() {
                    "Bases affichées"
                } else {
                    "Schémas affichés"
                });
                self.schemas_row(ui);
                ui.end_row();
            });
        let forced = self.advanced_force();
        if !advanced_fields(&self.db_type, &self.azure_auth).is_empty() {
            ui.add_space(4.0);
            self.advanced_options(ui, forced);
        }
        match (self.testing_name(), sign_in) {
            (Some(name), Some(url)) => {
                ui.add_space(6.0);
                sign_in::waiting_row(ui, name, url, true)
            }
            _ => false,
        }
    }

    /// The running test was cancelled.
    pub fn test_cancelled(&mut self) {
        self.testing = false;
        self.test_result = Some(Err(sign_in::CANCELLED.into()));
    }

    /// Do `action` (a footer button, or the sign-in's « Annuler »);
    /// whether the form stays open.
    pub fn act(&mut self, app: &mut App, action: FormAction) -> bool {
        match action {
            FormAction::CancelTest => {
                if let Some(name) = self.testing_name() {
                    app.sign_ins.dismiss(name);
                }
                app.worker.cancel_test();
                self.test_cancelled();
            }
            FormAction::Test => match self.validate(&app.config.connections) {
                Ok(cfg) => {
                    self.start_test(cfg.clone());
                    app.worker.test_connection(cfg);
                }
                Err(e) => self.error = Some(e),
            },
            FormAction::Save => match self.validate(&app.config.connections) {
                Ok(cfg) => {
                    app.save_connection(self.original_name.clone(), cfg);
                    return false;
                }
                Err(e) => self.error = Some(e),
            },
        }
        true
    }

    fn server_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Hôte");
        let hint = self.flavor.map_or("", |f| presets::preset(f).host_hint);
        let azure = self.db_type == DatabaseType::Azure;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.host).hint_text(hint));
            // Where the tokens come from, deduced from the host.
            if azure {
                ui.weak(AzureCloud::from_host(&self.host).to_string())
                    .on_hover_text("Cloud Azure déduit de l'hôte");
            }
        });
        ui.end_row();
        ui.label("Port");
        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
        ui.end_row();
        if azure {
            ui.label("Authentification");
            egui::ComboBox::from_id_salt("azure_auth")
                .width(popup::combo_width(
                    ui,
                    AZURE_METHODS.iter().map(|m| m.to_string()),
                ))
                .selected_text(self.azure_auth.to_string())
                .show_ui(ui, |ui| {
                    for m in AZURE_METHODS {
                        let label = m.to_string();
                        ui.selectable_value(&mut self.azure_auth, m, label);
                    }
                });
            ui.end_row();
        }
        if self.offers_db_auth() {
            ui.label("Authentification");
            let mut method = self.db_auth;
            egui::ComboBox::from_id_salt("db_auth")
                .width(popup::combo_width(
                    ui,
                    DB_AUTH_METHODS.iter().map(|m| m.to_string()),
                ))
                .selected_text(method.to_string())
                .show_ui(ui, |ui| {
                    for m in DB_AUTH_METHODS {
                        ui.selectable_value(&mut method, m, m.to_string());
                    }
                });
            if method != self.db_auth {
                self.set_db_auth(method);
            }
            ui.end_row();
        }
        for field in auth_fields(&self.db_type, &self.azure_auth, self.db_auth) {
            self.auth_field_row(ui, *field);
            ui.end_row();
        }
        ui.label("Base");
        ui.text_edit_singleline(&mut self.database);
        ui.end_row();
        if self.uses_tls() {
            self.ssl_fields(ui);
        }
    }

    /// MongoDB: a connection string or host / port, the sign-in, the
    /// databases and TLS (grid rows).
    fn mongo_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Connexion");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.mongo_uri_mode, true, "URI");
            ui.radio_value(&mut self.mongo_uri_mode, false, "Paramètres");
        });
        ui.end_row();
        if self.mongo_uri_mode {
            ui.label("URI");
            ui.add(
                egui::TextEdit::singleline(&mut self.uri)
                    .hint_text("mongodb://… ou mongodb+srv://…")
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text("Le mot de passe éventuel est déplacé dans « Mot de passe »");
            // Typed or pasted: the password leaves the URI at once.
            self.take_uri_password();
            ui.end_row();
        } else {
            ui.label("Hôte");
            ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("localhost"));
            ui.end_row();
            ui.label("Port");
            ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
            ui.end_row();
        }
        ui.label("Utilisateur");
        ui.text_edit_singleline(&mut self.username);
        ui.end_row();
        ui.label("Mot de passe");
        ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
        ui.end_row();
        ui.label("Base d'authentification");
        ui.add(egui::TextEdit::singleline(&mut self.auth_source).hint_text(
            if self.mongo_uri_mode {
                "celle de l'URI, sinon admin"
            } else {
                "admin"
            },
        ));
        ui.end_row();
        ui.label("Base par défaut");
        ui.add(egui::TextEdit::singleline(&mut self.database).hint_text("vide = toutes"));
        ui.end_row();
        self.nosql_tls_fields(ui);
        ui.label("");
        ui.label(egui::RichText::new(MONGO_SERVICES_HELP).weak().small());
        ui.end_row();
    }

    /// Redis: host / port, the sign-in (ACL user optional), the database
    /// index and TLS (grid rows).
    fn redis_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Hôte");
        ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("localhost"));
        ui.end_row();
        ui.label("Port");
        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
        ui.end_row();
        ui.label("Utilisateur");
        ui.add(egui::TextEdit::singleline(&mut self.username).hint_text("ACL, facultatif"));
        ui.end_row();
        ui.label("Mot de passe");
        ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
        ui.end_row();
        ui.label("Base");
        let current = self.database.trim().parse::<u8>().unwrap_or(0).min(15);
        let mut chosen = current;
        egui::ComboBox::from_id_salt("redis_db")
            .selected_text(current.to_string())
            .show_ui(ui, |ui| {
                for n in 0..=15u8 {
                    ui.selectable_value(&mut chosen, n, n.to_string());
                }
            });
        if chosen != current || self.database.trim().is_empty() {
            self.database = chosen.to_string();
        }
        ui.end_row();
        self.nosql_tls_fields(ui);
    }

    /// TLS of MongoDB / Redis: off, on (verified), on without checking the
    /// certificate; the extra CA (grid rows).
    fn nosql_tls_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("TLS");
        let current = NoSqlTls::from_mode(self.ssl_mode);
        let mut chosen = current;
        egui::ComboBox::from_id_salt("nosql_tls")
            .width(popup::combo_width(
                ui,
                NoSqlTls::ALL.iter().map(|t| t.label()),
            ))
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for t in NoSqlTls::ALL {
                    ui.selectable_value(&mut chosen, t, t.label());
                }
            });
        // Only a change rewrites the mode (a « Vérifier l'autorité » saved
        // before stays as it is).
        if chosen != current {
            self.ssl_mode = chosen.mode();
        }
        ui.end_row();
        ui.label("Certificat CA");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.ssl_ca).hint_text("racines du système"));
            if ui.button(egui_phosphor::regular::FOLDER_OPEN).clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Certificat", &["pem", "crt", "cer"])
                    .pick_file()
                {
                    self.ssl_ca = p.display().to_string();
                }
            }
        });
        ui.end_row();
    }

    /// The label and the input of `field`: two cells of a grid row.
    fn auth_field_row(&mut self, ui: &mut egui::Ui, field: AuthField) {
        const GUID_HINT: &str = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx";
        let browser = self.azure_auth == AzureAuthMethod::InteractiveBrowser;
        match field {
            AuthField::Tenant if browser => {
                ui.label("Tenant (facultatif)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.tenant_id)
                        .hint_text("auto : demandé au serveur"),
                );
            }
            AuthField::Tenant => {
                ui.label("Tenant ID");
                ui.text_edit_singleline(&mut self.tenant_id);
            }
            AuthField::AppClientId => {
                ui.label("ID client de l'application (facultatif)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.azure_client_id)
                        .hint_text("auto : application des drivers Microsoft"),
                );
            }
            AuthField::Account => {
                ui.label("Compte (facultatif)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.username)
                        .hint_text("prenom.nom@contoso.com"),
                );
            }
            AuthField::IdentityClientId => {
                ui.label(MANAGED_IDENTITY_LABEL);
                ui.add(egui::TextEdit::singleline(&mut self.username).hint_text(GUID_HINT));
            }
            AuthField::GoogleAccount => {
                ui.label("Compte Google");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.username)
                            .hint_text("prenom.nom@domaine.com"),
                    );
                    if let Some(hint) = iam_user_hint(&self.db_type, &self.username) {
                        ui.weak(hint)
                            .on_hover_text("Nom de l'utilisateur IAM sur l'instance Cloud SQL");
                    }
                });
            }
            AuthField::User => {
                ui.label("Utilisateur");
                ui.text_edit_singleline(&mut self.username);
            }
            AuthField::Password => {
                ui.label("Mot de passe");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
            }
        }
    }

    /// « Options avancées » of the browser sign-in: the organization's own
    /// tenant or application, for when the defaults don't suit it. Folded
    /// as the user leaves it unless `forced` (`advanced_force`).
    fn advanced_options(&mut self, ui: &mut egui::Ui, forced: Option<bool>) {
        egui::CollapsingHeader::new("Options avancées")
            .id_salt("aad_advanced")
            .default_open(forced == Some(true))
            .open(forced)
            .show(ui, |ui| {
                egui::Grid::new("conn_form_advanced")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        for field in advanced_fields(&self.db_type, &self.azure_auth) {
                            self.auth_field_row(ui, *field);
                            ui.end_row();
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(
                        egui::RichText::new(
                            "Le tenant est demandé au serveur ; l'application par défaut est \
                             celle des drivers Microsoft. ",
                        )
                        .weak()
                        .small(),
                    );
                    ui.hyperlink_to(egui::RichText::new("voir le README").small(), ENTRA_APP_DOC);
                });
            });
    }

    fn ssl_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("SSL");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ssl_mode")
                .width(popup::combo_width(
                    ui,
                    SslMode::ALL.iter().map(|m| m.to_string()),
                ))
                .selected_text(self.ssl_mode.to_string())
                .show_ui(ui, |ui| {
                    for m in SslMode::ALL {
                        ui.selectable_value(&mut self.ssl_mode, m, m.to_string());
                    }
                });
            // The IAM sign-in connects with TLS whatever the mode says.
            if let Some(note) = iam_ssl_note(self.db_auth, self.ssl_mode) {
                ui.label(egui::RichText::new(note).weak().small());
            }
        });
        ui.end_row();
        ui.label("Certificat CA");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.ssl_ca).hint_text("racines du système"))
                .on_hover_text("utilisé en mode Vérifier");
            if ui.button(egui_phosphor::regular::FOLDER_OPEN).clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Certificat", &["pem", "crt", "cer"])
                    .pick_file()
                {
                    self.ssl_ca = p.display().to_string();
                }
            }
            // The CA is only read when verifying (VerifyCa / VerifyFull).
            if !matches!(self.ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull)
                && !self.ssl_ca.trim().is_empty()
            {
                ui.label(
                    egui::RichText::new("utilisé en mode Vérifier")
                        .weak()
                        .small(),
                );
            }
        });
        ui.end_row();
    }

    /// The folder field, with the existing folders in a menu.
    fn folder_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.folder)
                    .hint_text("Racine")
                    .desired_width(220.0),
            )
            .on_hover_text("« / » sépare les sous-dossiers (ex. Clients/ACME)");
            let mut chosen = None;
            ui.add_enabled_ui(!self.known_folders.is_empty(), |ui| {
                ui.menu_button(icon::CARET_DOWN, |ui| {
                    if ui
                        .button(format!("{} Racine", icon::ARROW_LINE_UP))
                        .clicked()
                    {
                        chosen = Some(String::new());
                    }
                    for path in &self.known_folders {
                        let depth = path.matches(folders::SEP).count();
                        let text = format!(
                            "{}{} {}",
                            "    ".repeat(depth),
                            icon::FOLDER,
                            folders::name(path)
                        );
                        if ui.button(text).clicked() {
                            chosen = Some(path.clone());
                        }
                    }
                    if chosen.is_some() {
                        ui.close_menu();
                    }
                })
                .response
                .on_hover_text("Dossiers existants");
            });
            if let Some(path) = chosen {
                self.folder = path;
            }
        });
    }

    /// « Schémas affichés »: a box per schema when they are loaded, else the
    /// hidden ones, each with « Réafficher ».
    fn schemas_row(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let mut changes = Vec::new();
            match &self.schemas {
                Some(schemas) => {
                    egui::ScrollArea::vertical()
                        .id_salt("form_schemas")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for schema in schemas {
                                let mut shown = !self.hidden_schemas.contains(schema);
                                if ui.checkbox(&mut shown, schema).changed() {
                                    changes.push((schema.clone(), shown));
                                }
                            }
                        });
                }
                None if self.hidden_schemas.is_empty() => {
                    ui.label(egui::RichText::new("Tous (aucun schéma masqué)").weak());
                }
                None => {}
            }
            // Hidden schemas the list doesn't hold (or no list: not
            // connected).
            let listed = self.schemas.as_deref().unwrap_or_default();
            for schema in self.hidden_schemas.iter().filter(|s| !listed.contains(s)) {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{} {schema}", icon::EYE_SLASH)).weak());
                    if ui.small_button("Réafficher").clicked() {
                        changes.push((schema.clone(), true));
                    }
                });
            }
            for (schema, shown) in changes {
                self.set_shown(&schema, shown);
            }
        });
    }

    /// The palette swatches plus "aucune"; the selected one is outlined.
    fn color_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for c in CONNECTION_COLORS {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::click());
                ui.painter().rect_filled(rect, 3.0, theme::rgb(c));
                if self.color == Some(c) {
                    let stroke = Stroke::new(2.0, ui.visuals().strong_text_color());
                    ui.painter()
                        .rect_stroke(rect.expand(2.0), 4.0, stroke, StrokeKind::Outside);
                }
                if response.clicked() {
                    self.color = Some(c);
                }
            }
            if ui
                .selectable_label(self.color.is_none(), "aucune")
                .clicked()
            {
                self.color = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::modal::ButtonKind;

    fn existing() -> Vec<ConnectionConfig> {
        vec![ConnectionConfig {
            name: "Prod".into(),
            ..Default::default()
        }]
    }

    #[test]
    fn new_form_validates_name_and_uniqueness() {
        let mut f = ConnectionForm::new(None);
        f.name = "".into();
        assert_eq!(
            f.validate(&existing()).unwrap_err(),
            "Le nom est obligatoire"
        );
        f.name = "Prod".into();
        assert_eq!(
            f.validate(&existing()).unwrap_err(),
            "Une connexion porte déjà ce nom"
        );
        f.name = "Local".into();
        f.database = "app".into();
        assert!(f.validate(&existing()).is_ok());
    }

    #[test]
    fn the_folder_is_normalized_and_blank_means_the_root() {
        let mut f = ConnectionForm::new(None);
        f.name = "x".into();
        f.database = "d".into();
        f.folder = "  Clients / ACME// ".into();
        assert_eq!(
            f.validate(&[]).unwrap().folder.as_deref(),
            Some("Clients/ACME")
        );
        f.folder = "   ".into();
        assert_eq!(f.validate(&[]).unwrap().folder, None);
        f.folder = "a/../b".into();
        assert!(f.validate(&[]).is_err());
    }

    #[test]
    fn editing_keeps_the_folder_and_the_hidden_schemas() {
        let saved = ConnectionConfig {
            name: "Prod".into(),
            folder: Some("Clients/ACME".into()),
            hidden_schemas: vec!["audit".into()],
            ..Default::default()
        };
        let f = ConnectionForm::new(Some(saved.clone()));
        assert_eq!(f.folder, "Clients/ACME");
        let config = f.validate(std::slice::from_ref(&saved)).unwrap();
        assert_eq!(config.folder, saved.folder);
        assert_eq!(config.hidden_schemas, saved.hidden_schemas);
    }

    #[test]
    fn schemas_are_shown_or_hidden_from_the_form() {
        let saved = ConnectionConfig {
            name: "Prod".into(),
            hidden_schemas: vec!["audit".into()],
            ..Default::default()
        };
        let mut f = ConnectionForm::new(Some(saved.clone())).with_context(
            vec!["A".into()],
            Some(vec!["public".into(), "audit".into()]),
        );
        assert_eq!(f.known_folders, ["A"]);
        f.set_shown("public", false);
        f.set_shown("audit", true);
        f.set_shown("public", false);
        assert_eq!(
            f.validate(&[saved]).unwrap().hidden_schemas,
            ["public".to_string()]
        );
    }

    #[test]
    fn editing_keeps_its_own_name() {
        let mut f = ConnectionForm::new(Some(existing()[0].clone()));
        f.database = "x".into();
        assert!(f.validate(&existing()).is_ok());
    }

    #[test]
    fn port_must_be_numeric_and_type_change_sets_default_port() {
        let mut f = ConnectionForm::new(None);
        f.name = "x".into();
        f.database = "d".into();
        f.set_db_type(DatabaseType::MySQL);
        assert_eq!(f.port, "3306");
        f.port = "abc".into();
        assert!(f.validate(&[]).is_err());
    }

    #[test]
    fn sqlite_config_has_no_host() {
        let mut f = ConnectionForm::new(None);
        f.name = "file".into();
        f.set_db_type(DatabaseType::SQLite);
        f.database = "/tmp/a.db".into();
        let c = f.validate(&[]).unwrap();
        assert!(c.host.is_none() && c.port.is_none());
    }

    #[test]
    fn colour_round_trips_through_validate() {
        let mut f = ConnectionForm::new(None);
        f.name = "x".into();
        assert_eq!(f.validate(&[]).unwrap().color, None);
        f.color = Some(CONNECTION_COLORS[0]);
        assert_eq!(f.validate(&[]).unwrap().color, Some(CONNECTION_COLORS[0]));

        let edited = ConnectionConfig {
            color: Some([1, 2, 3]),
            ..existing()[0].clone()
        };
        let f = ConnectionForm::new(Some(edited));
        assert_eq!(f.validate(&existing()).unwrap().color, Some([1, 2, 3]));
    }

    #[test]
    fn managed_identity_client_id_round_trips_and_is_validated() {
        let id = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        let saved = ConnectionConfig {
            name: "Az".into(),
            db_type: DatabaseType::Azure,
            host: Some("srv.database.usgovcloudapi.net".into()),
            username: Some(id.into()),
            azure_auth_method: Some(AzureAuthMethod::ManagedIdentity),
            database: "app".into(),
            ..Default::default()
        };
        let mut f = ConnectionForm::new(Some(saved.clone()));
        let c = f.validate(&[saved]).unwrap();
        assert_eq!(c.username.as_deref(), Some(id));
        assert_eq!(c.azure_auth_method, Some(AzureAuthMethod::ManagedIdentity));

        f.username = "sa".into();
        assert!(f
            .validate(&[])
            .unwrap_err()
            .contains("n'est pas un ID client"));
        f.username.clear(); // system-assigned identity
        assert_eq!(f.validate(&[]).unwrap().username, None);
    }

    #[test]
    fn editing_keeps_the_app_client_id() {
        let saved = ConnectionConfig {
            name: "Az".into(),
            db_type: DatabaseType::Azure,
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: Some("contoso.com".into()),
            azure_client_id: Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into()),
            database: "app".into(),
            ..Default::default()
        };
        let mut f = ConnectionForm::new(Some(saved.clone()));
        let c = f.validate(std::slice::from_ref(&saved)).unwrap();
        assert_eq!(c.azure_client_id, saved.azure_client_id);
        assert_eq!(c.tenant_id, saved.tenant_id);
        f.set_db_type(DatabaseType::SQLServer);
        assert_eq!(f.validate(&[saved]).unwrap().azure_client_id, None);
    }

    #[test]
    fn each_method_shows_its_own_fields() {
        use AuthField::*;
        assert!(AZURE_METHODS.contains(&AzureAuthMethod::InteractiveBrowser));
        let azure = DatabaseType::Azure;
        let auth_fields =
            |t: &DatabaseType, m: &AzureAuthMethod| auth_fields(t, m, DbAuthMethod::Password);
        // The browser sign-in asks only for the account; its tenant and
        // application are advanced options.
        assert_eq!(
            auth_fields(&azure, &AzureAuthMethod::InteractiveBrowser),
            [Account]
        );
        assert_eq!(
            advanced_fields(&azure, &AzureAuthMethod::InteractiveBrowser),
            [Tenant, AppClientId]
        );
        for method in AZURE_METHODS {
            if method != AzureAuthMethod::InteractiveBrowser {
                assert!(advanced_fields(&azure, &method).is_empty(), "{method}");
            }
        }
        assert!(advanced_fields(
            &DatabaseType::SQLServer,
            &AzureAuthMethod::InteractiveBrowser
        )
        .is_empty());
        assert_eq!(auth_fields(&azure, &AzureAuthMethod::Interactive), [Tenant]);
        assert_eq!(
            auth_fields(&azure, &AzureAuthMethod::ManagedIdentity),
            [IdentityClientId]
        );
        assert_eq!(
            auth_fields(&azure, &AzureAuthMethod::Credentials),
            [User, Password]
        );
        // Other engines ignore the (default) Azure method.
        assert_eq!(
            auth_fields(
                &DatabaseType::Postgres,
                &AzureAuthMethod::InteractiveBrowser
            ),
            [User, Password]
        );
    }

    #[test]
    fn interactive_browser_checks_a_typed_tenant_and_app_client_id() {
        let mut f = ConnectionForm::new(None);
        f.name = "Az".into();
        f.database = "app".into();
        f.set_db_type(DatabaseType::Azure);
        f.azure_auth = AzureAuthMethod::InteractiveBrowser;
        // Both optional: the server's tenant, Microsoft's application.
        let c = f.validate(&[]).unwrap();
        assert_eq!((c.tenant_id, c.azure_client_id), (None, None));
        f.tenant_id = "Common".into();
        assert!(f.validate(&[]).unwrap_err().contains("pas « common »"));
        f.tenant_id = "organizations".into();
        assert_eq!(
            f.validate(&[]).unwrap().tenant_id.as_deref(),
            Some("organizations")
        );
        f.tenant_id = "contoso.com".into();
        f.azure_client_id = "my-app".into();
        assert_eq!(
            f.validate(&[]).unwrap_err(),
            "L'ID client de l'application doit être un GUID"
        );
        f.azure_client_id = " 8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f ".into();
        f.username = "jane@contoso.com".into();
        let c = f.validate(&[]).unwrap();
        assert_eq!(
            c.azure_auth_method,
            Some(AzureAuthMethod::InteractiveBrowser)
        );
        assert_eq!(c.tenant_id.as_deref(), Some("contoso.com"));
        assert_eq!(
            c.azure_client_id.as_deref(),
            Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f")
        );
        assert_eq!(c.username.as_deref(), Some("jane@contoso.com"));

        // The account is optional.
        f.username.clear();
        assert_eq!(f.validate(&[]).unwrap().username, None);
        // Other methods neither check nor keep the application.
        f.azure_auth = AzureAuthMethod::Interactive;
        f.tenant_id.clear();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.azure_client_id, None);
    }

    #[test]
    fn advanced_options_unfold_for_an_error_or_a_value_and_else_stay_as_folded() {
        // First showing in a form: as its values say, whatever egui
        // remembers of the previous form.
        assert_eq!(advanced_forced(None, false, true), Some(false));
        assert_eq!(advanced_forced(None, true, true), Some(true));
        // A value the sign-in rejects: unfolded, even when folded by hand.
        for shown in [None, Some(false), Some(true)] {
            assert_eq!(advanced_forced(shown, true, false), Some(true), "{shown:?}");
        }
        // The browser sign-in chosen again with a tenant or an application.
        assert_eq!(advanced_forced(Some(false), true, true), Some(true));
        // Otherwise as the user left it.
        assert_eq!(advanced_forced(Some(false), false, true), None);
        assert_eq!(advanced_forced(Some(true), true, true), None);
        assert_eq!(advanced_forced(Some(true), false, true), None);
    }

    #[test]
    fn advanced_options_unfold_when_a_tenant_or_an_app_is_filled_in() {
        let browser = |tenant: Option<&str>, app: Option<&str>| ConnectionConfig {
            name: "Az".into(),
            db_type: DatabaseType::Azure,
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: tenant.map(Into::into),
            azure_client_id: app.map(Into::into),
            database: "app".into(),
            ..Default::default()
        };
        let first = |c| ConnectionForm::new(Some(c)).advanced_force();
        // Nothing to set: folded.
        assert_eq!(first(browser(None, None)), Some(false));
        assert_eq!(first(browser(Some(" "), None)), Some(false));
        // A connection naming its own tenant or application shows them.
        assert_eq!(first(browser(Some("contoso.com"), None)), Some(true));
        let app = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        assert_eq!(first(browser(None, Some(app))), Some(true));

        // Then as the user folds it, whatever it holds.
        let mut f = ConnectionForm::new(Some(browser(Some("contoso.com"), None)));
        f.advanced_force();
        assert_eq!(f.advanced_force(), None);
        f.tenant_id.clear();
        assert_eq!(f.advanced_force(), None);
        // A tenant typed for Azure CLI shows when the browser sign-in is
        // chosen again…
        f.azure_auth = AzureAuthMethod::Interactive;
        f.advanced_force();
        f.tenant_id = "contoso.com".into();
        f.azure_auth = AzureAuthMethod::InteractiveBrowser;
        assert_eq!(f.advanced_force(), Some(true));
        assert_eq!(f.advanced_force(), None);
        // …and as well coming back from another engine.
        f.set_db_type(DatabaseType::SQLServer);
        f.advanced_force();
        f.set_db_type(DatabaseType::Azure);
        assert_eq!(f.advanced_force(), Some(true));
        // Without one, it shows as the user left it.
        f.tenant_id.clear();
        f.azure_auth = AzureAuthMethod::Interactive;
        f.advanced_force();
        f.azure_auth = AzureAuthMethod::InteractiveBrowser;
        assert_eq!(f.advanced_force(), None);
        // A value the sign-in rejects keeps it unfolded: the error under
        // the form names that field.
        f.azure_client_id = "my-app".into();
        assert_eq!(f.advanced_force(), Some(true));
        assert_eq!(f.advanced_force(), Some(true));
        f.azure_client_id = app.into();
        assert_eq!(f.advanced_force(), None);
        f.tenant_id = "common".into();
        assert_eq!(f.advanced_force(), Some(true));
    }

    #[test]
    fn the_app_client_id_is_part_of_the_target() {
        let a = ConnectionConfig {
            azure_client_id: Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into()),
            ..existing()[0].clone()
        };
        let b = ConnectionConfig {
            azure_client_id: Some("00000000-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into()),
            ..a.clone()
        };
        assert!(!same_target(&a, &b));
    }

    #[test]
    fn the_wait_shown_is_the_tested_connection_s() {
        let mut f = ConnectionForm::new(None);
        f.name = "Az".into();
        assert_eq!(f.testing_name(), None);
        f.start_test(ConnectionConfig {
            name: "Az".into(),
            ..Default::default()
        });
        assert!(f.testing && f.test_result.is_none());
        // Typed while the test runs: the wait is still the tested name's.
        f.name = "Azure prod".into();
        assert_eq!(f.testing_name(), Some("Az"));
        f.test_cancelled();
        assert_eq!(f.testing_name(), None);
    }

    #[test]
    fn a_test_no_saved_connection_uses_leaves_no_sign_in() {
        let browser = |tenant: &str| ConnectionConfig {
            name: "Az".into(),
            db_type: DatabaseType::Azure,
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: Some(tenant.into()),
            azure_client_id: Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f".into()),
            database: "app".into(),
            ..Default::default()
        };
        // A new connection tested, then not saved.
        let mut f = ConnectionForm::new(None);
        f.start_test(browser("contoso.com"));
        assert_eq!(f.orphaned_tests(&[]).len(), 1);
        assert!(f.orphaned_tests(&[]).is_empty(), "given once");

        // An edit tested with its own sign-in: still used.
        let saved = browser("contoso.com");
        let mut f = ConnectionForm::new(Some(saved.clone()));
        f.start_test(saved.clone());
        assert!(f.orphaned_tests(std::slice::from_ref(&saved)).is_empty());
        // Tested on another tenant, then cancelled: that sign-in goes.
        f.start_test(browser("fabrikam.com"));
        let orphans = f.orphaned_tests(std::slice::from_ref(&saved));
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].tenant_id.as_deref(), Some("fabrikam.com"));
        // Other methods keep nothing.
        f.start_test(ConnectionConfig::default());
        assert!(f.orphaned_tests(&[]).is_empty());
    }

    #[test]
    fn same_target_ignores_name_and_colour() {
        let a = existing()[0].clone();
        let b = ConnectionConfig {
            name: "Other".into(),
            color: Some([1, 2, 3]),
            ..a.clone()
        };
        assert!(same_target(&a, &b));
        let c = ConnectionConfig {
            database: "elsewhere".into(),
            ..a.clone()
        };
        assert!(!same_target(&a, &c));
    }

    #[test]
    fn preset_fills_driver_port_ssl_and_keeps_typed_values() {
        let mut f = ConnectionForm::new(None);
        f.host = "mine.example".into();
        f.username = String::new();
        f.apply_preset(Some(Flavor::CockroachDb));
        assert_eq!(f.db_type, DatabaseType::Postgres);
        assert_eq!(f.port, "26257");
        assert_eq!(f.ssl_mode, SslMode::Require);
        assert_eq!(f.host, "mine.example");
        assert_eq!(f.username, "root");
        f.apply_preset(None);
        assert_eq!(f.flavor, None);
    }

    #[test]
    fn preset_keeps_a_typed_username() {
        let mut f = ConnectionForm::new(None);
        f.username = "alice".into();
        f.apply_preset(Some(Flavor::MariaDb));
        assert_eq!(f.db_type, DatabaseType::MySQL);
        assert_eq!(f.username, "alice");
    }

    #[test]
    fn pasted_url_fills_every_field() {
        let mut f = ConnectionForm::new(None);
        f.url =
            "postgresql://postgres:p%40ss@db.abc.supabase.co:5432/postgres?sslmode=require".into();
        f.apply_url().unwrap();
        assert_eq!(f.host, "db.abc.supabase.co");
        assert_eq!(f.password, "p@ss");
        assert_eq!(f.ssl_mode, SslMode::Require);
        assert_eq!(f.flavor, Some(Flavor::Supabase));
        assert!(f.url.is_empty());

        f.url = "ftp://x".into();
        assert!(f.apply_url().is_err());
        assert_eq!(f.host, "db.abc.supabase.co"); // untouched
    }

    #[test]
    fn pasted_url_resets_ssl_it_does_not_mention_but_keeps_the_password() {
        let mut f = ConnectionForm::new(None);
        f.ssl_mode = SslMode::Require;
        f.ssl_ca = "C:/ca.pem".into();
        f.password = "typed".into();
        f.url = "postgres://u@localhost/db".into();
        f.apply_url().unwrap();
        assert_eq!(f.ssl_mode, SslMode::Prefer);
        assert!(f.ssl_ca.is_empty());
        assert_eq!(f.password, "typed");
        f.url = "postgres://u:p@localhost/db".into();
        f.apply_url().unwrap();
        assert_eq!(f.password, "p");
    }

    fn mongo_form() -> ConnectionForm {
        let mut f = ConnectionForm::new(None);
        f.name = "Atlas".into();
        f.set_db_type(DatabaseType::MongoDB);
        f
    }

    #[test]
    fn a_new_mongo_connection_hides_config_and_local() {
        let mut f = mongo_form();
        assert_eq!(f.hidden_schemas, ["config", "local"]);
        assert_eq!(f.port, "27017");
        // Another engine: they go again.
        f.set_db_type(DatabaseType::Redis);
        assert!(f.hidden_schemas.is_empty());
        assert_eq!(f.port, "6379");
        assert_eq!(f.database, "0");
        // An edited connection keeps its own list.
        let saved = ConnectionConfig {
            name: "m".into(),
            db_type: DatabaseType::Postgres,
            ..Default::default()
        };
        let mut e = ConnectionForm::new(Some(saved));
        e.set_db_type(DatabaseType::MongoDB);
        assert!(e.hidden_schemas.is_empty());
    }

    #[test]
    fn a_typed_mongo_uri_gives_its_password_to_the_password_field() {
        let mut f = mongo_form();
        f.mongo_uri_mode = true;
        f.uri = "mongodb+srv://app:p%40ss@cluster0.x.mongodb.net/shop?retryWrites=true".into();
        f.take_uri_password();
        assert_eq!(
            f.uri,
            "mongodb+srv://app@cluster0.x.mongodb.net/shop?retryWrites=true"
        );
        assert_eq!(f.password, "p@ss");
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.uri.as_deref(), Some(f.uri.as_str()));
        assert_eq!(c.password.as_deref(), Some("p@ss"));
        assert_eq!(c.database, "", "no default database: all of them");
        assert_eq!(c.ssl_mode, None, "TLS from the URI scheme");
        // Even a URI validated as typed loses its password.
        f.uri = "mongodb://u:secret@h:27017/".into();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.uri.as_deref(), Some("mongodb://u@h:27017/"));
        assert_eq!(c.password.as_deref(), Some("secret"));
    }

    #[test]
    fn editing_a_mongo_connection_keeps_its_uri_and_auth_source() {
        let saved = ConnectionConfig {
            name: "Atlas".into(),
            db_type: DatabaseType::MongoDB,
            uri: Some("mongodb+srv://app@cluster0.x.mongodb.net/?retryWrites=true".into()),
            auth_source: Some("admin".into()),
            password: Some("pw".into()),
            hidden_schemas: vec!["local".into()],
            ..Default::default()
        };
        let f = ConnectionForm::new(Some(saved.clone()));
        assert!(f.mongo_uri_mode);
        let c = f.validate(std::slice::from_ref(&saved)).unwrap();
        assert_eq!(c.uri, saved.uri);
        assert_eq!(c.auth_source, saved.auth_source);
        assert_eq!(c.password, saved.password);
        assert_eq!(c.hidden_schemas, saved.hidden_schemas);
        assert!(same_target(&saved, &c), "the session stays open");
        let other = ConnectionConfig {
            auth_source: Some("shop".into()),
            ..saved.clone()
        };
        assert!(!same_target(&saved, &other));
    }

    #[test]
    fn mongo_parameters_need_no_uri_and_a_uri_must_be_mongo() {
        let mut f = mongo_form();
        f.host = "db.local".into();
        f.auth_source = " admin ".into();
        f.database = "shop".into();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.uri, None);
        assert_eq!(c.host.as_deref(), Some("db.local"));
        assert_eq!(c.port, Some(27017));
        assert_eq!(c.auth_source.as_deref(), Some("admin"));
        assert_eq!(c.database, "shop");
        f.mongo_uri_mode = true;
        f.uri = "postgres://x".into();
        assert!(f.validate(&[]).unwrap_err().contains("mongodb"));
    }

    #[test]
    fn pasting_a_mongo_url_fills_the_uri() {
        let mut f = ConnectionForm::new(None);
        f.url = "mongodb://root:p%40ss@h1:27017,h2:27017/shop?replicaSet=rs0".into();
        f.apply_url().unwrap();
        assert_eq!(f.db_type, DatabaseType::MongoDB);
        assert!(f.mongo_uri_mode);
        assert!(!f.uri.contains("p%40ss") && f.uri.contains("h2:27017"));
        assert_eq!(f.password, "p@ss");
        assert_eq!(f.hidden_schemas, ["config", "local"]);
    }

    #[test]
    fn redis_databases_go_from_0_to_15() {
        let mut f = ConnectionForm::new(None);
        f.name = "cache".into();
        f.set_db_type(DatabaseType::Redis);
        f.database = " ".into();
        assert_eq!(f.validate(&[]).unwrap().database, "0");
        f.database = "15".into();
        assert_eq!(f.validate(&[]).unwrap().database, "15");
        f.database = "16".into();
        assert!(f.validate(&[]).unwrap_err().contains("0 à 15"));
        f.database = "3".into();
        f.username = "app".into();
        f.password = "pw".into();
        let c = f.validate(&[]).unwrap();
        assert_eq!((c.uri, c.auth_source), (None, None));
        assert_eq!(c.username.as_deref(), Some("app"));
        assert_eq!(c.port, Some(6379));
        f.url = "rediss://default:p%20w@cache.example.com:6380/2".into();
        f.apply_url().unwrap();
        assert_eq!(
            (f.host.as_str(), f.port.as_str(), f.database.as_str()),
            ("cache.example.com", "6380", "2")
        );
        assert_eq!(NoSqlTls::from_mode(f.ssl_mode), NoSqlTls::On);
    }

    #[test]
    fn nosql_tls_maps_to_the_ssl_mode() {
        for t in NoSqlTls::ALL {
            assert_eq!(NoSqlTls::from_mode(t.mode()), t);
        }
        assert_eq!(NoSqlTls::from_mode(SslMode::Prefer), NoSqlTls::Off);
        assert_eq!(NoSqlTls::from_mode(SslMode::VerifyCa), NoSqlTls::On);
        let mut f = mongo_form();
        f.ssl_mode = NoSqlTls::Insecure.mode();
        f.ssl_ca = "C:/global-bundle.pem".into();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.ssl_mode, Some(SslMode::Require));
        assert_eq!(c.ssl_ca, Some("C:/global-bundle.pem".into()));
        assert_eq!(c.nosql_tls(), Some(false));
        f.ssl_mode = SslMode::Disable;
        assert_eq!(f.validate(&[]).unwrap().ssl_mode, None);
    }

    #[test]
    fn validate_keeps_ssl_and_flavor_for_pg_mysql_only() {
        let mut f = ConnectionForm::new(None);
        f.name = "x".into();
        f.ssl_mode = SslMode::VerifyFull;
        f.ssl_ca = "C:/ca.pem".into();
        f.flavor = Some(Flavor::Neon);
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!(c.ssl_ca, Some("C:/ca.pem".into()));
        assert_eq!(c.flavor, Some(Flavor::Neon));
        f.set_db_type(DatabaseType::SQLServer);
        let c = f.validate(&[]).unwrap();
        assert_eq!((c.ssl_mode, c.ssl_ca, c.flavor), (None, None, None));
    }

    #[test]
    fn editing_keeps_ssl_settings_and_flavor() {
        let edited = ConnectionConfig {
            ssl_mode: Some(SslMode::VerifyCa),
            ssl_ca: Some("/etc/ca.pem".into()),
            flavor: Some(Flavor::TimescaleDb),
            ..existing()[0].clone()
        };
        let f = ConnectionForm::new(Some(edited));
        let c = f.validate(&existing()).unwrap();
        assert_eq!(c.ssl_mode, Some(SslMode::VerifyCa));
        assert_eq!(c.ssl_ca, Some("/etc/ca.pem".into()));
        assert_eq!(c.flavor, Some(Flavor::TimescaleDb));
    }

    fn iam_form(db_type: DatabaseType) -> ConnectionForm {
        let mut f = ConnectionForm::new(None);
        f.name = "Cloud SQL".into();
        f.set_db_type(db_type);
        f.set_db_auth(DbAuthMethod::GcpIam);
        f
    }

    #[test]
    fn gcp_iam_asks_for_the_google_account_only() {
        use AuthField::*;
        let none = AzureAuthMethod::default();
        for t in [DatabaseType::Postgres, DatabaseType::MySQL] {
            assert_eq!(
                auth_fields(&t, &none, DbAuthMethod::GcpIam),
                [GoogleAccount]
            );
            assert_eq!(
                auth_fields(&t, &none, DbAuthMethod::Password),
                [User, Password]
            );
        }
        // Other engines don't offer it.
        for t in [DatabaseType::SQLServer, DatabaseType::SQLite] {
            assert_eq!(
                auth_fields(&t, &none, DbAuthMethod::GcpIam),
                [User, Password]
            );
        }
        assert_eq!(
            auth_fields(
                &DatabaseType::Azure,
                &AzureAuthMethod::Interactive,
                DbAuthMethod::GcpIam
            ),
            [Tenant]
        );
        assert!(ConnectionForm::new(None).offers_db_auth());
        assert!(!iam_form(DatabaseType::SQLServer).offers_db_auth());
    }

    #[test]
    fn gcp_iam_needs_an_account_and_keeps_no_password() {
        let mut f = iam_form(DatabaseType::Postgres);
        // The driver's default user is no Google account.
        assert_eq!(f.username, "");
        f.password = "old secret".into();
        assert_eq!(f.validate(&[]).unwrap_err(), "Indique ton compte Google");
        f.username = " jane@example.com ".into();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.db_auth, Some(DbAuthMethod::GcpIam));
        assert_eq!(c.username.as_deref(), Some("jane@example.com"));
        assert_eq!(c.password, None);

        // Back to the password: nothing to check, nothing saved.
        f.set_db_auth(DbAuthMethod::Password);
        f.username.clear();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.db_auth, None);
        assert_eq!(c.password.as_deref(), Some("old secret"));
    }

    #[test]
    fn gcp_iam_is_saved_for_postgres_and_mysql_only() {
        let mut f = iam_form(DatabaseType::MySQL);
        f.username = "jane@example.com".into();
        assert_eq!(f.validate(&[]).unwrap().db_auth, Some(DbAuthMethod::GcpIam));
        f.set_db_type(DatabaseType::SQLServer);
        f.username.clear();
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.db_auth, None, "no account asked either");
    }

    #[test]
    fn switching_to_gcp_iam_keeps_a_typed_user() {
        let mut f = ConnectionForm::new(None);
        f.username = "jane@example.com".into();
        f.set_db_auth(DbAuthMethod::GcpIam);
        assert_eq!(f.username, "jane@example.com");
        let mut f = ConnectionForm::new(None);
        f.set_db_type(DatabaseType::MySQL);
        f.username = "root".into();
        f.set_db_auth(DbAuthMethod::GcpIam);
        assert_eq!(f.username, "");
    }

    #[test]
    fn editing_keeps_gcp_iam() {
        let saved = ConnectionConfig {
            name: "Cloud SQL".into(),
            username: Some("jane@example.com".into()),
            db_auth: Some(DbAuthMethod::GcpIam),
            ..Default::default()
        };
        let f = ConnectionForm::new(Some(saved.clone()));
        assert_eq!(f.db_auth, DbAuthMethod::GcpIam);
        let c = f.validate(std::slice::from_ref(&saved)).unwrap();
        let expected = ConnectionConfig {
            ssl_mode: Some(SslMode::Prefer),
            ..saved.clone()
        };
        assert_eq!(c, expected);
        // An old connection (no `db_auth`) stays as it was saved.
        let old = ConnectionConfig::default();
        let c = ConnectionForm::new(Some(old.clone()))
            .validate(&[old])
            .unwrap();
        assert_eq!(c.db_auth, None);
    }

    #[test]
    fn gcp_iam_hints_the_database_user_and_the_ssl_floor() {
        assert_eq!(iam_user_hint(&DatabaseType::Postgres, " "), None);
        assert_eq!(
            iam_user_hint(&DatabaseType::Postgres, "jane@example.com").as_deref(),
            Some("utilisateur base : jane@example.com")
        );
        assert_eq!(
            iam_user_hint(&DatabaseType::MySQL, "jane@example.com").as_deref(),
            Some("utilisateur base : jane")
        );
        assert_eq!(iam_ssl_note(DbAuthMethod::Password, SslMode::Prefer), None);
        for m in [SslMode::Disable, SslMode::Prefer] {
            assert_eq!(iam_ssl_note(DbAuthMethod::GcpIam, m), Some(IAM_SSL_NOTE));
        }
        for m in [SslMode::Require, SslMode::VerifyCa, SslMode::VerifyFull] {
            assert_eq!(iam_ssl_note(DbAuthMethod::GcpIam, m), None);
        }
        assert!(IAM_SSL_NOTE.contains("Obligatoire"));
    }

    #[test]
    fn the_db_auth_is_part_of_the_target() {
        let a = ConnectionConfig::default();
        let b = ConnectionConfig {
            db_auth: Some(DbAuthMethod::GcpIam),
            ..a.clone()
        };
        assert!(!same_target(&a, &b));
        // An old connection saved again with the default method.
        let c = ConnectionConfig {
            db_auth: Some(DbAuthMethod::Password),
            ..a.clone()
        };
        assert!(same_target(&a, &c));
    }

    #[test]
    fn ssl_settings_are_part_of_the_target() {
        let a = ConnectionConfig::default();
        let b = ConnectionConfig {
            ssl_mode: Some(SslMode::Require),
            ..a.clone()
        };
        assert!(!same_target(&a, &b));
        let c = ConnectionConfig {
            ssl_ca: Some("ca.pem".into()),
            ..a.clone()
        };
        assert!(!same_target(&a, &c));
    }

    #[test]
    fn footer_offers_test_and_save_and_test_waits_for_the_running_one() {
        let mut f = ConnectionForm::new(None);
        let b = f.buttons();
        assert_eq!(
            b.iter()
                .map(|b| (b.label.as_str(), b.kind, b.action, b.enabled))
                .collect::<Vec<_>>(),
            vec![
                ("Tester", ButtonKind::Secondary, FormAction::Test, true),
                ("Enregistrer", ButtonKind::Primary, FormAction::Save, true),
            ]
        );
        f.start_test(ConnectionConfig::default());
        let b = f.buttons();
        assert!(!b[0].enabled);
        assert_eq!(b[0].hint.as_deref(), Some("Test en cours…"));
        // Saving stays possible while a test runs (as before).
        assert!(b[1].enabled);
    }

    #[test]
    fn message_line_shows_the_error_then_the_test() {
        let mut f = ConnectionForm::new(None);
        assert_eq!(f.message(false), None);
        f.start_test(ConnectionConfig::default());
        assert_eq!(
            f.message(false),
            Some((MessageKind::Info, "Test de la connexion en cours…".into()))
        );
        // The body shows the browser sign-in instead.
        assert_eq!(f.message(true), None);
        f.testing = false;
        f.test_result = Some(Ok(()));
        assert_eq!(
            f.message(false),
            Some((MessageKind::Success, "Connexion réussie".into()))
        );
        f.test_result = Some(Err("refusée".into()));
        assert_eq!(
            f.message(false),
            Some((MessageKind::Error, "refusée".into()))
        );
        f.error = Some("Nom requis".into());
        assert_eq!(
            f.message(false),
            Some((MessageKind::Error, "Nom requis".into()))
        );
    }

    fn enter() -> egui::Event {
        egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// Show `form` in a modal once per entry of `frames`, the URL field
    /// focused first when `url_focus`; the responses.
    fn run(
        form: &mut ConnectionForm,
        frames: Vec<Vec<egui::Event>>,
        url_focus: bool,
    ) -> Vec<ModalResponse<FormAction>> {
        let ctx = egui::Context::default();
        let mut out = vec![];
        for (n, events) in frames.into_iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                events,
                focused: true,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let modal =
                    egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| form.ui(ui, None));
                out.push(modal.inner);
                if n == 0 && url_focus {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new(URL_FIELD)));
                }
            });
        }
        out
    }

    #[test]
    fn enter_saves_the_form() {
        let mut f = ConnectionForm::new(None);
        let r = run(&mut f, vec![vec![], vec![enter()]], false);
        assert_eq!(
            r,
            vec![ModalResponse::None, ModalResponse::Button(FormAction::Save)]
        );
    }

    #[test]
    fn enter_in_the_url_field_fills_the_form_without_saving() {
        let mut f = ConnectionForm::new(None);
        f.url = "postgresql://bob@db.example.com:5433/shop".into();
        let r = run(&mut f, vec![vec![], vec![enter()], vec![]], true);
        assert_eq!(r, vec![ModalResponse::None; 3]);
        assert_eq!(f.host, "db.example.com");
        assert_eq!(f.port, "5433");
        assert!(f.url.is_empty());
        // Empty, Enter there does nothing either.
        let mut f = ConnectionForm::new(None);
        let r = run(&mut f, vec![vec![], vec![enter()]], true);
        assert_eq!(r[1], ModalResponse::None);
    }
}
