//! Export / import / truncate dialogs, each bound to an explicit connection:
//! they work whatever the active tab's connection is.

use std::path::{Path, PathBuf};

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::db::utils::{display_qualified, display_table_name};
use crate::engine::models::{DatabaseType, QueryResult, SchemaInfo};
use crate::engine::nosql::mongo::CollectionRef;
use crate::engine::nosql::{ImportOptions, TransferFormat};
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
    if session.is_nosql() {
        app.error(NOSQL_SQL_TRANSFER);
        return;
    }
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

/// Refusal of the SQL transfers (CSV / SQL INSERT of tables) on MongoDB /
/// Redis, which have their own (`NoSqlBatch`).
pub const NOSQL_SQL_TRANSFER: &str =
    "Export / import SQL indisponible pour MongoDB et Redis : utilisez Exporter… / Importer…";

/// Ask before emptying `tables` (qualified) of `connection`.
pub fn open_truncate(app: &mut App, connection: &str, tables: Vec<String>) {
    if app.sessions.is_nosql(connection) {
        app.error(NOSQL_SQL_TRANSFER);
        return;
    }
    app.dialog = Some(Dialog::ConfirmTruncate {
        connection: connection.to_string(),
        tables,
    });
}

/// Batch dialog over every table of `connection`; `only` is ticked first.
/// MongoDB opens its own export / import (`NoSqlBatch`).
pub fn open_batch(app: &mut App, connection: &str, kind: BatchKind, only: Option<(&str, &str)>) {
    let Some(session) = app.sessions.get(connection) else {
        return;
    };
    if session.is_nosql() {
        let only = only.map(|(db, coll)| CollectionRef {
            db: db.to_string(),
            coll: coll.to_string(),
        });
        match (kind, &session.config.db_type) {
            (BatchKind::Truncate, _) | (_, DatabaseType::Redis) => app.error(NOSQL_SQL_TRANSFER),
            (kind, _) => {
                let b = NoSqlBatch::mongo(connection, kind, &session.schemas, only.as_ref());
                app.dialog = Some(Dialog::NoSqlBatch(Box::new(b)));
            }
        }
        return;
    }
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

/// What a MongoDB / Redis export or import works on.
#[derive(Clone, Debug, PartialEq)]
pub enum NoSqlTarget {
    Mongo {
        /// Every collection, ticked or not (export).
        colls: Vec<(CollectionRef, bool)>,
        /// Import: the files chosen, the collection each goes to (`db.coll`,
        /// editable) and whether it is imported.
        files: Vec<(PathBuf, String, bool)>,
        /// Import from a collection's menu: every file goes there.
        fixed: Option<CollectionRef>,
        /// Database of a file named `<collection>.<ext>`.
        default_db: String,
    },
    /// Redis database `db`: the keys matching `pattern`, one file.
    Redis {
        db: u8,
        pattern: String,
        file: Option<PathBuf>,
    },
}

/// Export / import of a MongoDB connection or a Redis database (JSON, JSON
/// Lines, CSV), in the standard medium modal.
#[derive(Clone, Debug, PartialEq)]
pub struct NoSqlBatch {
    pub connection: String,
    /// `Export` or `Import`.
    pub kind: BatchKind,
    pub format: TransferFormat,
    /// MongoDB export: the folder of the files.
    pub dir: Option<PathBuf>,
    /// MongoDB export of one collection: its filter (shell syntax).
    pub filter: String,
    pub opts: ImportOptions,
    pub target: NoSqlTarget,
}

/// The formats offered for `db`: MongoDB all of them, Redis JSON and CSV.
pub fn nosql_formats(db: &DatabaseType) -> &'static [TransferFormat] {
    match db {
        DatabaseType::Redis => &[TransferFormat::Json, TransferFormat::Csv],
        _ => &TransferFormat::ALL,
    }
}

/// The collection an import file goes to, from its name
/// `<db>.<collection>.<ext>`, else `<collection>.<ext>` in `default_db`.
pub fn collection_for_file(path: &Path, default_db: &str) -> Option<CollectionRef> {
    let stem = path.file_stem()?.to_str()?;
    if let Some(c) = CollectionRef::parse(stem) {
        return Some(c);
    }
    (!default_db.is_empty() && !stem.is_empty()).then(|| CollectionRef {
        db: default_db.to_string(),
        coll: stem.to_string(),
    })
}

