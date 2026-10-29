//! Small MongoDB / Redis dialogs: confirmations of destructive tasks and
//! console commands, the one-field prompts (new collection, key rename,
//! expiration) and « Nouvelle clé… ».

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::nosql::redis::{self, KeyEdit, RedisType, RedisValue, StreamEntry};
use crate::engine::nosql::Warning;
use crate::gui::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use crate::gui::tabs::keys::{parse_ttl, type_icon, type_label, TYPES};
use crate::gui::theme::ERROR;
use crate::gui::worker::nosql::NoSqlTask;

/// A destructive task to confirm (drop a collection, a database, empty
/// one, flush a Redis database, delete a key…).
#[derive(Clone, Debug, PartialEq)]
pub struct ConfirmTask {
    pub connection: String,
    pub title: String,
    pub text: String,
    /// Label of the danger button.
    pub button: String,
    pub task: NoSqlTask,
}

impl ConfirmTask {
    pub fn ui(&self, ui: &mut egui::Ui) -> ModalResponse<()> {
        modal::show(
            ui,
            ModalSpec::new(&self.title, ModalSize::Small).icon(icon::WARNING),
            |ui| {
                ui.label(RichText::new(&self.connection).weak().small());
                ui.add_space(4.0);
                ui.label(RichText::new(&self.text).color(ERROR));
            },
            None,
            &[ModalButton::danger(self.button.clone(), ())],
        )
    }
}

/// The confirmation of console commands `nosql::warnings` found dangerous.
pub fn warnings_ui(ui: &mut egui::Ui, warnings: &[Warning]) -> ModalResponse<()> {
    modal::show(
        ui,
        ModalSpec::new("Commande dangereuse", ModalSize::Medium).icon(icon::WARNING),
        |ui| {
            ui.label("Ces commandes vont être exécutées :");
            ui.add_space(4.0);
            for w in warnings {
                ui.label(RichText::new(format!("• {}", w.reason)).color(ERROR));
                ui.label(
                    RichText::new(crate::gui::history_popup::preview(&w.unit, 160))
                        .monospace()
                        .weak(),
                );
            }
        },
        None,
        &[ModalButton::danger("Exécuter quand même", ())],
    )
}

/// What a one-field prompt asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A collection in MongoDB database `db`.
    NewCollection { db: String },
    /// A new name for Redis key `key`.
    RenameKey { db: u8, key: Vec<u8> },
    /// A TTL for Redis key `key`.
    Expire { db: u8, key: Vec<u8> },
}

pub struct Prompt {
    pub connection: String,
    pub kind: PromptKind,
    pub text: String,
    pub error: Option<String>,
}

impl Prompt {
    pub fn new(connection: &str, kind: PromptKind, text: String) -> Self {
        Self {
            connection: connection.to_string(),
            kind,
            text,
            error: None,
        }
    }

    /// The task of the prompt, or why the text is refused.
    pub fn task(&self) -> Result<NoSqlTask, String> {
        let text = self.text.trim();
        match &self.kind {
            PromptKind::NewCollection { db } => {
                check_collection_name(text)?;
                Ok(NoSqlTask::CreateCollection {
                    db: db.clone(),
                    name: text.to_string(),
                })
            }
            PromptKind::RenameKey { db, key } => {
                if self.text.is_empty() {
                    return Err("Le nom est obligatoire".into());
                }
                let to = redis::parse_display(&self.text)?;
                if &to == key {
                    return Err("Même nom que la clé".into());
                }
                Ok(NoSqlTask::EditKey {
                    db: *db,
                    key: key.clone(),
                    edits: vec![KeyEdit::Rename { to }],
                })
            }
            PromptKind::Expire { db, key } => Ok(NoSqlTask::EditKey {
                db: *db,
                key: key.clone(),
                edits: vec![KeyEdit::Expire(parse_ttl(text)?)],
            }),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> ModalResponse<()> {
        let (title, label, hint, button) = match &self.kind {
            PromptKind::NewCollection { .. } => {
                ("Nouvelle collection", "Nom", "commandes", "Créer")
            }
            PromptKind::RenameKey { .. } => ("Renommer la clé", "Nouveau nom", "", "Renommer"),
            PromptKind::Expire { .. } => (
                "Expiration de la clé",
                "Secondes",
                "vide = persistante",
                "Appliquer",
            ),
        };
        let context = match &self.kind {
            PromptKind::NewCollection { db } => format!("{} · base {db}", self.connection),
            PromptKind::RenameKey { db, key } | PromptKind::Expire { db, key } => {
                format!(
                    "{} · db{db} · {}",
                    self.connection,
                    redis::display_bytes(key)
                )
            }
        };
        let error = self.error.clone();
        let text = &mut self.text;
        modal::show(
            ui,
            ModalSpec::new(title, ModalSize::Small),
            |ui| {
                ui.label(RichText::new(context).weak().small());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(label);
                    let r = ui.add(egui::TextEdit::singleline(text).hint_text(hint));
                    if !r.has_focus() && text.is_empty() {
                        r.request_focus();
                    }
                });
            },
            error.as_deref().map(|e| (MessageKind::Error, e)),
            &[ModalButton::primary(button, ())],
        )
    }
}

/// MongoDB collection names: not empty, no `$` nor NUL, not `system.*`.
pub fn check_collection_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        Err("Le nom est obligatoire".into())
    } else if name.contains(['$', '\0']) {
        Err("Le nom ne peut pas contenir « $ »".into())
    } else if name.starts_with("system.") {
        Err("Les noms « system.* » sont réservés".into())
    } else {
        Ok(())
    }
}

