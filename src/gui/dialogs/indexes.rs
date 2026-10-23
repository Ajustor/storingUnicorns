//! « Index… » of a MongoDB collection: its indexes (name, keys, unique,
//! sparse, TTL, partial, hidden), dropped after a confirmation, and the
//! « Nouvel index » form.

use bson::{Bson, Document};
use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::nosql::mongo::{shell, CollectionRef, MongoIndex};
use crate::gui::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use crate::gui::theme::ERROR;
use crate::gui::worker::RunId;

/// What the dialog asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexAnswer {
    /// Create the index of the form.
    Create,
    /// Ask before dropping this index.
    AskDrop(String),
    /// Drop the index asked about.
    Drop(String),
    /// Keep the index asked about.
    KeepIndex,
}

pub struct IndexDialog {
    pub connection: String,
    pub coll: CollectionRef,
    /// `None` while loading.
    pub indexes: Option<Result<Vec<MongoIndex>, String>>,
    /// The listing awaited.
    pub run: Option<RunId>,
    /// « Nouvel index »: keys (`{champ: 1}`), name, options.
    pub keys: String,
    pub name: String,
    pub unique: bool,
    pub sparse: bool,
    pub ttl: String,
    pub partial: String,
    pub hidden: bool,
    pub error: Option<String>,
    /// The index whose drop waits for a confirmation.
    pub dropping: Option<String>,
    /// A creation or a drop is running.
    pub busy: bool,
}

