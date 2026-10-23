//! NoSQL export / import in the `export_import` modal (plan 13, Task 5):
//! the shown result to a JSON / JSON Lines / CSV file (`x`, F6), MongoDB
//! collections ↔ files of a folder (`<base>.<collection>.<ext>`) and a
//! Redis database ↔ a JSON / CSV file (`E` / `I`, Shift+F6 / Shift+F7).

use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use ratatui::{backend::Backend, Terminal};

use crate::engine::models::{DatabaseType, QueryResult, SchemaInfo, NULL_CELL};
use crate::engine::nosql::mongo::{self, CollectionRef};
use crate::engine::nosql::redis;
use crate::engine::nosql::{is_absent, ImportOptions, Source, TransferFormat};
use crate::engine::services::export_import::{batch_file_name, export_to_csv_quoting_empty};
use crate::tui::{AppState, DialogMode};

/// What the modal transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The shown result to a file.
    Results,
    Export,
    Import,
}

/// A field of the modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Format,
    /// A file (result, Redis) or a folder (MongoDB).
    Path,
    /// Redis database.
    Database,
    /// Redis SCAN pattern of an export.
    Pattern,
    /// MongoDB filter of an export (one collection only).
    Filter,
    Upsert,
    KeepStrings,
    ParseDates,
    /// MongoDB collections.
    List,
}

/// State of the NoSQL transfer modal.
#[derive(Clone, Debug, PartialEq)]
pub struct Transfer {
    pub kind: Kind,
    pub engine: DatabaseType,
    pub format: TransferFormat,
    pub path: String,
    pub path_cursor: usize,
    /// Redis pattern or MongoDB filter.
    pub text: String,
    pub text_cursor: usize,
    /// Redis databases (`db0`…) and the chosen one.
    pub databases: Vec<String>,
    pub database: usize,
    /// MongoDB collections: (database, collection, selected).
    pub items: Vec<(String, String, bool)>,
    pub selected: usize,
    pub scroll: usize,
    pub options: ImportOptions,
    pub active: Field,
    /// (done, total, current) while running.
    pub progress: Option<(usize, usize, String)>,
}

/// File name of a free text (a collection, a key).
fn file_stem(name: &str) -> String {
    crate::engine::services::export_import::safe_file_stem(name)
}

impl Transfer {
    fn base(kind: Kind, engine: DatabaseType) -> Self {
        Self {
            kind,
            engine,
            format: TransferFormat::Json,
            path: String::new(),
            path_cursor: 0,
            text: String::new(),
            text_cursor: 0,
            databases: Vec::new(),
            database: 0,
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            options: ImportOptions::default(),
            active: Field::Format,
            progress: None,
        }
    }

    /// Export of the shown result, named after its collection / key.
    pub fn results(engine: DatabaseType, source: Option<&Source>) -> Self {
        let stem = match source {
            Some(Source::Collection { coll, .. }) => file_stem(&coll.to_string()),
            Some(Source::Key { key, .. }) => file_stem(&redis::display_bytes(key)),
            None => "export".to_string(),
        };
        let mut t = Self::base(Kind::Results, engine);
        t.path = format!("{stem}.json");
        t.path_cursor = t.path.len();
        t.active = Field::Path;
        t
    }