/// What a NoSQL export / import starts.
#[derive(Clone, Debug, PartialEq)]
pub enum NoSqlStart {
    ExportCollections {
        connection: String,
        colls: Vec<CollectionRef>,
        dir: PathBuf,
        format: TransferFormat,
        filter: String,
    },
    ImportCollections {
        connection: String,
        files: Vec<(PathBuf, CollectionRef)>,
        opts: ImportOptions,
    },
    ExportKeys {
        connection: String,
        db: u8,
        pattern: String,
        path: PathBuf,
        format: TransferFormat,
    },
    ImportKeys {
        connection: String,
        db: u8,
        path: PathBuf,
        opts: ImportOptions,
    },
}

impl NoSqlBatch {
    /// MongoDB export / import over the collections of `schemas`; `only`
    /// ticked alone (export) or the target of every file (import).
    pub fn mongo(
        connection: &str,
        kind: BatchKind,
        schemas: &[SchemaInfo],
        only: Option<&CollectionRef>,
    ) -> Self {
        let colls = schemas
            .iter()
            .flat_map(|s| {
                s.tables.iter().map(move |t| CollectionRef {
                    db: s.name.clone(),
                    coll: t.clone(),
                })
            })
            .map(|c| {
                let on = only == Some(&c);
                (c, on)
            })
            .collect();
        let default_db = only
            .map(|c| c.db.clone())
            .or_else(|| (schemas.len() == 1).then(|| schemas[0].name.clone()))
            .unwrap_or_default();
        Self {
            connection: connection.to_string(),
            kind,
            format: TransferFormat::Json,
            dir: None,
            filter: String::new(),
            opts: ImportOptions::default(),
            target: NoSqlTarget::Mongo {
                colls,
                files: Vec::new(),
                fixed: (kind == BatchKind::Import).then(|| only.cloned()).flatten(),
                default_db,
            },
        }
    }

    /// Export / import of Redis database `db`.
    pub fn redis(connection: &str, kind: BatchKind, db: u8) -> Self {
        Self {
            connection: connection.to_string(),
            kind,
            format: TransferFormat::Json,
            dir: None,
            filter: String::new(),
            opts: ImportOptions::default(),
            target: NoSqlTarget::Redis {
                db,
                pattern: "*".into(),
                file: None,
            },
        }
    }

    fn db_type(&self) -> DatabaseType {
        match self.target {
            NoSqlTarget::Mongo { .. } => DatabaseType::MongoDB,
            NoSqlTarget::Redis { .. } => DatabaseType::Redis,
        }
    }

    /// The collections ticked for an export.
    pub fn ticked(&self) -> Vec<CollectionRef> {
        match &self.target {
            NoSqlTarget::Mongo { colls, .. } => colls
                .iter()
                .filter(|(_, on)| *on)
                .map(|(c, _)| c.clone())
                .collect(),
            NoSqlTarget::Redis { .. } => Vec::new(),
        }
    }

    /// Add import files, each going to the collection its name gives (or
    /// the fixed one).
    pub fn add_files(&mut self, paths: Vec<PathBuf>) {
        let NoSqlTarget::Mongo {
            files,
            fixed,
            default_db,
            ..
        } = &mut self.target
        else {
            return;
        };
        for path in paths {
            if files.iter().any(|(p, _, _)| *p == path) {
                continue;
            }
            let target = fixed
                .clone()
                .or_else(|| collection_for_file(&path, default_db))
                .map(|c| c.to_string())
                .unwrap_or_default();
            let on = !target.is_empty();
            files.push((path, target, on));
        }
    }

    /// Whether a CSV file is involved (its options are shown).
    pub fn has_csv(&self) -> bool {
        match &self.target {
            NoSqlTarget::Mongo { files, .. } if self.kind == BatchKind::Import => files
                .iter()
                .any(|(p, _, _)| TransferFormat::from_path(p) == Some(TransferFormat::Csv)),
            NoSqlTarget::Redis { file: Some(p), .. } if self.kind == BatchKind::Import => {
                TransferFormat::from_path(p) == Some(TransferFormat::Csv)
            }
            _ => self.kind == BatchKind::Export && self.format == TransferFormat::Csv,
        }
    }

