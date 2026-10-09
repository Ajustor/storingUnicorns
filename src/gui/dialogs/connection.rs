//! New / edit connection form.

use egui::{Sense, Stroke, StrokeKind};

use crate::engine::models::{AzureAuthMethod, ConnectionConfig, DatabaseType};
use crate::gui::theme::{self, CONNECTION_COLORS, ERROR, SUCCESS};

const DB_TYPES: [DatabaseType; 5] = [
    DatabaseType::Postgres,
    DatabaseType::MySQL,
    DatabaseType::SQLite,
    DatabaseType::SQLServer,
    DatabaseType::Azure,
];

const AZURE_METHODS: [AzureAuthMethod; 3] = [
    AzureAuthMethod::Credentials,
    AzureAuthMethod::Interactive,
    AzureAuthMethod::ManagedIdentity,
];

fn default_port(t: &DatabaseType) -> &'static str {
    match t {
        DatabaseType::Postgres => "5432",
        DatabaseType::MySQL => "3306",
        DatabaseType::SQLite => "",
        DatabaseType::SQLServer | DatabaseType::Azure => "1433",
    }
}

/// Whether two configs reach the same database the same way (everything but
/// the name and the colour is equal): an open session stays valid.
pub fn same_target(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    a.db_type == b.db_type
        && a.host == b.host
        && a.port == b.port
        && a.username == b.username
        && a.password == b.password
        && a.database == b.database
        && a.azure_auth_method == b.azure_auth_method
        && a.tenant_id == b.tenant_id
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
    pub color: Option<[u8; 3]>,
    pub error: Option<String>,
    pub testing: bool,
    pub test_result: Option<Result<(), String>>,
}

pub enum FormAction {
    None,
    Save,
    Test,
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
            name: c.name,
            db_type: c.db_type,
            database: c.database,
            color: c.color,
            error: None,
            testing: false,
            test_result: None,
        }
    }

    pub fn set_db_type(&mut self, t: DatabaseType) {
        if self.port.is_empty() || self.port == default_port(&self.db_type) {
            self.port = default_port(&t).to_string();
        }
        self.db_type = t;
    }

    fn uses_server(&self) -> bool {
        self.db_type != DatabaseType::SQLite
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
            color: self.color,
            // Filled by the SSL / preset fields of the dialog (later task).
            ssl_mode: None,
            ssl_ca: None,
            flavor: None,
        })
    }

    /// Draw the form. Returns the action the user took.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> FormAction {
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
        action
    }

    fn server_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Hôte");
        ui.text_edit_singleline(&mut self.host);
        ui.end_row();
        ui.label("Port");
        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
        ui.end_row();
        if self.db_type == DatabaseType::Azure {
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
            ui.label("Tenant ID");
            ui.text_edit_singleline(&mut self.tenant_id);
            ui.end_row();
        }
        let needs_credentials =
            self.db_type != DatabaseType::Azure || self.azure_auth == AzureAuthMethod::Credentials;
        if needs_credentials {
            ui.label("Utilisateur");
            ui.text_edit_singleline(&mut self.username);
            ui.end_row();
            ui.label("Mot de passe");
            ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
            ui.end_row();
        }
        ui.label("Base");
        ui.text_edit_singleline(&mut self.database);
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
}
