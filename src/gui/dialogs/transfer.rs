//! Export / import / truncate dialogs, each bound to an explicit connection:
//! they work whatever the active tab's connection is.

use std::path::{Path, PathBuf};

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::db::utils::{display_qualified, display_table_name};
use crate::engine::models::{QueryResult, SchemaInfo};
use crate::engine::ops::transfer::qualified;
use crate::engine::services::export_import::{find_batch_csv, safe_file_stem, ExportFormat};

use super::super::app::App;
use super::super::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use super::super::theme::ERROR;
use super::Dialog;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BatchKind {
    Export,
    Import,
    Truncate,
}

pub struct BatchDialog {
    pub connection: String,
    pub kind: BatchKind,
    /// `(schema, table, selected)`
    pub items: Vec<(String, String, bool)>,
    pub dir: Option<PathBuf>,
    pub format: ExportFormat,
}

impl BatchDialog {
    pub fn new(connection: &str, kind: BatchKind, schemas: &[SchemaInfo]) -> Self {
        let items = schemas
            .iter()
            .flat_map(|s| {
                s.tables
                    .iter()
                    .map(move |t| (s.name.clone(), t.clone(), false))
            })
            .collect();
        Self {
            connection: connection.to_string(),
            kind,
            items,
            dir: None,
            format: ExportFormat::Csv,
        }
    }

    pub fn set_all(&mut self, on: bool) {
        self.items.iter_mut().for_each(|i| i.2 = on);
    }

    pub fn selected(&self) -> Vec<(String, String)> {
        self.items
            .iter()
            .filter(|i| i.2)
            .map(|(s, t, _)| (s.clone(), t.clone()))
            .collect()
    }

    /// Import: tick exactly the tables with a CSV in `dir` (see
    /// `find_batch_csv`; `exists` checks a path). No-op without a folder.
    pub fn select_matching_files(&mut self, exists: impl Fn(&Path) -> bool) {
        let Some(dir) = &self.dir else { return };
        for (s, t, sel) in &mut self.items {
            *sel = find_batch_csv(dir, s, t, &exists).is_some();
        }
    }

    /// Why the batch can't start yet (shown in the message line and on the
    /// disabled button), `None` when it can.
    pub fn missing(&self) -> Option<&'static str> {
        if self.kind != BatchKind::Truncate && self.dir.is_none() {
            Some("Choisissez un dossier.")
        } else if !self.items.iter().any(|i| i.2) {
            Some("Cochez au moins une table.")
        } else {
            None
        }
    }

    /// The batch to start, `None` while something is missing.
    pub fn start(&self) -> Option<BatchStart> {
        self.missing().is_none().then(|| BatchStart {
            connection: self.connection.clone(),
            kind: self.kind,
            tables: self.selected(),
            dir: self.dir.clone(),
            format: self.format,
        })
    }
}

/// Export format chosen by the file extension (`.sql` → INSERTs, else CSV).
pub fn format_for(path: &Path) -> ExportFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("sql") => ExportFormat::SqlInsert,
        _ => ExportFormat::Csv,
    }
}

/// File name stem for an export of `table` (possibly qualified and quoted):
/// its table part, made safe.
fn file_stem(table: &str) -> String {
    match display_table_name(table) {
        name if name.is_empty() => "export".into(),
        name => safe_file_stem(&name),
    }
}

/// Export a grid's rows: native save dialog, then a background write.
/// `table` names the INSERT target of the SQL format.
pub fn export_result(app: &mut App, connection: &str, result: QueryResult, table: String) {
    let quotes = app
        .sessions
        .get(connection)
        .map_or(('"', '"'), |s| s.quotes);
    let Some(path) = rfd::FileDialog::new()
        .add_filter("CSV", &["csv"])
        .add_filter("SQL INSERT", &["sql"])
        .set_file_name(format!("{}.csv", file_stem(&table)))
        .save_file()
    else {
        return;
    };
    let format = format_for(&path);
    app.info(format!("Export vers {}…", path.display()));
    app.worker
        .export_result(result, format, path, table, quotes);
}