    /// Why it can't start yet, `None` when it can.
    pub fn missing(&self) -> Option<String> {
        match (&self.target, self.kind) {
            (NoSqlTarget::Mongo { .. }, BatchKind::Export) => {
                if self.ticked().is_empty() {
                    Some("Cochez au moins une collection.".into())
                } else if self.dir.is_none() {
                    Some("Choisissez un dossier.".into())
                } else {
                    None
                }
            }
            (NoSqlTarget::Mongo { files, .. }, _) => {
                let chosen: Vec<_> = files.iter().filter(|(_, _, on)| *on).collect();
                if chosen.is_empty() {
                    return Some("Choisissez au moins un fichier.".into());
                }
                chosen.iter().find_map(|(p, target, _)| {
                    let name = p
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    if TransferFormat::from_path(p).is_none() {
                        Some(format!("{name} : format inconnu (json, jsonl, csv)"))
                    } else if CollectionRef::parse(target).is_none() {
                        Some(format!("{name} : collection « base.collection » attendue"))
                    } else {
                        None
                    }
                })
            }
            (NoSqlTarget::Redis { file: None, .. }, _) => Some("Choisissez un fichier.".into()),
            (NoSqlTarget::Redis { file: Some(p), .. }, BatchKind::Import)
                if TransferFormat::from_path(p).is_none() =>
            {
                Some("Format inconnu (json, csv)".into())
            }
            _ => None,
        }
    }

    /// The transfer to start, `None` while something is missing.
    pub fn start(&self) -> Option<NoSqlStart> {
        if self.missing().is_some() {
            return None;
        }
        let connection = self.connection.clone();
        Some(match (&self.target, self.kind) {
            (NoSqlTarget::Mongo { .. }, BatchKind::Export) => {
                let colls = self.ticked();
                // A filter only makes sense on one collection.
                let filter = if colls.len() == 1 {
                    self.filter.trim().to_string()
                } else {
                    String::new()
                };
                NoSqlStart::ExportCollections {
                    connection,
                    colls,
                    dir: self.dir.clone()?,
                    format: self.format,
                    filter,
                }
            }
            (NoSqlTarget::Mongo { files, .. }, _) => NoSqlStart::ImportCollections {
                connection,
                files: files
                    .iter()
                    .filter(|(_, _, on)| *on)
                    .filter_map(|(p, t, _)| Some((p.clone(), CollectionRef::parse(t)?)))
                    .collect(),
                opts: self.opts,
            },
            (NoSqlTarget::Redis { db, pattern, file }, BatchKind::Export) => {
                let path = file.clone()?;
                NoSqlStart::ExportKeys {
                    connection,
                    db: *db,
                    pattern: match pattern.trim() {
                        "" => "*".to_string(),
                        p => p.to_string(),
                    },
                    format: TransferFormat::from_path(&path).unwrap_or(self.format),
                    path,
                }
            }
            (NoSqlTarget::Redis { db, file, .. }, _) => NoSqlStart::ImportKeys {
                connection,
                db: *db,
                path: file.clone()?,
                opts: self.opts,
            },
        })
    }

    fn title(&self) -> String {
        match (&self.target, self.kind) {
            (NoSqlTarget::Redis { db, .. }, BatchKind::Export) => format!("Exporter db{db}"),
            (NoSqlTarget::Redis { db, .. }, _) => format!("Importer dans db{db}"),
            (_, BatchKind::Export) => "Exporter des collections".into(),
            _ => "Importer des collections".into(),
        }
    }

