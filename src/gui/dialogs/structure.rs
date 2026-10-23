//! Table structure dialog: list the columns of a table and add / modify /
//! rename / drop one, on an explicit connection.

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::db::utils::{display_qualified, display_table_name};
use crate::engine::models::{
    Column, DatabaseType, ForeignKeyInfo, IncomingForeignKey, TableDetails,
};
use crate::engine::services::{ColumnDefinition, SchemaModification};

use super::super::app::App;
use super::super::fk;
use super::super::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use super::super::status;
use super::super::theme::ERROR;
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
    /// A linked table clicked (its name as the key gives it): its
    /// structure replaces this one.
    pub goto: Option<String>,
}

/// Open the structure of `table` (qualified) on `connection`, loading its
/// details if they aren't cached yet.
pub fn open(app: &mut App, connection: &str, table: &str, title: &str) {
    let Some(session) = app.sessions.get_mut(connection) else {
        return;
    };
    if session.is_nosql() {
        let db = session.config.db_type.clone();
        app.error(format!("Structure indisponible pour {db}"));
        return;
    }
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
        goto: None,
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
    /// Open the structure of the linked table in `goto`.
    Goto,
}

/// What the body lists.
enum Content<'a> {
    NotConnected,
    Loading,
    Error(&'a str),
    Columns {
        details: &'a TableDetails,
        sqlite: bool,
    },
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
        Some((Some(Ok(details)), _)) => Content::Columns { details, sqlite },
    };
    let response = draw(ui, d, content);
    with_goto(response, d)
}

/// Why the links to other tables are disabled: following one replaces
/// the dialog, which would drop the pending column change.
fn links_lock(d: &StructureDialog) -> Option<&'static str> {
    if d.applying {
        Some("Modification en cours…")
    } else if d.edit.is_some() {
        Some("Appliquez ou annulez la modification en cours d'abord")
    } else {
        None
    }
}

/// A link clicked in the body (`goto`) answers like a button, unless the
/// links are locked (`links_lock`).
fn with_goto(
    response: ModalResponse<StructureAction>,
    d: &mut StructureDialog,
) -> ModalResponse<StructureAction> {
    if links_lock(d).is_some() {
        d.goto = None;
    }
    match (&response, &d.goto) {
        (ModalResponse::None, Some(_)) => ModalResponse::Button(StructureAction::Goto),
        _ => response,
    }
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
        Content::Columns { details, sqlite } => {
            egui::ScrollArea::vertical()
                .id_salt("structure_columns_scroll")
                .max_height(320.0)
                .show(ui, |ui| {
                    let lock = links_lock(d);
                    let mut grid = ColumnsGrid {
                        details,
                        sqlite,
                        applying: d.applying,
                        lock,
                        edit: &mut d.edit,
                        goto: &mut d.goto,
                    };
                    grid.show(ui);
                    links_sections(ui, details, lock, &mut d.goto);
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
        StructureAction::Goto => {
            let Some(name) = d.goto.take() else {
                return true;
            };
            let Some(session) = app.sessions.get(&d.connection) else {
                return true;
            };
            let key = fk::target_key(&session.schemas, session.quotes, &d.table, &name);
            let connection = d.connection.clone();
            // Replaces this dialog.
            open(app, &connection, &key, &display_qualified(&name));
            return false;
        }
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

/// Column list with per-column actions (they set `edit`) and links (they
/// set `goto`).
struct ColumnsGrid<'a> {
    details: &'a TableDetails,
    sqlite: bool,
    applying: bool,
    /// Why the links are disabled, if they are.
    lock: Option<&'static str>,
    edit: &'a mut Option<ColumnEdit>,
    goto: &'a mut Option<String>,
}

impl ColumnsGrid<'_> {
    fn show(&mut self, ui: &mut egui::Ui) {
        let color = fk::link_color(ui.visuals().dark_mode);
        let details = self.details;
        let fks = &details.foreign_keys;
        egui::Grid::new("structure_columns")
            .striped(true)
            .num_columns(6)
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                for h in ["Colonne", "Type", "Nullable", "Clé", "Lien", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for c in &details.columns {
                    let link = fk::link_of(fks, &c.name);
                    ui.label(&c.name);
                    ui.label(&c.type_name);
                    ui.label(if c.nullable { "oui" } else { "non" });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if c.is_primary_key {
                            fk::pk_badge(ui);
                        }
                        if let Some(l) = &link {
                            fk::fk_badge(ui, &l.tooltip());
                        }
                    });
                    match &link {
                        Some(l) => {
                            let text = format!("→ {}", l.target);
                            let link = egui::Link::new(RichText::new(text).color(color));
                            if ui
                                .add_enabled(self.lock.is_none(), link)
                                .on_hover_text(format!(
                                    "{} · ouvrir la structure de {}",
                                    l.tooltip(),
                                    l.table
                                ))
                                .on_disabled_hover_text(self.lock.unwrap_or_default())
                                .clicked()
                            {
                                *self.goto = Some(fks[l.fk].ref_table.clone());
                            }
                        }
                        None => {
                            ui.label("");
                        }
                    }
                    self.actions(ui, c);
                    ui.end_row();
                }
            });
    }

    fn actions(&mut self, ui: &mut egui::Ui, c: &Column) {
        let (sqlite, edit) = (self.sqlite, &mut *self.edit);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.applying, |ui| {
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
    }
}

