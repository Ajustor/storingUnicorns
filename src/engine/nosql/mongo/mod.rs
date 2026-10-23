//! MongoDB engine (plan 13). Task 1 sets the signatures; the bodies are
//! Task 2's: until then, async functions fail with "non implémenté" and pure
//! functions return empty values.
#![allow(unused_variables)] // stubs (plan 13, Task 2)

pub mod shell;

use std::ops::Range;
use std::path::Path;

use anyhow::Result;

use super::{
    not_implemented, CompletionContext, ImportOptions, NoSqlOutcome, NoSqlScope, TransferFormat,
    Warning,
};
use crate::engine::db::utils::split_qualified;
use crate::engine::models::{Column, ConnectionConfig, IndexInfo, QueryResult, SchemaInfo};
use crate::engine::ops::rows::RowChanges;
use crate::engine::ops::transfer::{qualified, ImportStats};
use crate::engine::sql::completion::Completions;

const ENGINE: &str = "MongoDB";

/// An open MongoDB connection (the driver's client is a pool, cheap to clone).
#[derive(Clone)]
pub struct MongoClient {
    pub(crate) client: mongodb::Client,
    /// `ConnectionConfig::database`, `None` when empty (every database).
    pub(crate) default_db: Option<String>,
}

/// A collection: database and collection names, unquoted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CollectionRef {
    pub db: String,
    pub coll: String,
}

impl CollectionRef {
    /// `"db"."coll"` (quoted, as the explorer keys tables: dots inside the
    /// quotes are kept) or bare `db.coll` (cut at the first dot: database
    /// names have none). `None` without a database or a collection.
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        let (db, coll) = if name.starts_with(['"', '`', '[']) {
            let (db, coll) = split_qualified(name);
            (db?, coll)
        } else {
            let (db, coll) = name.split_once('.')?;
            (db.to_string(), coll.to_string())
        };
        (!db.is_empty() && !coll.is_empty()).then_some(Self { db, coll })
    }

    /// `"db"."coll"`: the key of the collection in the sessions' details.
    pub fn key(&self) -> String {
        qualified(&self.db, &self.coll, ('"', '"'))
    }
}

impl std::fmt::Display for CollectionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.db, self.coll)
    }
}

/// Connect and ping; French errors for authentication, TLS and DNS (srv).
pub async fn connect(config: &ConnectionConfig) -> Result<MongoClient> {
    Err(not_implemented(ENGINE))
}

pub async fn test(c: &MongoClient) -> Result<()> {
    Err(not_implemented(ENGINE))
}

/// Databases → collections (`listDatabases`, falling back to the default /
/// authentication database when refused); `system.*` left out.
pub async fn get_tables_by_schema(c: &MongoClient) -> Result<Vec<SchemaInfo>> {
    Err(not_implemented(ENGINE))
}

/// Fields of a sample of `size` documents (`$sample`, else `find().limit`):
/// dotted paths up to 3 levels, type "string|int", nullable when not in
/// every document, `_id` as the primary key.
pub async fn sample_fields(
    c: &MongoClient,
    coll: &CollectionRef,
    size: u32,
) -> Result<Vec<Column>> {
    Err(not_implemented(ENGINE))
}

#[derive(Clone, Debug, PartialEq)]
pub struct MongoIndex {
    pub name: String,
    pub keys: bson::Document,
    pub unique: bool,
    pub sparse: bool,
    /// `expireAfterSeconds`.
    pub ttl: Option<i64>,
    pub partial: Option<bson::Document>,
    pub hidden: bool,
}

pub async fn list_indexes(c: &MongoClient, coll: &CollectionRef) -> Result<Vec<MongoIndex>> {
    Err(not_implemented(ENGINE))
}

/// The explorer's view of an index: columns "a: 1", primary = `_id_`.
pub fn index_info(i: &MongoIndex) -> IndexInfo {
    IndexInfo {
        name: i.name.clone(),
        columns: Vec::new(),
        unique: i.unique,
        primary: false,
    }
}

/// Create an index; returns its name.
pub async fn create_index(
    c: &MongoClient,
    coll: &CollectionRef,
    keys: bson::Document,
    options: bson::Document,
) -> Result<String> {
    Err(not_implemented(ENGINE))
}

pub async fn drop_index(c: &MongoClient, coll: &CollectionRef, name: &str) -> Result<()> {
    Err(not_implemented(ENGINE))
}

/// One page of a collection; texts in shell syntax, "" = `{}`.
#[derive(Clone, Debug, PartialEq)]
pub struct DocQuery {
    pub coll: CollectionRef,
    pub filter: String,
    pub sort: String,
    pub projection: String,
    pub skip: u64,
    pub limit: u32,
}

/// Documents and their grid (`flatten`).
#[derive(Clone, Debug, Default)]
pub struct DocPage {
    pub result: QueryResult,
    pub docs: Vec<bson::Document>,
}

pub async fn find_page(c: &MongoClient, q: &DocQuery) -> Result<DocPage> {
    Err(not_implemented(ENGINE))
}

/// Documents matching the filter (`estimatedDocumentCount` without one).
pub async fn count(c: &MongoClient, q: &DocQuery) -> Result<u64> {
    Err(not_implemented(ENGINE))
}

/// Run `pipeline` (a shell-syntax array) on `coll`.
pub async fn aggregate(
    c: &MongoClient,
    coll: &CollectionRef,
    pipeline: &str,
    max_rows: usize,
) -> Result<DocPage> {
    Err(not_implemented(ENGINE))
}