    /// Draw the dialog; `()` starts it (see [`NoSqlBatch::start`]).
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ModalResponse<()> {
        let missing = self.missing();
        let label = match self.kind {
            BatchKind::Export => "Exporter",
            _ => "Importer",
        };
        let button = match &missing {
            Some(why) => ModalButton::primary(label, ())
                .enabled(false)
                .hint(why.clone()),
            None => ModalButton::primary(label, ()),
        };
        let title = self.title();
        let icon = match self.kind {
            BatchKind::Export => icon::EXPORT,
            _ => icon::DOWNLOAD_SIMPLE,
        };
        modal::show(
            ui,
            ModalSpec::new(&title, ModalSize::Medium).icon(icon),
            |ui| self.body(ui),
            missing.as_deref().map(|m| (MessageKind::Info, m)),
            std::slice::from_ref(&button),
        )
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(&self.connection).weak().small());
        ui.add_space(4.0);
        let db_type = self.db_type();
        if self.kind == BatchKind::Export {
            ui.horizontal(|ui| {
                ui.label("Format :");
                for f in nosql_formats(&db_type) {
                    ui.radio_value(&mut self.format, *f, f.label());
                }
            });
        }
        let export = self.kind == BatchKind::Export;
        let format = self.format;
        let mut picked = Vec::new();
        let Self {
            target,
            dir: chosen_dir,
            filter: chosen_filter,
            format: chosen_format,
            ..
        } = self;
        match target {
            NoSqlTarget::Mongo {
                colls,
                files,
                fixed,
                ..
            } => {
                if export {
                    ui.horizontal(|ui| {
                        ui.label("Dossier :");
                        if ui
                            .button(format!("{} Choisir…", icon::FOLDER_OPEN))
                            .clicked()
                        {
                            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                *chosen_dir = Some(d);
                            }
                        }
                        let dir = chosen_dir
                            .as_ref()
                            .map_or_else(|| "—".to_string(), |d| d.display().to_string());
                        ui.add(egui::Label::new(RichText::new(&dir).monospace()).truncate())
                            .on_hover_text(dir);
                    });
                    ui.label(
                        RichText::new(format!(
                            "Un fichier <base>.<collection>.{} par collection.",
                            format.extension()
                        ))
                        .weak(),
                    );
                    let one = colls.iter().filter(|(_, on)| *on).count() == 1;
                    ui.horizontal(|ui| {
                        ui.label("Filtre :");
                        ui.add_enabled(
                            one,
                            egui::TextEdit::singleline(chosen_filter)
                                .code_editor()
                                .hint_text("{ actif: true }"),
                        )
                        .on_disabled_hover_text("Une seule collection cochée");
                    });
                    ui.horizontal(|ui| {
                        if ui.small_button("Tout").clicked() {
                            colls.iter_mut().for_each(|c| c.1 = true);
                        }
                        if ui.small_button("Rien").clicked() {
                            colls.iter_mut().for_each(|c| c.1 = false);
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (c, on) in colls.iter_mut() {
                                ui.checkbox(on, c.to_string());
                            }
                        });
                } else {
                    ui.horizontal(|ui| {
                        if ui
                            .button(format!("{} Choisir des fichiers…", icon::FOLDER_OPEN))
                            .clicked()
                        {
                            picked = rfd::FileDialog::new()
                                .add_filter(
                                    "JSON, JSON Lines, CSV",
                                    &["json", "jsonl", "ndjson", "csv"],
                                )
                                .pick_files()
                                .unwrap_or_default();
                        }
                        match fixed {
                            Some(c) => ui.label(RichText::new(format!("vers {c}")).weak()),
                            None => ui.label(
                                RichText::new(
                                    "Collection déduite du nom <base>.<collection>.<ext>",
                                )
                                .weak(),
                            ),
                        };
                    });
                    egui::Grid::new("nosql_import_files")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            for (path, target, on) in files.iter_mut() {
                                let name = path
                                    .file_name()
                                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                                ui.checkbox(on, name)
                                    .on_hover_text(path.display().to_string());
                                ui.add(
                                    egui::TextEdit::singleline(target)
                                        .hint_text("base.collection")
                                        .desired_width(240.0),
                                );
                                ui.end_row();
                            }
                        });
                }
            }
            NoSqlTarget::Redis { db, pattern, file } => {
                if export {
                    ui.horizontal(|ui| {
                        ui.label("Motif :");
                        ui.add(egui::TextEdit::singleline(pattern).hint_text("*"));
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Fichier :");
                    if ui
                        .button(format!("{} Choisir…", icon::FOLDER_OPEN))
                        .clicked()
                    {
                        let dialog =
                            rfd::FileDialog::new().add_filter("JSON, CSV", &["json", "csv"]);
                        let chosen = if export {
                            dialog
                                .set_file_name(format!("db{db}.{}", format.extension()))
                                .save_file()
                        } else {
                            dialog.pick_file()
                        };
                        if let Some(p) = chosen {
                            if let Some(f) = TransferFormat::from_path(&p) {
                                *chosen_format = f;
                            }
                            *file = Some(p);
                        }
                    }
                    let shown = file
                        .as_ref()
                        .map_or_else(|| "—".to_string(), |p| p.display().to_string());
                    ui.add(egui::Label::new(RichText::new(&shown).monospace()).truncate())
                        .on_hover_text(shown);
                });
            }
        }
        if !picked.is_empty() {
            self.add_files(picked);
        }
        if self.kind == BatchKind::Import {
            ui.add_space(4.0);
            let upsert = match self.target {
                NoSqlTarget::Mongo { .. } => "Remplacer les documents de même _id",
                NoSqlTarget::Redis { .. } => "Écraser les clés existantes",
            };
            ui.checkbox(&mut self.opts.upsert, upsert);
        }
        if self.has_csv() && matches!(self.target, NoSqlTarget::Mongo { .. }) {
            ui.label(
                egui::RichText::new(crate::engine::nosql::CSV_NULL_HINT)
                    .weak()
                    .small(),
            );
        }
        if self.has_csv() && self.kind == BatchKind::Import {
            ui.checkbox(&mut self.opts.keep_strings, "Tout garder en texte");
            ui.add_enabled(
                !self.opts.keep_strings,
                egui::Checkbox::new(&mut self.opts.parse_dates, "Reconnaître les dates ISO"),
            );
        }
    }
}

