//! Table structure dialog: list the columns of a table and add / modify /
//! rename / drop one, on an explicit connection.

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::db::utils::display_table_name;
use crate::engine::models::{Column, DatabaseType};
use crate::engine::services::{ColumnDefinition, SchemaModification};

use super::super::app::App;
use super::super::status;
use super::super::theme::{ACCENT, ERROR};
use super::Dialog;

/// One pending change of the structure dialog.
pub enum ColumnEdit {
    Add(ColumnDefinition),
    Modify(ColumnDefinition),
    Rename { old: String, new: String },
    Drop(String),
}

impl ColumnEdit {
    pub fn to_modification(&self, table: &str) -> SchemaModification {
        let table_name = table.to_string();
        match self {
            ColumnEdit::Add(column) => SchemaModification::AddColumn {
                table_name,
                column: column.clone(),
            },
            ColumnEdit::Modify(column) => SchemaModification::ModifyColumn {
                table_name,
                column: column.clone(),
            },
            ColumnEdit::Rename { old, new } => SchemaModification::RenameColumn {
                table_name,
                old_name: old.clone(),
                new_name: new.clone(),
            },
            ColumnEdit::Drop(name) => SchemaModification::DropColumn {
                table_name,
                column_name: name.clone(),
            },
        }
    }

    /// Whether the form can be applied (names and types filled in).
    fn is_complete(&self) -> bool {
        match self {
            ColumnEdit::Add(c) | ColumnEdit::Modify(c) => {
                !c.name.trim().is_empty() && !c.data_type.trim().is_empty()
            }
            ColumnEdit::Rename { old, new } => !new.trim().is_empty() && new != old,
            ColumnEdit::Drop(_) => true,
        }
    }
}

pub struct StructureDialog {
    pub connection: String,
    /// Qualified, quoted table name (key of `Session.details`).
    pub table: String,
    /// Plain table name, for the heading.
    pub title: String,
    pub edit: Option<ColumnEdit>,
    /// A modification is running.
    pub applying: bool,
    /// Last failed modification.
    pub error: Option<String>,
}

/// Open the structure of `table` (qualified) on `connection`, loading its
/// details if they aren't cached yet.
pub fn open(app: &mut App, connection: &str, table: &str, title: &str) {
    let Some(session) = app.sessions.get_mut(connection) else {
        return;
    };
    if !session.details.contains_key(table) && !session.loading.contains_key(table) {
        session.load_details(&mut app.worker, connection, table);
    }
    app.dialog = Some(Dialog::Structure(StructureDialog {
        connection: connection.to_string(),
        table: table.to_string(),
        title: title.to_string(),
        edit: None,
        applying: false,
        error: None,
    }));
}

/// Outcome of a schema change on `name`: report it, refresh the table's
/// details (dialog and explorer) and its open data tabs.
pub fn on_applied(app: &mut App, name: String, table: String, outcome: Result<String, String>) {
    if let Some(Dialog::Structure(d)) = &mut app.dialog {
        if d.connection == name && d.table == table {
            d.applying = false;
            match &outcome {
                Ok(_) => {
                    d.edit = None;
                    d.error = None;
                }
                Err(e) => d.error = Some(e.clone()),
            }
        }
    }
    match outcome {
        Ok(_) => {
            app.success(format!(
                "Structure de {} modifiée",
                display_table_name(&table)
            ));
            if let Some(s) = app.sessions.get_mut(&name) {
                s.details.remove(&table);
                // Supersedes a request still running (it predates the change).
                s.load_details(&mut app.worker, &name, &table);
            }
            app.reload_data_tabs(&name, Some(&table));
        }
        Err(e) => app.error(format!(
            "{} : modification de structure impossible : {e}",
            display_table_name(&table)
        )),
    }
}