/// `(a, b) → pairs (x, y)`: an outgoing key, its columns grouped.
fn outgoing_line(fk: &ForeignKeyInfo) -> String {
    let refs: Vec<&str> = fk
        .ref_columns
        .iter()
        .map(String::as_str)
        .filter(|c| !c.is_empty())
        .collect();
    let target = display_qualified(&fk.ref_table);
    let target = if refs.is_empty() {
        target
    } else {
        format!("{target} ({})", refs.join(", "))
    };
    format!("({}) → {target}", fk.columns.join(", "))
}

/// `orders (user_id) → id`: an incoming key.
fn incoming_line(fk: &IncomingForeignKey) -> String {
    format!("{} → {}", fk::incoming_label(fk), fk.ref_columns.join(", "))
}

/// The keys, each once: "Clés étrangères" (outgoing) and "Référencée par"
/// (incoming). A click opens the linked table's structure.
fn links_sections(
    ui: &mut egui::Ui,
    d: &TableDetails,
    lock: Option<&'static str>,
    goto: &mut Option<String>,
) {
    let color = fk::link_color(ui.visuals().dark_mode);
    let mut link = |ui: &mut egui::Ui, text: String, table: &str| {
        let link = egui::Link::new(RichText::new(format!("{} {text}", icon::LINK)).color(color));
        if ui
            .add_enabled(lock.is_none(), link)
            .on_hover_text(format!(
                "Ouvrir la structure de {}",
                display_qualified(table)
            ))
            .on_disabled_hover_text(lock.unwrap_or_default())
            .clicked()
        {
            *goto = Some(table.to_string());
        }
    };
    if !d.foreign_keys.is_empty() {
        ui.add_space(8.0);
        ui.strong("Clés étrangères");
        for fk in &d.foreign_keys {
            link(ui, outgoing_line(fk), &fk.ref_table);
        }
    }
    ui.add_space(8.0);
    ui.strong("Référencée par");
    if let Some(e) = &d.incoming_error {
        ui.colored_label(ERROR, e);
    } else if d.incoming.is_empty() {
        ui.label(RichText::new("Aucune table").weak());
    }
    for fk in &d.incoming {
        link(ui, incoming_line(fk), &fk.table);
    }
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
            goto: None,
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
        let details = TableDetails {
            columns: vec![Column {
                name: "id".into(),
                type_name: "INTEGER".into(),
                nullable: false,
                is_primary_key: true,
            }],
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut last = ModalResponse::None;
        for events in [vec![], vec![key(k)]] {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let content = Content::Columns {
                    details: &details,
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
    fn keys_are_listed_once_with_their_columns_grouped() {
        let out = ForeignKeyInfo {
            name: "fk".into(),
            columns: vec!["a".into(), "b".into()],
            ref_table: "public.pairs".into(),
            ref_columns: vec!["x".into(), "y".into()],
        };
        assert_eq!(outgoing_line(&out), "(a, b) → public.pairs (x, y)");
        let implicit = ForeignKeyInfo {
            ref_columns: vec![String::new()],
            columns: vec!["owner".into()],
            ..out
        };
        assert_eq!(outgoing_line(&implicit), "(owner) → public.pairs");
        let incoming = IncomingForeignKey {
            name: String::new(),
            table: "main.orders".into(),
            columns: vec!["user_id".into()],
            ref_columns: vec!["id".into()],
        };
        assert_eq!(incoming_line(&incoming), "main.orders (user_id) → id");
    }

    #[test]
    fn a_clicked_link_answers_like_a_button() {
        let mut d = dialog(None);
        assert_eq!(with_goto(ModalResponse::None, &mut d), ModalResponse::None);
        d.goto = Some("users".into());
        assert_eq!(
            with_goto(ModalResponse::None, &mut d),
            ModalResponse::Button(StructureAction::Goto)
        );
        // Closing the dialog in the same frame wins.
        assert_eq!(
            with_goto(ModalResponse::Cancel, &mut d),
            ModalResponse::Cancel
        );
    }

    #[test]
    fn links_are_locked_while_a_column_change_is_pending() {
        let mut d = dialog(None);
        assert_eq!(links_lock(&d), None);
        let mut editing = dialog(Some(filled()));
        assert!(links_lock(&editing).is_some());
        editing.goto = Some("users".into());
        assert_eq!(
            with_goto(ModalResponse::None, &mut editing),
            ModalResponse::None,
            "the pending change is kept"
        );
        assert_eq!(editing.goto, None);
        assert!(editing.edit.is_some());
        d.applying = true;
        assert_eq!(links_lock(&d), Some("Modification en cours…"));
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