    /// Export / import of a whole connection: its collections (MongoDB,
    /// the one under the cursor selected) or its databases (Redis, the one
    /// under the cursor chosen). `hidden` tells the hidden databases.
    pub fn batch(
        kind: Kind,
        engine: DatabaseType,
        schemas: &[SchemaInfo],
        hidden: &dyn Fn(&str) -> bool,
        cursor: Option<(&str, Option<&str>)>,
    ) -> Self {
        let mut t = Self::base(kind, engine.clone());
        if engine == DatabaseType::Redis {
            t.databases = schemas
                .iter()
                .filter(|s| nosql_db(&s.name).is_some())
                .map(|s| s.name.clone())
                .collect();
            t.database = cursor
                .and_then(|(db, _)| t.databases.iter().position(|d| d == db))
                .unwrap_or(0);
            match kind {
                Kind::Import => {
                    t.active = Field::Path;
                }
                _ => {
                    t.text = "*".into();
                    t.text_cursor = 1;
                    t.path = t.default_redis_file();
                    t.path_cursor = t.path.len();
                }
            }
        } else {
            t.items = schemas
                .iter()
                .filter(|s| !hidden(&s.name))
                .flat_map(|s| {
                    s.tables.iter().map(move |c| {
                        let chosen = cursor == Some((s.name.as_str(), Some(c.as_str())));
                        (s.name.clone(), c.clone(), chosen)
                    })
                })
                .collect();
            t.selected = t.items.iter().position(|i| i.2).unwrap_or(0);
            t.path = ".".into();
            t.path_cursor = 1;
            t.active = Field::List;
        }
        t
    }

    fn default_redis_file(&self) -> String {
        let db = self
            .databases
            .get(self.database)
            .map_or("db0", String::as_str);
        format!("{db}.{}", self.format.extension())
    }

    /// The formats offered: JSON Lines for MongoDB collections only.
    pub fn formats(&self) -> Vec<TransferFormat> {
        match (self.kind, &self.engine) {
            (Kind::Results, _) | (_, DatabaseType::Redis) => {
                vec![TransferFormat::Json, TransferFormat::Csv]
            }
            _ => TransferFormat::ALL.to_vec(),
        }
    }

    /// The fields shown, in order.
    pub fn fields(&self) -> Vec<Field> {
        let redis = self.engine == DatabaseType::Redis;
        match self.kind {
            Kind::Results => vec![Field::Format, Field::Path],
            Kind::Export if redis => {
                vec![Field::Format, Field::Database, Field::Pattern, Field::Path]
            }
            Kind::Export => vec![Field::Format, Field::Path, Field::Filter, Field::List],
            Kind::Import if redis => vec![Field::Database, Field::Path, Field::Upsert],
            Kind::Import => {
                let mut f = vec![Field::Format, Field::Path, Field::Upsert];
                if self.format == TransferFormat::Csv {
                    f.push(Field::KeepStrings);
                    if !self.options.keep_strings {
                        f.push(Field::ParseDates);
                    }
                }
                f.push(Field::List);
                f
            }
        }
    }

    /// Whether the path is a folder (MongoDB collections).
    pub fn path_is_folder(&self) -> bool {
        self.kind != Kind::Results && self.engine == DatabaseType::MongoDB
    }

