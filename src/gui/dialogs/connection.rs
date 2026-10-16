//! New / edit connection form.

use egui::{Sense, Stroke, StrokeKind};

use crate::engine::db::azure::{self, AzureCloud};
use crate::engine::db::entra_interactive;
use crate::engine::models::{AzureAuthMethod, ConnectionConfig, DatabaseType, Flavor, SslMode};
use crate::engine::presets;
use crate::gui::sign_in;
use crate::gui::theme::{self, CONNECTION_COLORS, ERROR, SUCCESS};

const DB_TYPES: [DatabaseType; 5] = [
    DatabaseType::Postgres,
    DatabaseType::MySQL,
    DatabaseType::SQLite,
    DatabaseType::SQLServer,
    DatabaseType::Azure,
];

const AZURE_METHODS: [AzureAuthMethod; 4] = [
    AzureAuthMethod::Credentials,
    AzureAuthMethod::Interactive,
    AzureAuthMethod::InteractiveBrowser,
    AzureAuthMethod::ManagedIdentity,
];

/// The username field of an Azure managed identity connection.
const MANAGED_IDENTITY_LABEL: &str = "ID client de l'identité (vide = identité système)";

/// The README section on registering the application of an interactive
/// browser sign-in.
const ENTRA_APP_DOC: &str = "https://github.com/Ajustor/storingUnicorns#azure-ad-interactif";

/// A field of the authentication part of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthField {
    /// `tenant_id`.
    Tenant,
    /// `azure_client_id`: the organization's Entra ID application.
    AppClientId,
    /// `username` as the Entra ID account (login hint), optional.
    Account,
    /// `username` as the client ID of a user-assigned managed identity.
    IdentityClientId,
    User,
    Password,
}

/// The authentication fields of `db_type` (and of Azure `method`), in order.
fn auth_fields(db_type: &DatabaseType, method: &AzureAuthMethod) -> &'static [AuthField] {
    use AuthField::*;
    if *db_type != DatabaseType::Azure {
        return &[User, Password];
    }
    match method {
        AzureAuthMethod::Credentials => &[User, Password],
        AzureAuthMethod::Interactive => &[Tenant],
        AzureAuthMethod::InteractiveBrowser => &[Tenant, AppClientId, Account],
        AzureAuthMethod::ManagedIdentity => &[IdentityClientId],
    }
}

fn default_port(t: &DatabaseType) -> &'static str {
    match t {
        DatabaseType::Postgres => "5432",
        DatabaseType::MySQL => "3306",
        DatabaseType::SQLite => "",
        DatabaseType::SQLServer | DatabaseType::Azure => "1433",
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
    pub color: Option<[u8; 3]>,
    /// Compatible product; `None` = the bare engine.
    pub flavor: Option<Flavor>,
    pub ssl_mode: SslMode,
    /// CA certificate path; empty = system roots only.
    pub ssl_ca: String,
    /// Pasted connection URL, consumed by "Remplir".
    pub url: String,
    pub url_error: Option<String>,
    pub error: Option<String>,
    pub testing: bool,
    pub test_result: Option<Result<(), String>>,
    /// The configs tested since the form opened, the running test's last.
    tested: Vec<ConnectionConfig>,
}