/// Grid of documents: top-level fields as columns (`_id` first), compact
/// shell syntax for nested values, `NULL_CELL` for null, `ABSENT_CELL` for
/// a missing field.
pub fn flatten(docs: &[bson::Document]) -> QueryResult {
    QueryResult::default()
}

/// The value typed into a cell, keeping the type of `original` (`None`
/// = the field is to be removed, `$unset`).
pub fn cell_to_bson(
    original: Option<&bson::Bson>,
    text: &str,
) -> Result<Option<bson::Bson>, String> {
    Err(not_implemented(ENGINE).to_string())
}

/// One write of a Submit.
#[derive(Clone, Debug, PartialEq)]
pub enum DocChange {
    Set {
        id: bson::Bson,
        field: String,
        value: bson::Bson,
    },
    Unset {
        id: bson::Bson,
        field: String,
    },
    Replace {
        id: bson::Bson,
        doc: bson::Document,
    },
    Insert(bson::Document),
    Delete {
        id: bson::Bson,
    },
}

/// The writes of the grid's pending edits (rows found by `_id`).
pub fn doc_changes(page: &DocPage, changes: &RowChanges) -> Result<Vec<DocChange>, String> {
    Err(not_implemented(ENGINE).to_string())
}

/// The shell command of a change (preview, TUI debug mode).
pub fn change_to_shell(coll: &CollectionRef, c: &DocChange) -> String {
    String::new()
}

/// Apply `changes` one by one; returns how many were applied.
pub async fn apply_changes(
    c: &MongoClient,
    coll: &CollectionRef,
    changes: &[DocChange],
) -> Result<usize> {
    Err(not_implemented(ENGINE))
}

pub async fn create_collection(c: &MongoClient, db: &str, name: &str) -> Result<()> {
    Err(not_implemented(ENGINE))
}

pub async fn drop_collection(c: &MongoClient, coll: &CollectionRef) -> Result<()> {
    Err(not_implemented(ENGINE))
}

pub async fn drop_database(c: &MongoClient, db: &str) -> Result<()> {
    Err(not_implemented(ENGINE))
}

/// Write the documents matching `filter` to `path`; returns their number.
pub async fn export_collection(
    c: &MongoClient,
    coll: &CollectionRef,
    filter: &str,
    path: &Path,
    format: TransferFormat,
    progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<usize> {
    Err(not_implemented(ENGINE))
}

/// Read `path` into `coll` (batches of 1000, unordered).
pub async fn import_collection(
    c: &MongoClient,
    coll: &CollectionRef,
    path: &Path,
    format: TransferFormat,
    opts: &ImportOptions,
    progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<ImportStats> {
    Err(not_implemented(ENGINE))
}

/// CSV grid: nested fields as dotted paths (`a.b`), arrays as relaxed
/// extended JSON, null as an empty field.
pub fn flatten_paths(docs: &[bson::Document]) -> QueryResult {
    QueryResult::default()
}

/// A CSV row as a document (dotted paths → nested objects, empty = absent,
/// types inferred unless `keep_strings`).
pub fn csv_row_to_doc(
    columns: &[String],
    row: &[String],
    opts: &ImportOptions,
) -> Result<bson::Document, String> {
    Err(not_implemented(ENGINE).to_string())
}

/// Byte ranges of the units of a console script.
pub fn units(text: &str) -> Vec<Range<usize>> {
    Vec::new()
}

/// Destructive commands among the units of `range` (all when `None`).
pub fn warnings(text: &str, range: Option<Range<usize>>) -> Vec<Warning> {
    Vec::new()
}

pub async fn run_script(
    c: &MongoClient,
    text: &str,
    range: Option<Range<usize>>,
    scope: &mut NoSqlScope,
    max_rows: usize,
) -> Vec<NoSqlOutcome> {
    vec![NoSqlOutcome {
        outcome: crate::engine::ops::query::StatementOutcome {
            sql: text.to_string(),
            result: Err(not_implemented(ENGINE).to_string()),
            elapsed_ms: 0,
        },
        source: None,
    }]
}

pub fn complete(text: &str, cursor: usize, cx: &CompletionContext) -> Completions {
    Completions::default()
}

/// The script reformatted; `None` when it doesn't parse.
pub fn format(text: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll(db: &str, coll: &str) -> Option<CollectionRef> {
        Some(CollectionRef {
            db: db.into(),
            coll: coll.into(),
        })
    }

    #[test]
    fn collection_names_keep_their_dots() {
        assert_eq!(
            CollectionRef::parse(r#""shop"."fs.chunks""#),
            coll("shop", "fs.chunks")
        );
        assert_eq!(
            CollectionRef::parse("shop.fs.chunks"),
            coll("shop", "fs.chunks")
        );
        assert_eq!(CollectionRef::parse(" shop.users "), coll("shop", "users"));
        assert_eq!(CollectionRef::parse(r#""a""b"."c""#), coll("a\"b", "c"));
        assert_eq!(CollectionRef::parse("users"), None);
        assert_eq!(CollectionRef::parse(r#""users""#), None);
        assert_eq!(CollectionRef::parse("shop."), None);
        assert_eq!(CollectionRef::parse(".users"), None);
    }

    #[test]
    fn key_round_trips_through_parse() {
        let c = coll("shop", "fs.chunks").unwrap();
        assert_eq!(c.key(), r#""shop"."fs.chunks""#);
        assert_eq!(CollectionRef::parse(&c.key()), Some(c.clone()));
        assert_eq!(c.to_string(), "shop.fs.chunks");
    }
}