    /// Next / previous format; a file keeps its name with the new extension.
    pub fn cycle_format(&mut self, forward: bool) {
        let formats = self.formats();
        let i = formats.iter().position(|f| *f == self.format).unwrap_or(0);
        let n = formats.len();
        let old = self.format;
        self.format = formats[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
        if !self.path_is_folder() {
            let suffix = format!(".{}", old.extension());
            if let Some(stem) = self.path.strip_suffix(&suffix) {
                self.path = format!("{stem}.{}", self.format.extension());
                self.path_cursor = self.path.len();
            }
        }
        if !self.options.keep_strings && self.format != TransferFormat::Csv {
            self.options.parse_dates = false;
        }
    }

    fn cycle_database(&mut self, forward: bool) {
        let n = self.databases.len();
        if n == 0 {
            return;
        }
        let default_file = self.default_redis_file();
        self.database = if forward {
            (self.database + 1) % n
        } else {
            (self.database + n - 1) % n
        };
        // A default file name follows the database.
        if self.kind == Kind::Export && self.path == default_file {
            self.path = self.default_redis_file();
            self.path_cursor = self.path.len();
        }
    }

    /// The Redis database chosen.
    pub fn redis_db(&self) -> Option<u8> {
        nosql_db(self.databases.get(self.database)?)
    }

    /// The file of collection `(db, coll)` in the folder.
    pub fn collection_file(&self, db: &str, coll: &str) -> PathBuf {
        Path::new(self.path.trim()).join(batch_file_name(db, coll, self.format.extension()))
    }

    /// The collections chosen and their files.
    pub fn chosen(&self) -> Vec<(CollectionRef, PathBuf)> {
        self.items
            .iter()
            .filter(|i| i.2)
            .map(|(db, coll, _)| {
                (
                    CollectionRef {
                        db: db.clone(),
                        coll: coll.clone(),
                    },
                    self.collection_file(db, coll),
                )
            })
            .collect()
    }

    /// Import: select the collections whose file is in the folder (`files`:
    /// its file names), and add the collections of files `<db>.<coll>.<ext>`
    /// of a known database that don't exist yet.
    pub fn match_files(&mut self, files: &[String]) {
        if self.kind != Kind::Import || self.engine != DatabaseType::MongoDB {
            return;
        }
        let ext = self.format.extension();
        let has = |name: &str| files.iter().any(|f| f == name);
        for item in &mut self.items {
            item.2 = has(&batch_file_name(&item.0, &item.1, ext));
        }
        let dbs: Vec<String> = {
            let mut d: Vec<String> = self.items.iter().map(|i| i.0.clone()).collect();
            d.dedup();
            d
        };
        for f in files {
            let Some(stem) = f.strip_suffix(&format!(".{ext}")) else {
                continue;
            };
            let Some((db, coll)) = stem.split_once('.') else {
                continue;
            };
            let known = self.items.iter().any(|i| i.0 == db && i.1 == coll);
            if !known && !coll.is_empty() && dbs.iter().any(|d| d == db) {
                self.items.push((db.to_string(), coll.to_string(), true));
            }
        }
    }

    /// `match_files` with the files of the folder on disk.
    fn rescan(&mut self) {
        if self.kind != Kind::Import || self.engine != DatabaseType::MongoDB {
            return;
        }
        let files: Vec<String> = std::fs::read_dir(self.path.trim())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        self.match_files(&files);
    }

    fn move_field(&mut self, forward: bool) {
        let fields = self.fields();
        let i = fields.iter().position(|f| *f == self.active).unwrap_or(0);
        let n = fields.len();
        self.active = fields[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
    }

    /// The text field being edited and its cursor.
    fn text_mut(&mut self) -> Option<(&mut String, &mut usize)> {
        match self.active {
            Field::Path => Some((&mut self.path, &mut self.path_cursor)),
            Field::Pattern | Field::Filter => Some((&mut self.text, &mut self.text_cursor)),
            _ => None,
        }
    }

    fn toggle_option(&mut self) {
        match self.active {
            Field::Upsert => self.options.upsert = !self.options.upsert,
            Field::KeepStrings => self.options.keep_strings = !self.options.keep_strings,
            Field::ParseDates => self.options.parse_dates = !self.options.parse_dates,
            _ => {}
        }
    }

    /// One key: `Some(true)` starts the transfer, `Some(false)` cancels.
    pub fn key(&mut self, key: KeyCode) -> Option<bool> {
        match key {
            KeyCode::Esc => return Some(false),
            KeyCode::Enter if self.active == Field::Path && self.path_is_folder() => {
                // Enter on the folder (import): look for its files first.
                if self.kind == Kind::Import {
                    self.rescan();
                    self.active = Field::List;
                    return None;
                }
                return Some(true);
            }
            KeyCode::Enter => return Some(true),
            KeyCode::Tab => self.move_field(true),
            KeyCode::BackTab => self.move_field(false),
            _ => {}
        }
        match (self.active, key) {
            (Field::Format, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) => {
                self.cycle_format(key != KeyCode::Left);
                self.rescan();
            }
            (Field::Database, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) => {
                self.cycle_database(key != KeyCode::Left);
            }
            (
                Field::Upsert | Field::KeepStrings | Field::ParseDates,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' '),
            ) => {
                self.toggle_option();
                if self.active == Field::KeepStrings && self.options.keep_strings {
                    self.options.parse_dates = false;
                }
            }
            (Field::List, KeyCode::Down | KeyCode::Char('j')) if !self.items.is_empty() => {
                self.selected = (self.selected + 1) % self.items.len();
            }
            (Field::List, KeyCode::Up | KeyCode::Char('k')) if !self.items.is_empty() => {
                self.selected = self.selected.checked_sub(1).unwrap_or(self.items.len() - 1);
            }
            (Field::List, KeyCode::Char(' ')) => {
                if let Some(i) = self.items.get_mut(self.selected) {
                    i.2 = !i.2;
                }
            }
            (Field::List, KeyCode::Char('a')) => self.items.iter_mut().for_each(|i| i.2 = true),
            (Field::List, KeyCode::Char('n')) => self.items.iter_mut().for_each(|i| i.2 = false),
            (Field::Format | Field::Database | Field::Upsert | Field::KeepStrings, _)
            | (Field::ParseDates | Field::List, _) => {}
            (_, KeyCode::Down) => self.move_field(true),
            (_, KeyCode::Up) => self.move_field(false),
            _ => {
                if let Some((text, cursor)) = self.text_mut() {
                    edit_text(text, cursor, key);
                }
            }
        }
        None
    }

    /// Why the transfer can't start, if it can't.
    pub fn check(&self) -> Result<(), String> {
        if self.path.trim().is_empty() {
            return Err(if self.path_is_folder() {
                "Dossier requis".into()
            } else {
                "Fichier requis".into()
            });
        }
        if self.engine == DatabaseType::MongoDB
            && self.kind != Kind::Results
            && !self.items.iter().any(|i| i.2)
        {
            return Err("Choisir au moins une collection (Espace)".into());
        }
        if self.engine == DatabaseType::Redis && self.kind != Kind::Results {
            if self.redis_db().is_none() {
                return Err("Aucune base Redis".into());
            }
            if self.kind == Kind::Import
                && TransferFormat::from_path(Path::new(self.path.trim())).is_none()
            {
                return Err("Extension attendue : .json, .jsonl ou .csv".into());
            }
        }
        Ok(())
    }
}

/// The index of a Redis database listed as `db3`.
fn nosql_db(name: &str) -> Option<u8> {
    super::redis_db(name)
}

/// Edit a one-line text field.
fn edit_text(text: &mut String, cursor: &mut usize, key: KeyCode) {
    *cursor = (*cursor).min(text.len());
    match key {
        KeyCode::Char(c) => {
            text.insert(*cursor, c);
            *cursor += c.len_utf8();
        }
        KeyCode::Backspace if *cursor > 0 => {
            let prev = crate::tui::prev_char_boundary(text, *cursor);
            text.remove(prev);
            *cursor = prev;
        }
        KeyCode::Delete if *cursor < text.len() => {
            text.remove(*cursor);
        }
        KeyCode::Left => *cursor = crate::tui::prev_char_boundary(text, *cursor),
        KeyCode::Right => *cursor = crate::tui::next_char_boundary(text, *cursor),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = text.len(),
        _ => {}
    }
}

/// The shown result as a file: the documents of a collection (canonical
/// extended JSON, types kept), else the rows as objects; CSV as `export_to_csv_quoting_empty` (a
/// missing field empty, like null).
pub fn results_file(
    result: &QueryResult,
    docs: Option<&[bson::Document]>,
    format: TransferFormat,
) -> String {
    if format == TransferFormat::Csv {
        let mut r = result.clone();
        for row in &mut r.rows {
            for cell in row.iter_mut() {
                if is_absent(cell) {
                    *cell = NULL_CELL.to_string();
                }
            }
        }
        return export_to_csv_quoting_empty(&r);
    }
    let values: Vec<serde_json::Value> = match docs {
        Some(docs) if docs.len() == result.rows.len() => docs
            .iter()
            .map(|d| bson::Bson::Document(d.clone()).into_canonical_extjson())
            .collect(),
        _ => result
            .rows
            .iter()
            .map(|row| super::row_object(result, row))
            .collect(),
    };
    match format {
        TransferFormat::JsonLines => values.iter().map(|v| format!("{v}\n")).collect::<String>(),
        _ => serde_json::to_string_pretty(&values).unwrap_or_default() + "\n",
    }
}

/// Open the modal for the shown result (`x`, F6).
pub fn open_results(state: &mut AppState) {
    let Some(engine) = super::engine(state) else {
        return;
    };
    if state.query_result.is_none() {
        state.set_status("Aucun résultat à exporter : exécuter une commande d'abord");
        return;
    }
    state.nosql.transfer = Some(Transfer::results(engine, state.nosql.source.as_ref()));
    state.dialog_mode = DialogMode::NoSqlTransfer;
}

/// Open the export (`E`, Shift+F6) or import (`I`, F7, Shift+F7) modal of
/// the connection.
pub fn open_batch(state: &mut AppState, kind: Kind) {
    let Some(engine) = super::engine(state) else {
        return;
    };
    if state.schemas.is_empty() {
        state.set_status("Aucune base : se connecter d'abord");
        return;
    }
    let cursor_db = state
        .schemas
        .get(state.selected_schema)
        .map(|s| s.name.clone());
    let cursor_coll = (engine == DatabaseType::MongoDB)
        .then(|| super::selected_collection(state))
        .flatten()
        .map(|c| c.coll);
    let cursor = cursor_db.as_deref().map(|db| (db, cursor_coll.as_deref()));
    let hidden = |s: &str| state.is_schema_hidden(s);
    let mut t = Transfer::batch(kind, engine, &state.schemas, &hidden, cursor);
    t.rescan();
    state.nosql.transfer = Some(t);
    state.dialog_mode = DialogMode::NoSqlTransfer;
}

/// Keys of the modal; returns whether to start the transfer.
pub fn key(state: &mut AppState, key: KeyCode) -> bool {
    let Some(t) = state.nosql.transfer.as_mut() else {
        state.close_dialog();
        return false;
    };
    match t.key(key) {
        Some(true) => match t.check() {
            Ok(()) => true,
            Err(e) => {
                state.set_status(e);
                false
            }
        },
        Some(false) => {
            state.close_dialog();
            state.set_status("Annulé");
            false
        }
        None => false,
    }
}

/// Draw the progress of the transfer.
fn draw_progress<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    progress: (usize, usize, String),
) {
    if let Some(t) = state.nosql.transfer.as_mut() {
        t.progress = Some(progress);
    }
    let registry = crate::tui::ui::ClickableRegistry::new();
    let _ = terminal.draw(|f| crate::tui::ui::render_ui(f, state, &registry));
}