/// `"main"."users"` → `users`.
fn definition(c: &Column) -> ColumnDefinition {
    ColumnDefinition {
        name: c.name.clone(),
        data_type: c.type_name.clone(),
        nullable: c.nullable,
        is_primary_key: c.is_primary_key,
        default_value: None,
    }
}

fn column_fields(ui: &mut egui::Ui, c: &mut ColumnDefinition, name_editable: bool) {
    egui::Grid::new("structure_fields")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Nom");
            ui.add_enabled(name_editable, egui::TextEdit::singleline(&mut c.name));
            ui.end_row();
            ui.label("Type");
            ui.text_edit_singleline(&mut c.data_type);
            ui.end_row();
            ui.label("Nullable");
            ui.checkbox(&mut c.nullable, "");
            ui.end_row();
            if name_editable {
                ui.label("Défaut");
                let mut d = c.default_value.clone().unwrap_or_default();
                if ui
                    .add(egui::TextEdit::singleline(&mut d).hint_text("expression SQL"))
                    .changed()
                {
                    c.default_value = (!d.trim().is_empty()).then_some(d);
                }
                ui.end_row();
            }
        });
}

/// Draws the dialog; returns false when it should close.
pub fn ui(app: &mut App, ui: &mut egui::Ui, d: &mut StructureDialog) -> bool {
    let mut keep = true;
    ui.set_min_width(560.0);
    ui.heading(format!("{} Structure de {}", icon::COLUMNS, d.title));
    ui.label(RichText::new(&d.connection).weak().small());
    ui.add_space(6.0);

    let session = app.sessions.get(&d.connection);
    let sqlite = session.is_some_and(|s| s.config.db_type == DatabaseType::SQLite);
    match session.map(|s| (s.details.get(&d.table), s.loading.contains_key(&d.table))) {
        None => {
            ui.colored_label(ERROR, status::not_connected(&d.connection));
        }
        Some((None, _)) | Some((_, true)) => {
            ui.spinner();
        }
        Some((Some(Err(e)), _)) => {
            ui.colored_label(ERROR, e);
        }
        Some((Some(Ok(details)), _)) => {
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    columns_grid(ui, &details.columns, sqlite, d.applying, &mut d.edit);
                });
            if ui
                .add_enabled(
                    !d.applying,
                    egui::Button::new(format!("{} Ajouter une colonne", icon::PLUS)),
                )
                .clicked()
            {
                d.edit = Some(ColumnEdit::Add(ColumnDefinition::default()));
                d.error = None;
            }
        }
    }

    let mut apply = false;
    if let Some(edit) = &mut d.edit {
        ui.separator();
        let danger = matches!(edit, ColumnEdit::Drop(_));
        match edit {
            ColumnEdit::Add(c) => {
                ui.strong("Nouvelle colonne");
                column_fields(ui, c, true);
            }
            ColumnEdit::Modify(c) => {
                ui.strong(format!("Modifier {}", c.name));
                column_fields(ui, c, false);
            }
            ColumnEdit::Rename { old, new } => {
                ui.strong(format!("Renommer {old}"));
                ui.text_edit_singleline(new);
            }
            ColumnEdit::Drop(name) => {
                ui.label(
                    RichText::new(format!(
                        "{} Supprimer la colonne {name} et toutes ses données ?",
                        icon::WARNING
                    ))
                    .color(ERROR),
                );
            }
        }
        let ready = !d.applying && edit.is_complete();
        ui.horizontal(|ui| {
            let button = if danger {
                egui::Button::new(RichText::new("Supprimer").color(egui::Color32::WHITE))
                    .fill(ERROR)
            } else {
                egui::Button::new("Appliquer")
            };
            if ui.add_enabled(ready, button).clicked() {
                apply = true;
            }
            if ui.button("Annuler").clicked() {
                d.edit = None;
                d.error = None;
            }
            if d.applying {
                ui.spinner();
            }
        });
    }
    if let Some(e) = &d.error {
        ui.add(egui::Label::new(RichText::new(e).color(ERROR)).wrap());
    }
    if apply {
        if let (Some(edit), Some(session)) = (&d.edit, app.sessions.get(&d.connection)) {
            let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
            d.applying = true;
            d.error = None;
            app.worker.apply_schema(
                d.connection.clone(),
                conn,
                edit.to_modification(&d.table),
                db_type,
            );
        }
    }

    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("Fermer").clicked() {
            keep = false;
        }
    });
    keep
}

