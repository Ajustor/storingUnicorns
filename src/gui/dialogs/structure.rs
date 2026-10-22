//! Table structure dialog: list the columns of a table and add / modify /
//! rename / drop one, on an explicit connection.

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::db::utils::display_table_name;
use crate::engine::models::{Column, DatabaseType};
use crate::engine::services::{ColumnDefinition, SchemaModification};

use super::super::app::App;
use super::super::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
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

/// What the footer asks for (closing the dialog is the modal's `Cancel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureAction {
    /// Run the pending change.
    Apply,
    /// Drop the pending change.
    CloseEdit,
}

/// What the body lists.
enum Content<'a> {
    NotConnected,
    Loading,
    Error(&'a str),
    Columns { columns: &'a [Column], sqlite: bool },
}

/// Draw the dialog in the standard modal; what the user asked for.
pub fn ui(app: &App, ui: &mut egui::Ui, d: &mut StructureDialog) -> ModalResponse<StructureAction> {
    let session = app.sessions.get(&d.connection);
    let sqlite = session.is_some_and(|s| s.config.db_type == DatabaseType::SQLite);
    let content = match session.map(|s| (s.details.get(&d.table), s.loading.contains_key(&d.table)))
    {
        None => Content::NotConnected,
        Some((None, _)) | Some((_, true)) => Content::Loading,
        Some((Some(Err(e)), _)) => Content::Error(e),
        Some((Some(Ok(details)), _)) => Content::Columns {
            columns: &details.columns,
            sqlite,
        },
    };
    draw(ui, d, content)
}

fn draw(
    ui: &mut egui::Ui,
    d: &mut StructureDialog,
    content: Content,
) -> ModalResponse<StructureAction> {
    let title = format!("Structure de {}", d.title);
    let spec = ModalSpec {
        cancel: Some("Fermer"),
        ..ModalSpec::new(&title, ModalSize::Medium).icon(icon::COLUMNS)
    };
    let message = match (&d.error, d.applying) {
        (Some(e), _) => Some((MessageKind::Error, e.clone())),
        (None, true) => Some((MessageKind::Info, "Modification en cours…".to_owned())),
        (None, false) => None,
    };
    let buttons = buttons(d);
    modal::show(
        ui,
        spec,
        |ui| body(ui, d, content),
        message.as_ref().map(|(kind, text)| (*kind, text.as_str())),
        &buttons,
    )
}

/// Footer buttons: those of the pending change, if any.
fn buttons(d: &StructureDialog) -> Vec<ModalButton<StructureAction>> {
    let Some(edit) = &d.edit else {
        return vec![];
    };
    let ready = !d.applying && edit.is_complete();
    let apply = match edit {
        ColumnEdit::Drop(_) => ModalButton::danger("Supprimer", StructureAction::Apply),
        _ => ModalButton::primary("Appliquer", StructureAction::Apply),
    };
    vec![
        ModalButton::secondary("Annuler la modification", StructureAction::CloseEdit),
        apply.enabled(ready),
    ]
}

fn body(ui: &mut egui::Ui, d: &mut StructureDialog, content: Content) {
    ui.label(RichText::new(&d.connection).weak().small());
    ui.add_space(6.0);
    match content {
        Content::NotConnected => {
            ui.colored_label(ERROR, status::not_connected(&d.connection));
        }
        Content::Loading => {
            ui.spinner();
        }
        Content::Error(e) => {
            ui.colored_label(ERROR, e);
        }
        Content::Columns { columns, sqlite } => {
            egui::ScrollArea::vertical()
                .id_salt("structure_columns_scroll")
                .max_height(320.0)
                .show(ui, |ui| {
                    columns_grid(ui, columns, sqlite, d.applying, &mut d.edit);
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

    let Some(edit) = &mut d.edit else {
        return;
    };
    ui.separator();
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
}

/// Do `action`; the dialog stays open.
pub fn act(app: &mut App, d: &mut StructureDialog, action: StructureAction) -> bool {
    match action {
        StructureAction::CloseEdit => {
            d.edit = None;
            d.error = None;
        }
        StructureAction::Apply => {
            let ready = d.edit.as_ref().is_some_and(|e| e.is_complete()) && !d.applying;
            if let (true, Some(edit), Some(session)) =
                (ready, &d.edit, app.sessions.get(&d.connection))
            {
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
    }
    true
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
    use crate::gui::modal::ButtonKind;

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

    fn dialog(edit: Option<ColumnEdit>) -> StructureDialog {
        StructureDialog {
            connection: "Boutique".into(),
            table: "\"main\".\"t\"".into(),
            title: "t".into(),
            edit,
            applying: false,
            error: None,
        }
    }

    fn filled() -> ColumnEdit {
        ColumnEdit::Add(ColumnDefinition {
            name: "remise".into(),
            data_type: "INT".into(),
            ..Default::default()
        })
    }

    #[test]
    fn footer_holds_the_pending_change_s_buttons() {
        assert!(buttons(&dialog(None)).is_empty());
        let b = buttons(&dialog(Some(filled())));
        assert_eq!(
            b.iter()
                .map(|b| (b.label.as_str(), b.kind, b.action, b.enabled))
                .collect::<Vec<_>>(),
            vec![
                (
                    "Annuler la modification",
                    ButtonKind::Secondary,
                    StructureAction::CloseEdit,
                    true
                ),
                (
                    "Appliquer",
                    ButtonKind::Primary,
                    StructureAction::Apply,
                    true
                ),
            ]
        );
        // Incomplete, or already running: cannot be applied.
        let b = buttons(&dialog(Some(ColumnEdit::Add(ColumnDefinition::default()))));
        assert!(!b[1].enabled);
        let mut running = dialog(Some(filled()));
        running.applying = true;
        assert!(!buttons(&running)[1].enabled);
        // Dropping a column is destructive.
        let b = buttons(&dialog(Some(ColumnEdit::Drop("a".into()))));
        assert_eq!(
            (b[1].label.as_str(), b[1].kind),
            ("Supprimer", ButtonKind::Danger)
        );
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Draw `d` over columns, then press `k`: the response.
    fn press(d: &mut StructureDialog, k: egui::Key) -> ModalResponse<StructureAction> {
        let columns = [Column {
            name: "id".into(),
            type_name: "INTEGER".into(),
            nullable: false,
            is_primary_key: true,
        }];
        let ctx = egui::Context::default();
        let mut last = ModalResponse::None;
        for events in [vec![], vec![key(k)]] {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let content = Content::Columns {
                    columns: &columns,
                    sqlite: false,
                };
                last = egui::Modal::new(egui::Id::new("dialog"))
                    .show(ctx, |ui| draw(ui, d, content))
                    .inner;
            });
        }
        last
    }

    #[test]
    fn enter_applies_a_ready_change_but_never_a_drop() {
        let mut d = dialog(Some(filled()));
        assert_eq!(
            press(&mut d, egui::Key::Enter),
            ModalResponse::Button(StructureAction::Apply)
        );
        let mut d = dialog(Some(ColumnEdit::Drop("id".into())));
        assert_eq!(press(&mut d, egui::Key::Enter), ModalResponse::None);
        let mut d = dialog(None);
        assert_eq!(press(&mut d, egui::Key::Enter), ModalResponse::None);
        assert_eq!(press(&mut d, egui::Key::Escape), ModalResponse::Cancel);
    }
}