/// Run the transfer of the modal.
pub async fn run<B: Backend>(terminal: &mut Terminal<B>, state: &mut AppState) {
    let Some(t) = state.nosql.transfer.clone() else {
        state.close_dialog();
        return;
    };
    let status = match t.kind {
        Kind::Results => export_results(state, &t),
        _ => match state.connection.clone() {
            Some(conn) => match (t.kind, &t.engine) {
                (Kind::Export, DatabaseType::MongoDB) => {
                    export_collections(terminal, state, &conn, &t).await
                }
                (Kind::Import, DatabaseType::MongoDB) => {
                    import_collections(terminal, state, &conn, &t).await
                }
                (Kind::Export, _) => export_keys(terminal, state, &conn, &t).await,
                _ => import_keys(terminal, state, &conn, &t).await,
            },
            None => "Not connected".to_string(),
        },
    };
    state.close_dialog();
    state.set_status(status);
}

fn export_results(state: &AppState, t: &Transfer) -> String {
    let Some(result) = state.query_result.as_ref() else {
        return "Aucun résultat".into();
    };
    let docs = match &state.nosql.source {
        Some(Source::Collection { docs, .. }) => Some(docs.as_slice()),
        _ => None,
    };
    let content = results_file(result, docs, t.format);
    match std::fs::write(t.path.trim(), content) {
        Ok(()) => format!(
            "{} ligne(s) exportée(s) vers {} ({})",
            result.rows.len(),
            t.path.trim(),
            t.format.label()
        ),
        Err(e) => format!("Écriture de {} : {e}", t.path.trim()),
    }
}