/// The keys of an index as shown: `a: 1, b: -1`.
pub fn keys_label(keys: &Document) -> String {
    keys.iter()
        .map(|(k, v)| {
            let v = match v {
                Bson::Int32(n) => n.to_string(),
                Bson::Int64(n) => n.to_string(),
                Bson::Double(n) => n.to_string(),
                Bson::String(s) => format!("\"{s}\""),
                other => other.to_string(),
            };
            format!("{k}: {v}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The options document of `createIndex` (`name`, `unique`, `sparse`,
/// `expireAfterSeconds`, `partialFilterExpression`, `hidden`).
pub fn index_options(
    name: &str,
    unique: bool,
    sparse: bool,
    hidden: bool,
    ttl: &str,
    partial: Option<Document>,
) -> Result<Document, String> {
    let mut options = Document::new();
    if !name.trim().is_empty() {
        options.insert("name", name.trim());
    }
    if unique {
        options.insert("unique", true);
    }
    if sparse {
        options.insert("sparse", true);
    }
    if hidden {
        options.insert("hidden", true);
    }
    if !ttl.trim().is_empty() {
        let seconds = ttl
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|s| *s >= 0)
            .ok_or("TTL : un nombre de secondes (0 ou plus)")?;
        options.insert("expireAfterSeconds", seconds);
    }
    if let Some(p) = partial.filter(|p| !p.is_empty()) {
        options.insert("partialFilterExpression", p);
    }
    Ok(options)
}

impl IndexDialog {
    pub fn new(connection: &str, coll: CollectionRef) -> Self {
        Self {
            connection: connection.to_string(),
            coll,
            indexes: None,
            run: None,
            keys: String::new(),
            name: String::new(),
            unique: false,
            sparse: false,
            ttl: String::new(),
            partial: String::new(),
            hidden: false,
            error: None,
            dropping: None,
            busy: false,
        }
    }

    /// The keys and options of the index to create.
    pub fn new_index(&self) -> Result<(Document, Document), String> {
        if self.keys.trim().is_empty() {
            return Err("Clés obligatoires, par exemple { email: 1 }".into());
        }
        let keys = shell::parse_document(&self.keys).map_err(|e| format!("Clés : {e}"))?;
        if keys.is_empty() {
            return Err("Clés obligatoires, par exemple { email: 1 }".into());
        }
        let partial = if self.partial.trim().is_empty() {
            None
        } else {
            Some(
                shell::parse_document(&self.partial)
                    .map_err(|e| format!("Filtre partiel : {e}"))?,
            )
        };
        let options = index_options(
            &self.name,
            self.unique,
            self.sparse,
            self.hidden,
            &self.ttl,
            partial,
        )?;
        Ok((keys, options))
    }

    /// The form is cleared once its index exists.
    pub fn created(&mut self) {
        self.keys.clear();
        self.name.clear();
        self.unique = false;
        self.sparse = false;
        self.hidden = false;
        self.ttl.clear();
        self.partial.clear();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> ModalResponse<IndexAnswer> {
        let mut clicked = None;
        let title = format!("Index de {}", self.coll);
        let create = ModalButton::primary("Créer l'index", IndexAnswer::Create)
            .enabled(!self.busy && !self.keys.trim().is_empty());
        let message = self.error.clone();
        let response = modal::show(
            ui,
            ModalSpec::new(&title, ModalSize::Medium).icon(icon::LIST_BULLETS),
            |ui| clicked = self.body(ui),
            message.as_deref().map(|m| (MessageKind::Error, m)),
            std::slice::from_ref(&create),
        );
        match (response, clicked) {
            (ModalResponse::None, Some(a)) => ModalResponse::Button(a),
            (response, _) => response,
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<IndexAnswer> {
        let mut answer = None;
        match &self.indexes {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Chargement…").weak());
                });
            }
            Some(Err(e)) => {
                ui.colored_label(ERROR, e);
            }
            Some(Ok(list)) if list.is_empty() => {
                ui.label(RichText::new("Aucun index").weak());
            }
            Some(Ok(list)) => {
                egui::Grid::new("indexes")
                    .num_columns(4)
                    .striped(true)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        for i in list {
                            ui.label(RichText::new(&i.name).strong());
                            ui.label(RichText::new(keys_label(&i.keys)).monospace());
                            ui.horizontal(|ui| {
                                let mut tags = Vec::new();
                                if i.unique {
                                    tags.push("unique".to_string());
                                }
                                if i.sparse {
                                    tags.push("sparse".to_string());
                                }
                                if let Some(t) = i.ttl {
                                    tags.push(format!("TTL {t} s"));
                                }
                                if i.partial.is_some() {
                                    tags.push("partiel".to_string());
                                }
                                if i.hidden {
                                    tags.push("caché".to_string());
                                }
                                let label =
                                    ui.label(RichText::new(tags.join(" · ")).weak().small());
                                if let Some(p) = &i.partial {
                                    label.on_hover_text(p.to_string());
                                }
                            });
                            // The `_id` index can't be dropped.
                            let primary = i.name == "_id_";
                            if ui
                                .add_enabled(
                                    !primary && !self.busy,
                                    egui::Button::new(RichText::new(icon::TRASH).color(ERROR)),
                                )
                                .on_hover_text("Supprimer l'index")
                                .clicked()
                            {
                                answer = Some(IndexAnswer::AskDrop(i.name.clone()));
                            }
                            ui.end_row();
                        }
                    });
            }
        }
        if let Some(name) = self.dropping.clone() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.colored_label(ERROR, format!("Supprimer l'index « {name} » ?"));
                if ui.button(RichText::new("Supprimer").color(ERROR)).clicked() {
                    answer = Some(IndexAnswer::Drop(name.clone()));
                }
                if ui.button("Garder").clicked() {
                    answer = Some(IndexAnswer::KeepIndex);
                }
            });
        }
        ui.add_space(8.0);
        ui.separator();
        ui.strong("Nouvel index");
        egui::Grid::new("new_index")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Clés");
                ui.add(
                    egui::TextEdit::singleline(&mut self.keys)
                        .code_editor()
                        .hint_text("{ email: 1 }"),
                );
                ui.end_row();
                ui.label("Nom");
                ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("auto"));
                ui.end_row();
                ui.label("Options");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.unique, "Unique");
                    ui.checkbox(&mut self.sparse, "Sparse");
                    ui.checkbox(&mut self.hidden, "Caché");
                });
                ui.end_row();
                ui.label("TTL (s)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.ttl)
                        .desired_width(80.0)
                        .hint_text("aucun"),
                );
                ui.end_row();
                ui.label("Filtre partiel");
                ui.add(
                    egui::TextEdit::singleline(&mut self.partial)
                        .code_editor()
                        .hint_text("{ actif: true }"),
                );
                ui.end_row();
            });
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll() -> CollectionRef {
        CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        }
    }

    #[test]
    fn options_carry_only_what_is_set() {
        assert_eq!(
            index_options("", false, false, false, " ", None),
            Ok(Document::new())
        );
        let o = index_options(
            "by_mail",
            true,
            true,
            true,
            "3600",
            Some(bson::doc! { "a": true }),
        )
        .unwrap();
        assert_eq!(
            o,
            bson::doc! {
                "name": "by_mail",
                "unique": true,
                "sparse": true,
                "hidden": true,
                "expireAfterSeconds": 3600i64,
                "partialFilterExpression": { "a": true },
            }
        );
        assert!(index_options("", false, false, false, "-1", None).is_err());
        assert!(index_options("", false, false, false, "1h", None).is_err());
        // An empty partial filter is no filter.
        assert_eq!(
            index_options("", false, false, false, "", Some(Document::new())),
            Ok(Document::new())
        );
    }

    #[test]
    fn keys_are_required() {
        let d = IndexDialog::new("m", coll());
        assert!(d.new_index().unwrap_err().contains("Clés obligatoires"));
    }

    #[test]
    fn keys_read_like_the_shell() {
        assert_eq!(
            keys_label(&bson::doc! { "a": 1, "b": -1i64 }),
            "a: 1, b: -1"
        );
        assert_eq!(keys_label(&bson::doc! { "t": "text" }), "t: \"text\"");
    }

    #[test]
    fn the_form_is_cleared_once_created() {
        let mut d = IndexDialog::new("m", coll());
        d.keys = "{ a: 1 }".into();
        d.unique = true;
        d.ttl = "60".into();
        d.created();
        assert!(d.keys.is_empty() && !d.unique && d.ttl.is_empty());
    }
}