pub enum FormAction {
    None,
    Save,
    Test,
    /// Stop the running test (its browser sign-in).
    CancelTest,
    Cancel,
}

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
            url: String::new(),
            url_error: None,
            error: None,
            testing: false,
            test_result: None,
            tested: Vec::new(),
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
        self.db_type = t;
    }

    fn uses_server(&self) -> bool {
        self.db_type != DatabaseType::SQLite
    }

    /// Whether the driver honours the SSL settings (PostgreSQL / MySQL).
    fn uses_tls(&self) -> bool {
        matches!(self.db_type, DatabaseType::Postgres | DatabaseType::MySQL)
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
        if self.database.trim().is_empty() {
            return Err(if server {
                "La base est obligatoire"
            } else {
                "Le fichier est obligatoire"
            }
            .into());
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
        Ok(ConnectionConfig {
            name: name.to_string(),
            db_type: self.db_type.clone(),
            host: if server { opt(&self.host) } else { None },
            port,
            username: if server { opt(&self.username) } else { None },
            password: if server && !self.password.is_empty() {
                Some(self.password.clone())
            } else {
                None
            },
            database: self.database.trim().to_string(),
            azure_auth_method: azure.then(|| self.azure_auth.clone()),
            tenant_id: if azure { opt(&self.tenant_id) } else { None },
            // Only that method uses an application of its own.
            azure_client_id: if browser {
                opt(&self.azure_client_id)
            } else {
                None
            },
            color: self.color,
            ssl_mode: self.uses_tls().then_some(self.ssl_mode),
            ssl_ca: if self.uses_tls() {
                opt(&self.ssl_ca).map(Into::into)
            } else {
                None
            },
            flavor: self.flavor.filter(|f| f.driver() == self.db_type),
        })
    }

    /// Draw the form; `sign_in` is the URL of the browser sign-in the test
    /// waits for (`testing_name`'s; the modal hides the banner). Returns the
    /// action the user took.
    pub fn ui(&mut self, ui: &mut egui::Ui, sign_in: Option<&str>) -> FormAction {
        let mut action = FormAction::None;
        ui.heading(if self.original_name.is_some() {
            "Modifier la connexion"
        } else {
            "Nouvelle connexion"
        });
        ui.add_space(6.0);
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

                ui.label("Coller une URL");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.url)
                            .hint_text("postgresql://user:pass@host:5432/db?sslmode=require"),
                    );
                    if ui
                        .add_enabled(!self.url.trim().is_empty(), egui::Button::new("Remplir"))
                        .clicked()
                    {
                        self.url_error = self.apply_url().err();
                    }
                });
                ui.end_row();

                ui.label("Modèle");
                let mut flavor = self.flavor;
                egui::ComboBox::from_id_salt("flavor")
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

                ui.label("Type");
                let mut t = self.db_type.clone();
                egui::ComboBox::from_id_salt("db_type")
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

                if self.uses_server() {
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
            });

        if let Some(e) = &self.url_error {
            ui.colored_label(ERROR, e);
        }
        if let Some(e) = &self.error {
            ui.colored_label(ERROR, e);
        }
        match &self.test_result {
            Some(Ok(())) => {
                ui.colored_label(SUCCESS, "Connexion réussie");
            }
            Some(Err(e)) => {
                ui.colored_label(ERROR, e);
            }
            None => {}
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Enregistrer").clicked() {
                action = FormAction::Save;
            }
            if ui
                .add_enabled(!self.testing, egui::Button::new("Tester"))
                .clicked()
            {
                action = FormAction::Test;
            }
            if self.testing {
                ui.spinner();
            }
            if ui.button("Annuler").clicked() {
                action = FormAction::Cancel;
            }
        });
        if let (Some(name), Some(url)) = (self.testing_name(), sign_in) {
            ui.add_space(6.0);
            if sign_in::waiting_row(ui, name, url, true) {
                action = FormAction::CancelTest;
            }
        }
        action
    }

    /// The running test was cancelled.
    pub fn test_cancelled(&mut self) {
        self.testing = false;
        self.test_result = Some(Err(sign_in::CANCELLED.into()));
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
                .selected_text(self.azure_auth.to_string())
                .show_ui(ui, |ui| {
                    for m in AZURE_METHODS {
                        let label = m.to_string();
                        ui.selectable_value(&mut self.azure_auth, m, label);
                    }
                });
            ui.end_row();
        }
        const GUID_HINT: &str = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx";
        let browser = self.azure_auth == AzureAuthMethod::InteractiveBrowser;
        for field in auth_fields(&self.db_type, &self.azure_auth) {
            match field {
                AuthField::Tenant if browser => {
                    ui.label("Tenant (ID ou domaine)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.tenant_id).hint_text("contoso.com"),
                    );
                }
                AuthField::Tenant => {
                    ui.label("Tenant ID");
                    ui.text_edit_singleline(&mut self.tenant_id);
                }
                AuthField::AppClientId => {
                    ui.label("ID client de l'application");
                    ui.vertical(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.azure_client_id)
                                .hint_text(GUID_HINT),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label(
                                egui::RichText::new("Application à enregistrer dans Entra ID : ")
                                    .weak()
                                    .small(),
                            );
                            ui.hyperlink_to(
                                egui::RichText::new("voir le README").small(),
                                ENTRA_APP_DOC,
                            );
                        });
                    });
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
                AuthField::User => {
                    ui.label("Utilisateur");
                    ui.text_edit_singleline(&mut self.username);
                }
                AuthField::Password => {
                    ui.label("Mot de passe");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                }
            }
            ui.end_row();
        }
        ui.label("Base");
        ui.text_edit_singleline(&mut self.database);
        ui.end_row();
        if self.uses_tls() {
            self.ssl_fields(ui);
        }
    }

    fn ssl_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("SSL");
        egui::ComboBox::from_id_salt("ssl_mode")
            .selected_text(self.ssl_mode.to_string())
            .show_ui(ui, |ui| {
                for m in SslMode::ALL {
                    ui.selectable_value(&mut self.ssl_mode, m, m.to_string());
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
        assert_eq!(
            auth_fields(&azure, &AzureAuthMethod::InteractiveBrowser),
            [Tenant, AppClientId, Account]
        );
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

        f.url = "redis://x".into();
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
}
