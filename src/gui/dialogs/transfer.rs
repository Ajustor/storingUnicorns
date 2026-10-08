//! Export / import / truncate dialogs, each bound to an explicit connection:
//! they work whatever the active tab's connection is.

use std::path::{Path, PathBuf};

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::db::utils::display_qualified;
use crate::engine::models::{QueryResult, SchemaInfo};
use crate::engine::ops::transfer::qualified;
use crate::engine::services::export_import::{BatchExportState, ExportFormat};

use super::super::app::App;
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

    /// Import: tick exactly the tables with a `<table>.csv` in `dir`
    /// (`exists` checks a path). No-op without a folder.
    pub fn select_matching_files(&mut self, exists: impl Fn(&Path) -> bool) {
        let Some(dir) = &self.dir else { return };
        for (_, t, sel) in &mut self.items {
            *sel = exists(&dir.join(format!("{}.csv", BatchExportState::clean_table_name(t))));
        }
    }
}

/// Export format chosen by the file extension (`.sql` → INSERTs, else CSV).
pub fn format_for(path: &Path) -> ExportFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("sql") => ExportFormat::SqlInsert,
        _ => ExportFormat::Csv,
    }
}

/// Last segment of a possibly qualified, quoted table name, for file names.
fn file_stem(table: &str) -> String {
    let last = table.rsplit('.').next().unwrap_or(table);
    let stem = BatchExportState::clean_table_name(last);
    if stem.is_empty() {
        "export".into()
    } else {
        stem
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
        app.error(format!("{connection} n'est pas connectée"));
        return;
    };
    let (conn, quotes) = (session.conn.clone(), session.quotes);
    let Some(path) = rfd::FileDialog::new()
        .add_filter("CSV", &["csv"])
        .pick_file()
    else {
        return;
    };
    app.progress = Some((0, 0, "lignes".into()));
    app.worker.import_csv(
        connection.to_string(),
        conn,
        table.to_string(),
        path,
        quotes,
    );
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

/// Confirmation of a truncate; returns false when the dialog should close.
pub fn confirm_truncate_ui(
    app: &mut App,
    ui: &mut egui::Ui,
    connection: &str,
    tables: &[String],
) -> bool {
    let mut keep = true;
    ui.heading(format!("{} Vider les tables", icon::ERASER));
    ui.label(RichText::new(connection).weak().small());
    ui.add_space(4.0);
    ui.label(
        RichText::new("Toutes les lignes des tables suivantes seront supprimées :").color(ERROR),
    );
    for t in tables.iter().take(12) {
        ui.label(format!("• {}", display_qualified(t)));
    }
    if tables.len() > 12 {
        ui.label(format!("… et {} autres", tables.len() - 12));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let danger =
            egui::Button::new(RichText::new("Vider").color(egui::Color32::WHITE)).fill(ERROR);
        if ui.add(danger).clicked() {
            match app.sessions.conn(connection) {
                Some(conn) => {
                    app.progress = Some((0, tables.len(), "tables".into()));
                    app.worker
                        .truncate_tables(connection.to_string(), conn, tables.to_vec());
                }
                None => app.error(format!("{connection} n'est pas connectée")),
            }
            keep = false;
        }
        if ui.button("Annuler").clicked() {
            keep = false;
        }
    });
    keep
}

/// Batch export / import / truncate; returns false when it should close.
pub fn batch_ui(app: &mut App, ui: &mut egui::Ui, b: &mut BatchDialog) -> bool {
    let mut keep = true;
    ui.set_max_width(560.0);
    ui.heading(match b.kind {
        BatchKind::Export => format!("{} Export par lot", icon::EXPORT),
        BatchKind::Import => format!("{} Import par lot (CSV)", icon::DOWNLOAD_SIMPLE),
        BatchKind::Truncate => format!("{} Vidage par lot", icon::ERASER),
    });
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
        ui.horizontal(|ui| {
            ui.label("Format :");
            ui.radio_value(&mut b.format, ExportFormat::Csv, "CSV");
            ui.radio_value(&mut b.format, ExportFormat::SqlInsert, "SQL INSERT");
        });
    }
    if b.kind == BatchKind::Import {
        ui.label(RichText::new("Un fichier <table>.csv par table, cochée s'il existe.").weak());
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
    let selected = b.selected();
    let ready = !selected.is_empty() && (b.kind == BatchKind::Truncate || b.dir.is_some());
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let label = match b.kind {
            BatchKind::Export => "Exporter",
            BatchKind::Import => "Importer",
            BatchKind::Truncate => "Vider…",
        };
        let label = format!("{label} ({})", selected.len());
        let button = if b.kind == BatchKind::Truncate {
            egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(ERROR)
        } else {
            egui::Button::new(label)
        };
        if ui.add_enabled(ready, button).clicked() {
            start_batch(app, b, selected);
            keep = false;
        }
        if ui.button("Annuler").clicked() {
            keep = false;
        }
    });
    keep
}

/// Start the batch operation of `b` on its connection (a truncate asks for
/// confirmation first).
fn start_batch(app: &mut App, b: &BatchDialog, selected: Vec<(String, String)>) {
    let Some(session) = app.sessions.get(&b.connection) else {
        app.error(format!("{} n'est pas connectée", b.connection));
        return;
    };
    let (conn, quotes) = (session.conn.clone(), session.quotes);
    let name = b.connection.clone();
    let n = selected.len();
    match (b.kind, b.dir.clone()) {
        (BatchKind::Truncate, _) => {
            let tables = selected
                .iter()
                .map(|(s, t)| qualified(s, t, quotes))
                .collect();
            open_truncate(app, &name, tables);
        }
        (BatchKind::Export, Some(dir)) => {
            app.progress = Some((0, n, "tables".into()));
            app.worker
                .export_tables(name, conn, selected, dir, b.format, quotes);
        }
        (BatchKind::Import, Some(dir)) => {
            app.progress = Some((0, n, "tables".into()));
            app.worker.import_tables(name, conn, selected, dir, quotes);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
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
        b.select_matching_files(|p| p == Path::new("dump").join("c.csv"));
        assert_eq!(b.selected(), vec![pair("aux", "c")]);
    }

    #[test]
    fn format_from_extension() {
        assert_eq!(format_for(Path::new("x.sql")), ExportFormat::SqlInsert);
        assert_eq!(format_for(Path::new("x.SQL")), ExportFormat::SqlInsert);
        assert_eq!(format_for(Path::new("x.CSV")), ExportFormat::Csv);
        assert_eq!(format_for(Path::new("x")), ExportFormat::Csv);
    }
}