/// "Importer un CSV…" on `table` (qualified) of `connection`.
pub fn open_import(app: &mut App, connection: &str, table: &str) {
    let Some(session) = app.sessions.get(connection) else {
        app.not_connected(connection);
        return;
    };
    let (conn, quotes) = (session.conn.clone(), session.quotes);
    let Some(path) = rfd::FileDialog::new()
        .add_filter("CSV", &["csv"])
        .pick_file()
    else {
        return;
    };
    let op = app.worker.import_csv(
        connection.to_string(),
        conn,
        table.to_string(),
        path,
        quotes,
    );
    app.progress.update(op, 0, 0, "lignes");
}

/// Ask before emptying `tables` (qualified) of `connection`.
pub fn open_truncate(app: &mut App, connection: &str, tables: Vec<String>) {
    app.dialog = Some(Dialog::ConfirmTruncate {
        connection: connection.to_string(),
        tables,
    });
}

/// Batch dialog over every table of `connection`; `only` is ticked first.
pub fn open_batch(app: &mut App, connection: &str, kind: BatchKind, only: Option<(&str, &str)>) {
    let Some(session) = app.sessions.get(connection) else {
        return;
    };
    let mut b = BatchDialog::new(connection, kind, &session.schemas);
    if let Some((schema, table)) = only {
        for (s, t, sel) in &mut b.items {
            *sel = s == schema && t == table;
        }
    }
    app.dialog = Some(Dialog::Batch(b));
}

/// Confirmation of a truncate (standard small modal).
pub fn confirm_truncate_ui(
    ui: &mut egui::Ui,
    connection: &str,
    tables: &[String],
) -> ModalResponse<()> {
    modal::show(
        ui,
        ModalSpec::new("Vider les tables", ModalSize::Small).icon(icon::ERASER),
        |ui| {
            ui.label(RichText::new(connection).weak().small());
            ui.add_space(4.0);
            ui.label(
                RichText::new("Toutes les lignes des tables suivantes seront supprimées :")
                    .color(ERROR),
            );
            for t in tables {
                ui.label(format!("• {}", display_qualified(t)));
            }
        },
        None,
        &[ModalButton::danger("Vider", ())],
    )
}

/// Empty `tables` of `connection` (confirmed).
pub fn truncate(app: &mut App, connection: &str, tables: Vec<String>) {
    match app.sessions.conn(connection) {
        Some(conn) => {
            let n = tables.len();
            let op = app
                .worker
                .truncate_tables(connection.to_string(), conn, tables);
            app.progress.update(op, 0, n, "tables");
        }
        None => app.not_connected(connection),
    }
}

/// Batch export / import / truncate (standard medium modal); the button
/// answers `()` to start it (see [`BatchDialog::start`]).
pub fn batch_ui(ui: &mut egui::Ui, b: &mut BatchDialog) -> ModalResponse<()> {
    let (title, icon) = match b.kind {
        BatchKind::Export => ("Export par lot", icon::EXPORT),
        BatchKind::Import => ("Import par lot (CSV)", icon::DOWNLOAD_SIMPLE),
        BatchKind::Truncate => ("Vidage par lot", icon::ERASER),
    };
    // From the ticks of the last frame: the body may change them.
    let button = batch_button(b);
    let missing = b.missing();
    modal::show(
        ui,
        ModalSpec::new(title, ModalSize::Medium).icon(icon),
        |ui| batch_body(ui, b),
        missing.map(|m| (MessageKind::Info, m)),
        std::slice::from_ref(&button),
    )
}

/// The batch's button: "Exporter (n)" / "Importer (n)" (Primary), "Vider…
/// (n)" (Danger: it asks for a confirmation); disabled while something is
/// missing.
fn batch_button(b: &BatchDialog) -> ModalButton<()> {
    let n = b.items.iter().filter(|i| i.2).count();
    let button = match b.kind {
        BatchKind::Export => ModalButton::primary(format!("Exporter ({n})"), ()),
        BatchKind::Import => ModalButton::primary(format!("Importer ({n})"), ()),
        BatchKind::Truncate => ModalButton::danger(format!("Vider… ({n})"), ()),
    };
    match b.missing() {
        Some(why) => button.enabled(false).hint(why),
        None => button,
    }
}