/// Column list with per-column actions (they set `edit`).
fn columns_grid(
    ui: &mut egui::Ui,
    columns: &[Column],
    sqlite: bool,
    applying: bool,
    edit: &mut Option<ColumnEdit>,
) {
    egui::Grid::new("structure_columns")
        .striped(true)
        .num_columns(5)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for h in ["Colonne", "Type", "Nullable", "Clé", ""] {
                ui.strong(h);
            }
            ui.end_row();
            for c in columns {
                ui.label(&c.name);
                ui.label(&c.type_name);
                ui.label(if c.nullable { "oui" } else { "non" });
                if c.is_primary_key {
                    ui.label(RichText::new(icon::KEY).color(ACCENT))
                        .on_hover_text("Clé primaire");
                } else {
                    ui.label("");
                }
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!applying, |ui| {
                        let modify = ui
                            .add_enabled(!sqlite, egui::Button::new(icon::PENCIL_SIMPLE).small())
                            .on_hover_text("Modifier le type")
                            .on_disabled_hover_text("SQLite ne permet pas de modifier une colonne");
                        if modify.clicked() {
                            *edit = Some(ColumnEdit::Modify(definition(c)));
                        }
                        if ui
                            .small_button(icon::TEXT_AA)
                            .on_hover_text("Renommer")
                            .clicked()
                        {
                            *edit = Some(ColumnEdit::Rename {
                                old: c.name.clone(),
                                new: c.name.clone(),
                            });
                        }
                        if ui
                            .small_button(RichText::new(icon::TRASH).color(ERROR))
                            .on_hover_text("Supprimer")
                            .clicked()
                        {
                            *edit = Some(ColumnEdit::Drop(c.name.clone()));
                        }
                    });
                });
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_builds_the_right_modification() {
        let col = ColumnDefinition {
            name: "age".into(),
            data_type: "INT".into(),
            ..Default::default()
        };
        let m = ColumnEdit::Rename {
            old: "age".into(),
            new: "years".into(),
        }
        .to_modification("t");
        assert!(matches!(
            m,
            SchemaModification::RenameColumn { ref table_name, ref old_name, ref new_name }
                if table_name == "t" && old_name == "age" && new_name == "years"
        ));
        let m = ColumnEdit::Add(col.clone()).to_modification("t");
        assert!(
            matches!(m, SchemaModification::AddColumn { ref column, .. } if column.name == "age")
        );
        let m = ColumnEdit::Drop("age".into()).to_modification("t");
        assert!(matches!(
            m,
            SchemaModification::DropColumn { ref column_name, .. } if column_name == "age"
        ));
        let m = ColumnEdit::Modify(col).to_modification("\"main\".\"t\"");
        assert!(matches!(
            m,
            SchemaModification::ModifyColumn { ref table_name, ref column }
                if table_name == "\"main\".\"t\"" && column.data_type == "INT"
        ));
    }

    #[test]
    fn incomplete_edits_cannot_be_applied() {
        let blank = ColumnDefinition::default();
        assert!(!ColumnEdit::Add(blank).is_complete());
        let same = ColumnEdit::Rename {
            old: "a".into(),
            new: "a".into(),
        };
        assert!(!same.is_complete());
        let empty = ColumnEdit::Rename {
            old: "a".into(),
            new: " ".into(),
        };
        assert!(!empty.is_complete());
        assert!(ColumnEdit::Drop("a".into()).is_complete());
    }
}