/// Start the NoSQL transfer `s` on its connection.
pub fn start_nosql(app: &mut App, s: NoSqlStart) {
    let connection = match &s {
        NoSqlStart::ExportCollections { connection, .. }
        | NoSqlStart::ImportCollections { connection, .. }
        | NoSqlStart::ExportKeys { connection, .. }
        | NoSqlStart::ImportKeys { connection, .. } => connection.clone(),
    };
    let Some(conn) = app.sessions.conn(&connection) else {
        app.not_connected(&connection);
        return;
    };
    let (op, total, label) = match s {
        NoSqlStart::ExportCollections {
            connection,
            colls,
            dir,
            format,
            filter,
        } => {
            let n = colls.len();
            let op = app
                .worker
                .export_collections(connection, conn, colls, dir, format, filter);
            (op, n, "documents")
        }
        NoSqlStart::ImportCollections {
            connection,
            files,
            opts,
        } => {
            let n = files.len();
            (
                app.worker.import_collections(connection, conn, files, opts),
                n,
                "documents",
            )
        }
        NoSqlStart::ExportKeys {
            connection,
            db,
            pattern,
            path,
            format,
        } => (
            app.worker
                .export_keys(connection, conn, db, pattern, path, format),
            1,
            "clés",
        ),
        NoSqlStart::ImportKeys {
            connection,
            db,
            path,
            opts,
        } => (
            app.worker.import_keys(connection, conn, db, path, opts),
            1,
            "clés",
        ),
    };
    app.progress.update(op, 0, total, label);
}

#[cfg(test)]
mod tests {
    use super::super::super::modal::ButtonKind;
    use super::*;

    fn coll(db: &str, c: &str) -> CollectionRef {
        CollectionRef {
            db: db.into(),
            coll: c.into(),
        }
    }

    fn mongo_schemas() -> Vec<SchemaInfo> {
        vec![
            SchemaInfo {
                name: "shop".into(),
                tables: vec!["users".into(), "orders".into()],
                expanded: true,
            },
            SchemaInfo {
                name: "logs".into(),
                tables: vec!["events".into()],
                expanded: false,
            },
        ]
    }

    #[test]
    fn formats_offered_by_engine() {
        assert_eq!(nosql_formats(&DatabaseType::MongoDB).len(), 3);
        assert_eq!(
            nosql_formats(&DatabaseType::Redis),
            &[TransferFormat::Json, TransferFormat::Csv]
        );
    }