fn batch_body(ui: &mut egui::Ui, b: &mut BatchDialog) {
    ui.label(RichText::new(&b.connection).weak().small());
    ui.add_space(4.0);
    if b.kind != BatchKind::Truncate {
        ui.horizontal(|ui| {
            ui.label("Dossier :");
            if ui
                .button(format!("{} Choisir…", icon::FOLDER_OPEN))
                .clicked()
            {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    b.dir = Some(d);
                    if b.kind == BatchKind::Import {
                        b.select_matching_files(|p| p.is_file());
                    }
                }
            }
            let dir = b
                .dir
                .as_ref()
                .map_or_else(|| "—".to_string(), |d| d.display().to_string());
            // Long paths are cut, the full one is in the tooltip.
            ui.add(egui::Label::new(RichText::new(&dir).monospace()).truncate())
                .on_hover_text(dir);
        });
    }
    if b.kind == BatchKind::Export {
        ui.label(RichText::new("Un fichier <schéma>.<table>.<ext> par table.").weak());
        ui.horizontal(|ui| {
            ui.label("Format :");
            ui.radio_value(&mut b.format, ExportFormat::Csv, "CSV");
            ui.radio_value(&mut b.format, ExportFormat::SqlInsert, "SQL INSERT");
        });
    }
    if b.kind == BatchKind::Import {
        ui.label(
            RichText::new(
                "Un fichier <schéma>.<table>.csv (ou <table>.csv) par table, cochée s'il existe.",
            )
            .weak(),
        );
    }
    ui.horizontal(|ui| {
        if ui.small_button("Tout").clicked() {
            b.set_all(true);
        }
        if ui.small_button("Rien").clicked() {
            b.set_all(false);
        }
    });
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (schema, table, sel) in &mut b.items {
                ui.checkbox(sel, format!("{schema}.{table}"));
            }
        });
}

/// What a batch dialog starts: its kind on these `(schema, table)` of
/// `connection`, with the folder and format of an export / import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchStart {
    pub connection: String,
    pub kind: BatchKind,
    pub tables: Vec<(String, String)>,
    pub dir: Option<PathBuf>,
    pub format: ExportFormat,
}