/// « Nouvelle clé… » of a Redis database.
pub struct NewKey {
    pub connection: String,
    pub db: u8,
    pub name: String,
    pub kind: RedisType,
    /// String: the text; other types: JSON (see `value_hint`).
    pub value: String,
    pub ttl: String,
    pub error: Option<String>,
}

/// A sample value of each type, shown as the JSON the value field expects.
fn sample(kind: &RedisType) -> Option<RedisValue> {
    let b = |s: &str| s.as_bytes().to_vec();
    Some(match kind {
        RedisType::String => RedisValue::String(b("valeur")),
        RedisType::List => RedisValue::List(vec![b("a"), b("b")]),
        RedisType::Set => RedisValue::Set(vec![b("a"), b("b")]),
        RedisType::ZSet => RedisValue::ZSet(vec![(b("a"), 1.0), (b("b"), 2.0)]),
        RedisType::Hash => RedisValue::Hash(vec![(b("champ"), b("valeur"))]),
        RedisType::Stream => RedisValue::Stream(vec![StreamEntry {
            id: "*".into(),
            fields: vec![(b("champ"), b("valeur"))],
        }]),
        RedisType::Other(_) => return None,
    })
}

/// The hint of the value field: the JSON form of a sample value of the type
/// (as `redis::value_to_json` writes it).
pub fn value_hint(kind: &RedisType) -> String {
    if *kind == RedisType::String {
        return "texte (\\xHH pour un octet)".into();
    }
    match sample(kind).map(|v| redis::value_to_json(&v)) {
        Some(json) if !json.is_null() => json.to_string(),
        _ => "JSON".into(),
    }
}

impl NewKey {
    pub fn new(connection: &str, db: u8) -> Self {
        Self {
            connection: connection.to_string(),
            db,
            name: String::new(),
            kind: RedisType::String,
            value: String::new(),
            ttl: String::new(),
            error: None,
        }
    }

    /// The creation task, or why the form is refused.
    pub fn task(&self) -> Result<NoSqlTask, String> {
        if self.name.is_empty() {
            return Err("Le nom est obligatoire".into());
        }
        let key = redis::parse_display(&self.name)?;
        let value = match &self.kind {
            RedisType::String => RedisValue::String(redis::parse_display(&self.value)?),
            kind => {
                let json: serde_json::Value = serde_json::from_str(&self.value)
                    .map_err(|e| format!("Valeur : JSON invalide ({e})"))?;
                redis::value_from_json(kind, &json)?
            }
        };
        Ok(NoSqlTask::CreateKey {
            db: self.db,
            key,
            value,
            ttl: parse_ttl(&self.ttl)?,
        })
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> ModalResponse<()> {
        let error = self.error.clone();
        let context = format!("{} · db{}", self.connection, self.db);
        modal::show(
            ui,
            ModalSpec::new("Nouvelle clé", ModalSize::Medium).icon(icon::KEY),
            |ui| {
                ui.label(RichText::new(context).weak().small());
                ui.add_space(4.0);
                egui::Grid::new("new_key")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Nom");
                        ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("user:42"));
                        ui.end_row();
                        ui.label("Type");
                        egui::ComboBox::from_id_salt("new_key_type")
                            .selected_text(type_label(&self.kind).to_string())
                            .show_ui(ui, |ui| {
                                for t in TYPES {
                                    let text = format!("{} {}", type_icon(&t), type_label(&t));
                                    ui.selectable_value(&mut self.kind, t, text);
                                }
                            });
                        ui.end_row();
                        ui.label("Valeur");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.value)
                                .code_editor()
                                .desired_rows(4)
                                .hint_text(value_hint(&self.kind)),
                        );
                        ui.end_row();
                        ui.label("TTL (s)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.ttl)
                                .desired_width(80.0)
                                .hint_text("persistante"),
                        );
                        ui.end_row();
                    });
            },
            error.as_deref().map(|e| (MessageKind::Error, e)),
            &[ModalButton::primary("Créer", ())],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_names_are_checked() {
        assert!(check_collection_name("orders").is_ok());
        assert!(check_collection_name("fs.chunks").is_ok());
        assert!(check_collection_name("").is_err());
        assert!(check_collection_name("a$b").is_err());
        assert!(check_collection_name("system.views").is_err());
        let p = Prompt::new(
            "m",
            PromptKind::NewCollection { db: "shop".into() },
            " orders ".into(),
        );
        assert_eq!(
            p.task(),
            Ok(NoSqlTask::CreateCollection {
                db: "shop".into(),
                name: "orders".into()
            })
        );
    }

    #[test]
    fn expiration_prompts_take_seconds_or_nothing() {
        let p = |text: &str| {
            Prompt::new(
                "r",
                PromptKind::Expire {
                    db: 2,
                    key: b"k".to_vec(),
                },
                text.into(),
            )
            .task()
        };
        assert_eq!(
            p("60"),
            Ok(NoSqlTask::EditKey {
                db: 2,
                key: b"k".to_vec(),
                edits: vec![KeyEdit::Expire(Some(60))]
            })
        );
        assert_eq!(
            p(""),
            Ok(NoSqlTask::EditKey {
                db: 2,
                key: b"k".to_vec(),
                edits: vec![KeyEdit::Expire(None)]
            })
        );
        assert!(p("bientôt").is_err());
    }

    #[test]
    fn new_keys_need_a_name_and_a_valid_ttl() {
        let mut k = NewKey::new("r", 0);
        assert_eq!(k.task(), Err("Le nom est obligatoire".into()));
        k.name = "user:1".into();
        k.kind = RedisType::List;
        k.value = "[not json".into();
        // The JSON is checked before the engine sees it (when the name is
        // accepted).
        if let Err(e) = k.task() {
            assert!(!e.is_empty());
        }
        assert!(!value_hint(&RedisType::Hash).is_empty());
    }
}