    #[test]
    fn import_files_go_to_the_collection_of_their_name() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            collection_for_file(&p("dump/shop.users.json"), ""),
            Some(coll("shop", "users"))
        );
        assert_eq!(
            collection_for_file(&p("shop.fs.chunks.jsonl"), ""),
            Some(coll("shop", "fs.chunks"))
        );
        assert_eq!(
            collection_for_file(&p("users.csv"), "shop"),
            Some(coll("shop", "users"))
        );
        assert_eq!(collection_for_file(&p("users.csv"), ""), None);
    }

    #[test]
    fn mongo_export_ticks_one_collection_and_its_filter_only_alone() {
        let users = coll("shop", "users");
        let mut b = NoSqlBatch::mongo("m", BatchKind::Export, &mongo_schemas(), Some(&users));
        assert_eq!(b.ticked(), vec![users.clone()]);
        assert_eq!(b.missing().as_deref(), Some("Choisissez un dossier."));
        b.dir = Some(PathBuf::from("out"));
        b.filter = " { a: 1 } ".into();
        b.format = TransferFormat::Csv;
        assert!(b.has_csv());
        assert_eq!(
            b.start(),
            Some(NoSqlStart::ExportCollections {
                connection: "m".into(),
                colls: vec![users],
                dir: PathBuf::from("out"),
                format: TransferFormat::Csv,
                filter: "{ a: 1 }".into(),
            })
        );
        let NoSqlTarget::Mongo { colls, .. } = &mut b.target else {
            unreachable!()
        };
        colls.iter_mut().for_each(|c| c.1 = true);
        let Some(NoSqlStart::ExportCollections { colls, filter, .. }) = b.start() else {
            panic!("export")
        };
        assert_eq!(colls.len(), 3);
        assert_eq!(filter, "", "no filter over several collections");
    }

    #[test]
    fn mongo_import_checks_formats_and_targets() {
        let mut b = NoSqlBatch::mongo("m", BatchKind::Import, &mongo_schemas(), None);
        assert_eq!(
            b.missing().as_deref(),
            Some("Choisissez au moins un fichier.")
        );
        b.add_files(vec![
            PathBuf::from("shop.users.jsonl"),
            PathBuf::from("notes.txt"),
            PathBuf::from("orders.csv"),
        ]);
        let NoSqlTarget::Mongo { files, .. } = &b.target else {
            unreachable!()
        };
        // Two databases: a bare `orders.csv` has no collection yet.
        assert_eq!(files[0].1, "shop.users");
        assert_eq!(files[2].1, "");
        assert!(!files[2].2, "not ticked without a collection");
        let NoSqlTarget::Mongo { files, .. } = &mut b.target else {
            unreachable!()
        };
        files[1] = (PathBuf::from("notes.txt"), "shop.notes".into(), true);
        assert!(b.missing().unwrap().contains("format inconnu"));
        let NoSqlTarget::Mongo { files, .. } = &mut b.target else {
            unreachable!()
        };
        files[1].2 = false;
        files[2].1 = "shop.orders".into();
        files[2].2 = true;
        assert!(b.has_csv(), "CSV options shown");
        b.opts.upsert = true;
        assert_eq!(
            b.start(),
            Some(NoSqlStart::ImportCollections {
                connection: "m".into(),
                files: vec![
                    (PathBuf::from("shop.users.jsonl"), coll("shop", "users")),
                    (PathBuf::from("orders.csv"), coll("shop", "orders")),
                ],
                opts: ImportOptions {
                    upsert: true,
                    ..Default::default()
                },
            })
        );
    }

    #[test]
    fn import_from_a_collection_menu_targets_it() {
        let users = coll("shop", "users");
        let mut b = NoSqlBatch::mongo("m", BatchKind::Import, &mongo_schemas(), Some(&users));
        b.add_files(vec![PathBuf::from("dump.json")]);
        let NoSqlTarget::Mongo { files, .. } = &b.target else {
            unreachable!()
        };
        assert_eq!(files[0].1, "shop.users");
    }

    #[test]
    fn redis_transfers_need_a_file() {
        let mut b = NoSqlBatch::redis("r", BatchKind::Export, 2);
        assert_eq!(b.missing().as_deref(), Some("Choisissez un fichier."));
        let NoSqlTarget::Redis { pattern, file, .. } = &mut b.target else {
            unreachable!()
        };
        *pattern = " ".into();
        *file = Some(PathBuf::from("db2.csv"));
        assert_eq!(
            b.start(),
            Some(NoSqlStart::ExportKeys {
                connection: "r".into(),
                db: 2,
                pattern: "*".into(),
                path: PathBuf::from("db2.csv"),
                format: TransferFormat::Csv,
            })
        );
        let mut i = NoSqlBatch::redis("r", BatchKind::Import, 2);
        let NoSqlTarget::Redis { file, .. } = &mut i.target else {
            unreachable!()
        };
        *file = Some(PathBuf::from("keys.txt"));
        assert!(i.missing().is_some());
        let NoSqlTarget::Redis { file, .. } = &mut i.target else {
            unreachable!()
        };
        *file = Some(PathBuf::from("keys.csv"));
        assert!(i.has_csv());
        assert!(matches!(
            i.start(),
            Some(NoSqlStart::ImportKeys { db: 2, .. })
        ));
    }

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