/// Summary of a batch: done, then the first errors.
fn summary(done: String, errors: &[String]) -> String {
    if errors.is_empty() {
        done
    } else {
        format!("{done} — {} erreur(s) : {}", errors.len(), errors[0])
    }
}

async fn export_collections<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    conn: &crate::engine::db::DatabaseConnection,
    t: &Transfer,
) -> String {
    let client = match conn.mongo() {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    if let Err(e) = std::fs::create_dir_all(t.path.trim()) {
        return format!("Dossier {} : {e}", t.path.trim());
    }
    let chosen = t.chosen();
    // The filter applies to a single collection only.
    let filter = if chosen.len() == 1 { t.text.trim() } else { "" };
    let (mut docs, mut errors) = (0, Vec::new());
    for (i, (coll, path)) in chosen.iter().enumerate() {
        draw_progress(terminal, state, (i, chosen.len(), coll.to_string()));
        match mongo::export_collection(client, coll, filter, path, t.format, |_, _, _| {}).await {
            Ok(n) => docs += n,
            Err(e) => errors.push(format!("{coll} : {e}")),
        }
    }
    summary(
        format!(
            "{docs} document(s) exporté(s) de {} collection(s) vers {}",
            chosen.len() - errors.len(),
            t.path.trim()
        ),
        &errors,
    )
}

async fn import_collections<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    conn: &crate::engine::db::DatabaseConnection,
    t: &Transfer,
) -> String {
    let client = match conn.mongo() {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    let chosen = t.chosen();
    let (mut done, mut errors) = (0, Vec::new());
    for (i, (coll, path)) in chosen.iter().enumerate() {
        draw_progress(terminal, state, (i, chosen.len(), coll.to_string()));
        if !path.exists() {
            errors.push(format!("{coll} : fichier absent ({})", path.display()));
            continue;
        }
        match mongo::import_collection(client, coll, path, t.format, &t.options, |_, _, _| {}).await
        {
            Ok(stats) => {
                done += stats.succeeded();
                errors.extend(stats.errors.into_iter().map(|e| format!("{coll} : {e}")));
            }
            Err(e) => errors.push(format!("{coll} : {e}")),
        }
    }
    summary(format!("{done} document(s) importé(s)"), &errors)
}

async fn export_keys<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    conn: &crate::engine::db::DatabaseConnection,
    t: &Transfer,
) -> String {
    let (client, db) = match (conn.redis(), t.redis_db()) {
        (Ok(c), Some(db)) => (c, db),
        (Err(e), _) => return e.to_string(),
        (_, None) => return "Aucune base Redis".into(),
    };
    draw_progress(terminal, state, (0, 1, format!("db{db}")));
    let pattern = if t.text.trim().is_empty() {
        "*"
    } else {
        t.text.trim()
    };
    let path = Path::new(t.path.trim());
    match redis::export_keys(client, db, pattern, path, t.format, |_, _, _| {}).await {
        Ok(n) => format!("{n} clé(s) de db{db} exportée(s) vers {}", path.display()),
        Err(e) => format!("Export de db{db} : {e}"),
    }
}

async fn import_keys<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    conn: &crate::engine::db::DatabaseConnection,
    t: &Transfer,
) -> String {
    let (client, db) = match (conn.redis(), t.redis_db()) {
        (Ok(c), Some(db)) => (c, db),
        (Err(e), _) => return e.to_string(),
        (_, None) => return "Aucune base Redis".into(),
    };
    let path = Path::new(t.path.trim());
    let Some(format) = TransferFormat::from_path(path) else {
        return "Extension attendue : .json, .jsonl ou .csv".into();
    };
    draw_progress(terminal, state, (0, 1, format!("db{db}")));
    match redis::import_keys(client, db, path, format, &t.options, |_, _, _| {}).await {
        Ok(stats) => summary(
            format!("{} clé(s) importée(s) dans db{db}", stats.succeeded()),
            &stats.errors,
        ),
        Err(e) => format!("Import dans db{db} : {e}"),
    }
}