/// Start the batch operation `b` on its connection (a truncate asks for
/// confirmation first).
pub fn start_batch(app: &mut App, b: BatchStart) {
    let Some(session) = app.sessions.get(&b.connection) else {
        app.not_connected(&b.connection);
        return;
    };
    let (conn, quotes) = (session.conn.clone(), session.quotes);
    let name = b.connection;
    let selected = b.tables;
    let n = selected.len();
    match (b.kind, b.dir) {
        (BatchKind::Truncate, _) => {
            let tables = selected
                .iter()
                .map(|(s, t)| qualified(s, t, quotes))
                .collect();
            open_truncate(app, &name, tables);
        }
        (BatchKind::Export, Some(dir)) => {
            let op = app
                .worker
                .export_tables(name, conn, selected, dir, b.format, quotes);
            app.progress.update(op, 0, n, "tables");
        }
        (BatchKind::Import, Some(dir)) => {
            let op = app.worker.import_tables(name, conn, selected, dir, quotes);
            app.progress.update(op, 0, n, "tables");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::modal::ButtonKind;
    use super::*;

    fn schemas() -> Vec<SchemaInfo> {
        vec![
            SchemaInfo {
                name: "main".into(),
                tables: vec!["a".into(), "b".into()],
                expanded: true,
            },
            SchemaInfo {
                name: "aux".into(),
                tables: vec!["c".into()],
                expanded: false,
            },
        ]
    }

    fn pair(s: &str, t: &str) -> (String, String) {
        (s.to_string(), t.to_string())
    }

    #[test]
    fn batch_selection_helpers() {
        let mut b = BatchDialog::new("prod", BatchKind::Export, &schemas());
        assert_eq!(b.connection, "prod");
        assert!(b.selected().is_empty());
        b.set_all(true);
        assert_eq!(
            b.selected(),
            vec![pair("main", "a"), pair("main", "b"), pair("aux", "c")]
        );
        b.items[0].2 = false;
        assert_eq!(b.selected(), vec![pair("main", "b"), pair("aux", "c")]);
        b.set_all(false);
        assert!(b.selected().is_empty());
    }

    #[test]
    fn import_ticks_tables_with_a_csv() {
        let mut b = BatchDialog::new("prod", BatchKind::Import, &schemas());
        b.set_all(true);
        // No folder chosen: nothing changes.
        b.select_matching_files(|_| true);
        assert_eq!(b.selected().len(), 3);
        b.dir = Some(PathBuf::from("dump"));
        // `aux` is a Windows device name: the file is `_aux.c.csv`.
        b.select_matching_files(|p| p == Path::new("dump").join("_aux.c.csv"));
        assert_eq!(b.selected(), vec![pair("aux", "c")]);
        // Files of older exports (`<table>.csv`) still count.
        b.select_matching_files(|p| p == Path::new("dump").join("a.csv"));
        assert_eq!(b.selected(), vec![pair("main", "a")]);
    }

    #[test]
    fn batch_starts_once_tables_and_folder_are_chosen() {
        let mut b = BatchDialog::new("prod", BatchKind::Export, &schemas());
        assert_eq!(b.missing(), Some("Choisissez un dossier."));
        assert_eq!(b.start(), None);
        b.dir = Some(PathBuf::from("dump"));
        assert_eq!(b.missing(), Some("Cochez au moins une table."));
        b.items[1].2 = true;
        b.format = ExportFormat::SqlInsert;
        assert_eq!(b.missing(), None);
        assert_eq!(
            b.start(),
            Some(BatchStart {
                connection: "prod".into(),
                kind: BatchKind::Export,
                tables: vec![pair("main", "b")],
                dir: Some(PathBuf::from("dump")),
                format: ExportFormat::SqlInsert,
            })
        );
        // A truncate needs no folder.
        let mut t = BatchDialog::new("prod", BatchKind::Truncate, &schemas());
        assert_eq!(t.missing(), Some("Cochez au moins une table."));
        t.set_all(true);
        assert_eq!(t.start().unwrap().tables.len(), 3);
    }

    #[test]
    fn batch_button_by_kind() {
        let mut b = BatchDialog::new("prod", BatchKind::Truncate, &schemas());
        b.items[0].2 = true;
        let button = batch_button(&b);
        assert_eq!(button.label, "Vider… (1)");
        assert_eq!(button.kind, ButtonKind::Danger, "never on Enter");
        assert!(button.enabled);
        let mut b = BatchDialog::new("prod", BatchKind::Import, &schemas());
        b.set_all(true);
        let button = batch_button(&b);
        assert_eq!(button.label, "Importer (3)");
        assert_eq!(button.kind, ButtonKind::Primary);
        assert!(!button.enabled, "no folder yet");
        assert_eq!(button.hint.as_deref(), Some("Choisissez un dossier."));
        b.dir = Some(PathBuf::from("dump"));
        assert!(batch_button(&b).enabled);
        b.kind = BatchKind::Export;
        assert_eq!(batch_button(&b).label, "Exporter (3)");
    }

    #[test]
    fn batch_modal_starts_with_its_button_or_enter() {
        use crate::gui::ui_test::{click, key, Driver};
        let mut b = BatchDialog::new("prod", BatchKind::Export, &schemas());
        b.items[0].2 = true;
        let mut d = Driver::new();
        // Nothing starts without a folder.
        d.modal(vec![], |ui| batch_ui(ui, &mut b));
        let r = d.modal(vec![key(egui::Key::Enter)], |ui| batch_ui(ui, &mut b));
        assert_eq!(r, ModalResponse::None);
        assert!(!d.button("Exporter").unwrap().enabled);
        b.dir = Some(PathBuf::from("dump"));
        d.modal(vec![], |ui| batch_ui(ui, &mut b));
        let export = d.button("Exporter (1)").expect("Exporter");
        assert!(export.enabled);
        let r = d.modal(click(export.center), |ui| batch_ui(ui, &mut b));
        assert_eq!(r, ModalResponse::Button(()));
        let r = d.modal(vec![key(egui::Key::Enter)], |ui| batch_ui(ui, &mut b));
        assert_eq!(r, ModalResponse::Button(()));
    }

    #[test]
    fn export_file_stem_is_the_safe_table_name() {
        assert_eq!(file_stem("\"main\".\"users\""), "users");
        assert_eq!(file_stem("[dbo].[a/b]"), "a_b");
        assert_eq!(file_stem(""), "export");
    }

    #[test]
    fn format_from_extension() {
        assert_eq!(format_for(Path::new("x.sql")), ExportFormat::SqlInsert);
        assert_eq!(format_for(Path::new("x.SQL")), ExportFormat::SqlInsert);
        assert_eq!(format_for(Path::new("x.CSV")), ExportFormat::Csv);
        assert_eq!(format_for(Path::new("x")), ExportFormat::Csv);
    }
}
