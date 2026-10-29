//! MongoDB engine (plan 13): connection, catalog, documents, indexes, the
//! shell-syntax console and JSON / CSV transfer. Also works with Cosmos DB
//! (API MongoDB) and Amazon DocumentDB.

mod complete;
mod flatten;
#[cfg(test)]
mod integration_tests;
pub mod shell;
mod transfer;

use std::ops::Range;
use std::time::{Duration, Instant};

use anyhow::Result;
use bson::{doc, Bson, Document};
use futures_util::TryStreamExt;
use mongodb::options::{ClientOptions, Credential, Tls, TlsOptions};

use super::{CompletionContext, NoSqlOutcome, NoSqlScope, Source, Warning};
use crate::engine::db::utils::split_qualified;
use crate::engine::models::{Column, ConnectionConfig, QueryResult, SchemaInfo};
use crate::engine::ops::query::StatementOutcome;
use crate::engine::ops::transfer::qualified;
use crate::engine::sql::completion::Completions;
use shell::{MongoCommand, UpdateSpec};

#[allow(unused_imports)] // used by the GUI and the TUI (plan 13, Tasks 4 and 5)
pub use flatten::{
    cell_text, cell_to_bson, change_to_shell, doc_changes, fields_of, flatten, index_info,
};
#[allow(unused_imports)] // used by the GUI and the TUI (plan 13, Tasks 4 and 5)
pub use transfer::{csv_row_to_doc, export_collection, flatten_paths, import_collection};

const ENGINE: &str = "MongoDB";
/// Time limit of a count (`maxTimeMS`).
const COUNT_TIME: Duration = Duration::from_secs(30);

/// An open MongoDB connection (the driver's client is a pool, cheap to clone).
#[derive(Clone)]
pub struct MongoClient {
    pub(crate) client: mongodb::Client,
    /// `ConnectionConfig::database` (else the URI's database), `None` when
    /// empty (every database).
    pub(crate) default_db: Option<String>,
}

impl MongoClient {
    pub(crate) fn coll(&self, c: &CollectionRef) -> mongodb::Collection<Document> {
        self.client.database(&c.db).collection(&c.coll)
    }
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

/// A driver error in French: authentication, TLS, DNS (`mongodb+srv`),
/// unreachable server, rights, server errors.
pub(crate) fn mongo_error(e: mongodb::error::Error) -> anyhow::Error {
    use mongodb::error::{ErrorKind, WriteFailure};
    let text = match e.kind.as_ref() {
        ErrorKind::Authentication { message, .. } => {
            format!("{ENGINE} : authentification refusée ({message})")
        }
        ErrorKind::DnsResolve { message, .. } => {
            format!("{ENGINE} : résolution DNS impossible (mongodb+srv) : {message}")
        }
        ErrorKind::InvalidTlsConfig { message, .. } => {
            format!("{ENGINE} : configuration TLS invalide : {message}")
        }
        ErrorKind::InvalidArgument { message, .. } => {
            format!("{ENGINE} : paramètre invalide : {message}")
        }
        ErrorKind::ServerSelection { message, .. } => {
            let lower = message.to_lowercase();
            if ["certificate", "tls", "handshake", "unknownissuer", "x509"]
                .iter()
                .any(|w| lower.contains(w))
            {
                format!("{ENGINE} : échec de la négociation TLS : {message}")
            } else if lower.contains("authentication") || lower.contains("auth error") {
                format!("{ENGINE} : authentification refusée : {message}")
            } else {
                format!("{ENGINE} : serveur injoignable : {message}")
            }
        }
        ErrorKind::Command(c) if c.code == 13 => {
            format!("{ENGINE} : droits insuffisants : {}", c.message)
        }
        ErrorKind::Command(c) if c.code == 18 => {
            format!("{ENGINE} : authentification refusée : {}", c.message)
        }
        ErrorKind::Command(c) => format!("{ENGINE} : {} ({})", c.message, c.code_name),
        ErrorKind::Write(WriteFailure::WriteError(w)) => match w.code {
            11000 => format!("{ENGINE} : clé en double : {}", w.message),
            _ => format!("{ENGINE} : {}", w.message),
        },
        ErrorKind::Io(io) => format!("{ENGINE} : erreur réseau : {io}"),
        _ => format!("{ENGINE} : {e}"),
    };
    anyhow::anyhow!(text)
}

/// The command error code of `e`, if any (26 = namespace not found).
fn error_code(e: &mongodb::error::Error) -> Option<i32> {
    match e.kind.as_ref() {
        mongodb::error::ErrorKind::Command(c) => Some(c.code),
        _ => None,
    }
}

/// The system's root certificates as a PEM file in the app directory
/// (rustls only knows Mozilla's: corporate proxies need these). `None`
/// when they can't be read or written.
fn system_roots_pem() -> Option<std::path::PathBuf> {
    use base64::Engine as _;
    let found = rustls_native_certs::load_native_certs();
    if found.certs.is_empty() {
        return None;
    }
    let mut pem = String::new();
    for cert in &found.certs {
        let b64 = base64::engine::general_purpose::STANDARD.encode(cert.as_ref());
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        for chunk in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
    }
    let path = crate::engine::config::app_dir()
        .ok()?
        .join("mongo-roots.pem");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(pem.as_str()) {
        std::fs::write(&path, pem).ok()?;
    }
    Some(path)
}

/// The URI to connect with: the saved one with the password put back, or
/// one built from host and port.
fn connection_uri(config: &ConnectionConfig) -> String {
    let password = config.password.as_deref().filter(|p| !p.is_empty());
    match config
        .uri
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(uri) => match password {
            Some(p) => crate::engine::presets::uri_with_password(uri, p),
            None => uri.to_string(),
        },
        None => {
            let host = config
                .host
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .unwrap_or("localhost");
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host.to_string()
            };
            format!("mongodb://{host}:{}/", config.port.unwrap_or(27017))
        }
    }
}

/// The driver options of a connection (no I/O except `mongodb+srv` DNS).
async fn client_options(config: &ConnectionConfig) -> Result<ClientOptions> {
    let mut opts = ClientOptions::parse(connection_uri(config))
        .await
        .map_err(mongo_error)?;
    // With a connection string, the user is the URI's.
    let username = config
        .effective_username()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let auth_source = config
        .auth_source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match opts.credential.as_mut() {
        Some(cred) => {
            if let Some(src) = auth_source {
                cred.source = Some(src.to_string());
            }
        }
        None => {
            if let Some(user) = username {
                let mut cred = Credential::default();
                cred.username = Some(user.to_string());
                cred.password = config.password.clone().filter(|p| !p.is_empty());
                cred.source = Some(auth_source.unwrap_or("admin").to_string());
                opts.credential = Some(cred);
            }
        }
    }
    match config.nosql_tls() {
        None => opts.tls = Some(Tls::Disabled),
        Some(verify) => {
            let mut tls = match opts.tls.take() {
                Some(Tls::Enabled(t)) => t,
                _ => TlsOptions::default(),
            };
            if !verify {
                tls.allow_invalid_certificates = Some(true);
            }
            if tls.ca_file_path.is_none() {
                if let Some(ca) = &config.ssl_ca {
                    tls.ca_file_path = Some(ca.clone());
                } else if verify {
                    tls.ca_file_path = system_roots_pem();
                }
            }
            opts.tls = Some(Tls::Enabled(tls));
        }
    }
    opts.server_selection_timeout
        .get_or_insert(Duration::from_secs(10));
    opts.connect_timeout.get_or_insert(Duration::from_secs(10));
    opts.app_name
        .get_or_insert_with(|| "storingUnicorns".into());
    Ok(opts)
}

/// Connect and ping; French errors for authentication, TLS and DNS (srv).
pub async fn connect(config: &ConnectionConfig) -> Result<MongoClient> {
    let opts = client_options(config).await?;
    let default_db = Some(config.database.trim())
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| opts.default_database.clone().filter(|d| !d.is_empty()));
    let client = mongodb::Client::with_options(opts).map_err(mongo_error)?;
    let c = MongoClient { client, default_db };
    test(&c).await?;
    Ok(c)
}

pub async fn test(c: &MongoClient) -> Result<()> {
    c.client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await
        .map_err(mongo_error)?;
    Ok(())
}

/// Case-insensitive name order.
fn sort_names(names: &mut [String]) {
    names.sort_by_key(|n| n.to_lowercase());
}

/// Databases → collections (`listDatabases` with `authorizedDatabases`,
/// falling back to the default database when refused); `system.*` left out.
pub async fn get_tables_by_schema(c: &MongoClient) -> Result<Vec<SchemaInfo>> {
    let mut names = match c
        .client
        .list_database_names()
        .authorized_databases(true)
        .await
    {
        Ok(names) => names,
        Err(e) => match &c.default_db {
            Some(db) => vec![db.clone()],
            None => return Err(mongo_error(e)),
        },
    };
    if let Some(db) = &c.default_db {
        if !names.contains(db) {
            names.push(db.clone());
        }
    }
    sort_names(&mut names);
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let mut tables = c
            .client
            .database(&name)
            .list_collection_names()
            .await
            .unwrap_or_default();
        tables.retain(|t| !t.starts_with("system."));
        sort_names(&mut tables);
        out.push(SchemaInfo {
            name,
            tables,
            expanded: false,
        });
    }
    Ok(out)
}

/// Fields of a sample of `size` documents (`$sample`, else `find().limit`):
/// dotted paths up to 3 levels, type "string|int", nullable when not in
/// every document, `_id` as the primary key.
pub async fn sample_fields(
    c: &MongoClient,
    coll: &CollectionRef,
    size: u32,
) -> Result<Vec<Column>> {
    let collection = c.coll(coll);
    let sampled = match collection
        .aggregate([doc! { "$sample": { "size": i64::from(size) } }])
        .await
    {
        Ok(cursor) => cursor.try_collect::<Vec<Document>>().await,
        Err(e) => Err(e),
    };
    let docs = match sampled {
        Ok(docs) => docs,
        Err(_) => collection
            .find(doc! {})
            .limit(i64::from(size))
            .await
            .map_err(mongo_error)?
            .try_collect()
            .await
            .map_err(mongo_error)?,
    };
    Ok(fields_of(&docs))
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

/// The raw `listIndexes` descriptions (none for a missing collection).
async fn index_docs(c: &MongoClient, coll: &CollectionRef) -> Result<Vec<Document>> {
    let db = c.client.database(&coll.db);
    match db
        .run_cursor_command(doc! { "listIndexes": &coll.coll })
        .await
    {
        Ok(cursor) => cursor.try_collect().await.map_err(mongo_error),
        Err(e) if error_code(&e) == Some(26) => Ok(Vec::new()),
        Err(e) => Err(mongo_error(e)),
    }
}

pub async fn list_indexes(c: &MongoClient, coll: &CollectionRef) -> Result<Vec<MongoIndex>> {
    Ok(index_docs(c, coll)
        .await?
        .iter()
        .map(flatten::index_from_doc)
        .collect())
}

/// The server's default name of an index: `a_1_b_-1`.
fn default_index_name(keys: &Document) -> String {
    keys.iter()
        .map(|(k, v)| {
            let v = match v {
                Bson::String(s) => s.clone(),
                Bson::Double(f) if f.fract() == 0.0 => format!("{}", *f as i64),
                other => flatten::cell_text(other),
            };
            format!("{k}_{v}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// Create an index; returns its name. `options`: `name`, `unique`,
/// `sparse`, `expireAfterSeconds`, `partialFilterExpression`, `hidden`.
pub async fn create_index(
    c: &MongoClient,
    coll: &CollectionRef,
    keys: bson::Document,
    options: bson::Document,
) -> Result<String> {
    if keys.is_empty() {
        anyhow::bail!("{ENGINE} : clés de l'index attendues");
    }
    if let Some(k) = options
        .keys()
        .find(|k| !shell::INDEX_OPTIONS.contains(&k.as_str()))
    {
        anyhow::bail!("{ENGINE} : option d'index non prise en charge : {k}");
    }
    let name = match options.get("name") {
        Some(Bson::String(n)) if !n.trim().is_empty() => n.trim().to_string(),
        _ => default_index_name(&keys),
    };
    let mut spec = doc! { "key": keys, "name": &name };
    for (k, v) in options {
        if k != "name" {
            spec.insert(k, v);
        }
    }
    c.client
        .database(&coll.db)
        .run_command(doc! { "createIndexes": &coll.coll, "indexes": [spec] })
        .await
        .map_err(mongo_error)?;
    Ok(name)
}

pub async fn drop_index(c: &MongoClient, coll: &CollectionRef, name: &str) -> Result<()> {
    c.client
        .database(&coll.db)
        .run_command(doc! { "dropIndexes": &coll.coll, "index": name })
        .await
        .map_err(mongo_error)?;
    Ok(())
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

impl DocPage {
    fn new(docs: Vec<Document>, truncated: bool, elapsed: Instant) -> Self {
        let mut result = flatten(&docs);
        result.truncated = truncated;
        result.execution_time_ms = elapsed.elapsed().as_millis();
        Self { result, docs }
    }
}

/// A bar's text as a document, its error named after the bar.
fn bar(label: &str, text: &str) -> Result<Document> {
    shell::parse_document(text).map_err(|e| anyhow::anyhow!("{label} : {e}"))
}

pub async fn find_page(c: &MongoClient, q: &DocQuery) -> Result<DocPage> {
    let start = Instant::now();
    let filter = bar("Filtre", &q.filter)?;
    let sort = bar("Tri", &q.sort)?;
    let projection = bar("Projection", &q.projection)?;
    let collection = c.coll(&q.coll);
    let mut find = collection.find(filter).skip(q.skip);
    if q.limit > 0 {
        find = find.limit(i64::from(q.limit));
    }
    if !sort.is_empty() {
        find = find.sort(sort);
    }
    if !projection.is_empty() {
        find = find.projection(projection);
    }
    let docs: Vec<Document> = find
        .await
        .map_err(mongo_error)?
        .try_collect()
        .await
        .map_err(mongo_error)?;
    Ok(DocPage::new(docs, false, start))
}

/// Documents matching the filter (`estimatedDocumentCount` without one).
pub async fn count(c: &MongoClient, q: &DocQuery) -> Result<u64> {
    let filter = bar("Filtre", &q.filter)?;
    let coll = c.coll(&q.coll);
    if filter.is_empty() {
        coll.estimated_document_count()
            .max_time(COUNT_TIME)
            .await
            .map_err(mongo_error)
    } else {
        coll.count_documents(filter)
            .max_time(COUNT_TIME)
            .await
            .map_err(mongo_error)
    }
}

/// Up to `cap` documents of a cursor; `true` when more were available.
async fn collect_capped(
    mut cursor: mongodb::Cursor<Document>,
    cap: usize,
) -> Result<(Vec<Document>, bool)> {
    let mut docs = Vec::new();
    while let Some(doc) = cursor.try_next().await.map_err(mongo_error)? {
        if docs.len() == cap {
            return Ok((docs, true));
        }
        docs.push(doc);
    }
    Ok((docs, false))
}

/// The `aggregate` command of a pipeline (options merged, `batchSize`
/// into `cursor`).
fn aggregate_command(coll: &str, pipeline: Vec<Document>, options: Option<Document>) -> Document {
    let mut cursor = Document::new();
    let mut command = doc! {
        "aggregate": coll,
        "pipeline": pipeline.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    };
    for (k, v) in options.unwrap_or_default() {
        if k == "batchSize" {
            cursor.insert(k, v);
        } else {
            command.insert(k, v);
        }
    }
    command.insert("cursor", cursor);
    command
}

/// Run `pipeline` (a shell-syntax array) on `coll`.
pub async fn aggregate(
    c: &MongoClient,
    coll: &CollectionRef,
    pipeline: &str,
    max_rows: usize,
) -> Result<DocPage> {
    let start = Instant::now();
    let stages =
        match shell::parse_value(pipeline).map_err(|e| anyhow::anyhow!("Pipeline : {e}"))? {
            Bson::Array(items) => items
                .into_iter()
                .map(|s| match s {
                    Bson::Document(d) => Ok(d),
                    _ => Err(anyhow::anyhow!(
                        "Pipeline : chaque étape doit être un document"
                    )),
                })
                .collect::<Result<Vec<_>>>()?,
            Bson::Document(d) => vec![d],
            _ => anyhow::bail!("Pipeline : tableau d'étapes attendu"),
        };
    // Writes to a collection: only from the console, which confirms them.
    if let Some(stage) = shell::writing_stage(&stages) {
        anyhow::bail!(
            "Pipeline : l'étape {stage} écrit dans une collection ; l'exécuter depuis la console              (confirmation demandée)"
        );
    }
    let cursor = c
        .client
        .database(&coll.db)
        .run_cursor_command(aggregate_command(&coll.coll, stages, None))
        .await
        .map_err(mongo_error)?;
    let (docs, truncated) = collect_capped(cursor, max_rows.max(1)).await?;
    Ok(DocPage::new(docs, truncated, start))
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

/// Apply one change; `Err` names the missing document.
async fn apply_one(coll: &mongodb::Collection<Document>, change: &DocChange) -> Result<()> {
    let missing =
        |id: &Bson| anyhow::anyhow!("document introuvable (_id {})", shell::to_shell(id, false));
    match change {
        DocChange::Set { id, field, value } => {
            let r = coll
                .update_one(
                    doc! { "_id": id.clone() },
                    doc! { "$set": { field: value.clone() } },
                )
                .await
                .map_err(mongo_error)?;
            if r.matched_count == 0 {
                return Err(missing(id));
            }
        }
        DocChange::Unset { id, field } => {
            let r = coll
                .update_one(doc! { "_id": id.clone() }, doc! { "$unset": { field: "" } })
                .await
                .map_err(mongo_error)?;
            if r.matched_count == 0 {
                return Err(missing(id));
            }
        }
        DocChange::Replace { id, doc } => {
            let r = coll
                .replace_one(doc! { "_id": id.clone() }, doc)
                .await
                .map_err(mongo_error)?;
            if r.matched_count == 0 {
                return Err(missing(id));
            }
        }
        DocChange::Insert(doc) => {
            coll.insert_one(doc).await.map_err(mongo_error)?;
        }
        DocChange::Delete { id } => {
            let r = coll
                .delete_one(doc! { "_id": id.clone() })
                .await
                .map_err(mongo_error)?;
            if r.deleted_count == 0 {
                return Err(missing(id));
            }
        }
    }
    Ok(())
}

/// Apply `changes` one by one (no transaction: that needs a replica set);
/// returns how many were applied. On an error: « n appliquées, échec : … ».
pub async fn apply_changes(
    c: &MongoClient,
    coll: &CollectionRef,
    changes: &[DocChange],
) -> Result<usize> {
    let collection = c.coll(coll);
    for (done, change) in changes.iter().enumerate() {
        if let Err(e) = apply_one(&collection, change).await {
            anyhow::bail!(
                "{done} modification{} appliquée{}, échec : {e}",
                plural(done),
                plural(done)
            );
        }
    }
    Ok(changes.len())
}

pub async fn create_collection(c: &MongoClient, db: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("{ENGINE} : nom de collection attendu");
    }
    c.client
        .database(db)
        .create_collection(name)
        .await
        .map_err(mongo_error)
}

pub async fn drop_collection(c: &MongoClient, coll: &CollectionRef) -> Result<()> {
    c.coll(coll).drop().await.map_err(mongo_error)
}

pub async fn drop_database(c: &MongoClient, db: &str) -> Result<()> {
    c.client.database(db).drop().await.map_err(mongo_error)
}

// ---------------------------------------------------------------------------
// Console
// ---------------------------------------------------------------------------

/// Byte ranges of the units of a console script.
pub fn units(text: &str) -> Vec<Range<usize>> {
    shell::units(text)
}

/// Whether unit `span` is to run for the selection `range` (`None` = all;
/// an empty range = the unit holding that position).
fn selected(span: &Range<usize>, range: &Option<Range<usize>>) -> bool {
    match range {
        None => true,
        Some(r) if r.is_empty() => span.start <= r.start && r.start <= span.end,
        Some(r) => span.start < r.end && r.start < span.end,
    }
}

/// Destructive commands among the units of `range` (all when `None`).
pub fn warnings(text: &str, range: Option<Range<usize>>) -> Vec<Warning> {
    units(text)
        .into_iter()
        .filter(|span| selected(span, &range))
        .filter_map(|span| {
            let unit = &text[span.clone()];
            let cmd = shell::parse_command(unit, span.start).ok()?;
            shell::danger(&cmd).map(|reason| Warning {
                unit: unit.to_string(),
                reason,
            })
        })
        .collect()
}

fn plural(n: usize) -> &'static str {
    if n > 1 {
        "s"
    } else {
        ""
    }
}

/// A one-column result.
fn column_result(name: &str, rows: Vec<String>) -> QueryResult {
    QueryResult {
        columns: vec![Column {
            name: name.to_string(),
            type_name: "string".into(),
            nullable: true,
            is_primary_key: false,
        }],
        rows: rows.into_iter().map(|r| vec![r]).collect(),
        ..Default::default()
    }
}

/// The « résultat » of a write.
fn write_result(text: String, affected: u64) -> QueryResult {
    QueryResult {
        rows_affected: affected,
        ..column_result("résultat", vec![text])
    }
}

/// Run one command in the scope's database.
async fn execute(
    c: &MongoClient,
    cmd: MongoCommand,
    scope: &mut NoSqlScope,
    max_rows: usize,
) -> Result<(QueryResult, Option<Source>)> {
    let cap = max_rows.max(1);
    let db_name = scope.database.clone();
    let need_db = || -> Result<mongodb::Database> {
        if db_name.is_empty() {
            anyhow::bail!("Aucune base sélectionnée : utiliser « use <base> »");
        }
        Ok(c.client.database(&db_name))
    };
    let coll_ref = |coll: &str| CollectionRef {
        db: db_name.clone(),
        coll: coll.to_string(),
    };
    Ok(match cmd {
        MongoCommand::Use(db) => {
            scope.database = db.clone();
            (
                column_result("résultat", vec![format!("base courante : {db}")]),
                None,
            )
        }
        MongoCommand::ShowDbs => {
            let reply = c
                .client
                .database("admin")
                .run_command(doc! { "listDatabases": 1, "authorizedDatabases": true })
                .await
                .map_err(mongo_error)?;
            let docs: Vec<Document> = reply
                .get_array("databases")
                .map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect())
                .unwrap_or_default();
            (flatten(&docs), None)
        }
        MongoCommand::ShowCollections | MongoCommand::GetCollectionNames => {
            let mut names = need_db()?
                .list_collection_names()
                .await
                .map_err(mongo_error)?;
            sort_names(&mut names);
            (column_result("collection", names), None)
        }
        MongoCommand::Find {
            coll,
            filter,
            projection,
            sort,
            skip,
            limit,
            one,
        } => {
            let collection = need_db()?.collection::<Document>(&coll);
            let wanted = limit.map(i64::unsigned_abs).filter(|l| *l > 0);
            let cap = if one { 1 } else { cap };
            let fetch = match wanted {
                Some(l) if l <= cap as u64 => l as i64,
                _ => cap as i64 + 1,
            };
            let mut find = collection.find(filter).limit(if one { 1 } else { fetch });
            if let Some(s) = sort {
                find = find.sort(s);
            }
            if let Some(s) = skip {
                find = find.skip(s);
            }
            if let Some(p) = projection.clone() {
                find = find.projection(p);
            }
            let cursor = find.await.map_err(mongo_error)?;
            let (docs, truncated) = collect_capped(cursor, cap).await?;
            let mut result = flatten(&docs);
            result.truncated = truncated && !one;
            // A projection hides fields: writing its rows back would lose
            // them (`$set` of a projected sub-document, Replace…).
            let projected = projection.as_ref().is_some_and(|p| !p.is_empty());
            let source = (!projected).then(|| Source::Collection {
                coll: coll_ref(&coll),
                docs,
            });
            (result, source)
        }
        MongoCommand::Aggregate {
            coll,
            pipeline,
            options,
        } => {
            let cursor = need_db()?
                .run_cursor_command(aggregate_command(&coll, pipeline, options))
                .await
                .map_err(mongo_error)?;
            let (docs, truncated) = collect_capped(cursor, cap).await?;
            let mut result = flatten(&docs);
            result.truncated = truncated;
            (result, None)
        }
        MongoCommand::Count { coll, filter } => {
            let n = need_db()?
                .collection::<Document>(&coll)
                .count_documents(filter)
                .max_time(COUNT_TIME)
                .await
                .map_err(mongo_error)?;
            (column_result("nombre", vec![n.to_string()]), None)
        }
        MongoCommand::EstimatedCount { coll } => {
            let n = need_db()?
                .collection::<Document>(&coll)
                .estimated_document_count()
                .max_time(COUNT_TIME)
                .await
                .map_err(mongo_error)?;
            (column_result("nombre", vec![n.to_string()]), None)
        }
        MongoCommand::Distinct {
            coll,
            field,
            filter,
        } => {
            let values = need_db()?
                .collection::<Document>(&coll)
                .distinct(&field, filter)
                .await
                .map_err(mongo_error)?;
            let truncated = values.len() > cap;
            let mut result =
                column_result("valeur", values.iter().take(cap).map(cell_text).collect());
            result.truncated = truncated;
            (result, None)
        }
        MongoCommand::InsertOne { coll, doc } => {
            let r = need_db()?
                .collection::<Document>(&coll)
                .insert_one(doc)
                .await
                .map_err(mongo_error)?;
            (
                write_result(
                    format!("1 inséré, _id {}", shell::to_shell(&r.inserted_id, false)),
                    1,
                ),
                None,
            )
        }
        MongoCommand::InsertMany {
            coll,
            docs,
            options,
        } => {
            if docs.is_empty() {
                anyhow::bail!("insertMany : aucun document");
            }
            let ordered = !matches!(
                options.as_ref().and_then(|o| o.get("ordered")),
                Some(Bson::Boolean(false))
            );
            let r = need_db()?
                .collection::<Document>(&coll)
                .insert_many(docs)
                .ordered(ordered)
                .await
                .map_err(mongo_error)?;
            let n = r.inserted_ids.len();
            (
                write_result(format!("{n} inséré{}", plural(n)), n as u64),
                None,
            )
        }
        MongoCommand::Update {
            coll,
            filter,
            update,
            many,
            upsert,
        } => {
            let collection = need_db()?.collection::<Document>(&coll);
            let update: mongodb::options::UpdateModifications = match update {
                UpdateSpec::Document(d) => d.into(),
                UpdateSpec::Pipeline(p) => p.into(),
            };
            let r = if many {
                collection.update_many(filter, update).upsert(upsert).await
            } else {
                collection.update_one(filter, update).upsert(upsert).await
            }
            .map_err(mongo_error)?;
            (
                update_result(r.matched_count, r.modified_count, r.upserted_id),
                None,
            )
        }
        MongoCommand::ReplaceOne {
            coll,
            filter,
            replacement,
            upsert,
        } => {
            let r = need_db()?
                .collection::<Document>(&coll)
                .replace_one(filter, replacement)
                .upsert(upsert)
                .await
                .map_err(mongo_error)?;
            (
                update_result(r.matched_count, r.modified_count, r.upserted_id),
                None,
            )
        }
        MongoCommand::Delete { coll, filter, many } => {
            let collection = need_db()?.collection::<Document>(&coll);
            let r = if many {
                collection.delete_many(filter).await
            } else {
                collection.delete_one(filter).await
            }
            .map_err(mongo_error)?;
            let n = r.deleted_count;
            (
                write_result(format!("{n} supprimé{}", plural(n as usize)), n),
                None,
            )
        }
        MongoCommand::CreateIndex {
            coll,
            keys,
            options,
        } => {
            need_db()?;
            let name = create_index(c, &coll_ref(&coll), keys, options.unwrap_or_default()).await?;
            (write_result(format!("index créé : {name}"), 0), None)
        }
        MongoCommand::DropIndex { coll, index } => {
            need_db()?
                .run_command(doc! { "dropIndexes": &coll, "index": index })
                .await
                .map_err(mongo_error)?;
            (write_result("index supprimé".into(), 0), None)
        }
        MongoCommand::GetIndexes { coll } => {
            need_db()?;
            (flatten(&index_docs(c, &coll_ref(&coll)).await?), None)
        }
        MongoCommand::CreateCollection { name, options } => {
            let mut command = doc! { "create": &name };
            command.extend(options.unwrap_or_default());
            need_db()?.run_command(command).await.map_err(mongo_error)?;
            (write_result(format!("collection créée : {name}"), 0), None)
        }
        MongoCommand::DropCollection { coll } => {
            need_db()?
                .collection::<Document>(&coll)
                .drop()
                .await
                .map_err(mongo_error)?;
            (
                write_result(format!("collection supprimée : {coll}"), 0),
                None,
            )
        }
        MongoCommand::DropDatabase => {
            need_db()?.drop().await.map_err(mongo_error)?;
            (write_result(format!("base supprimée : {db_name}"), 0), None)
        }
        MongoCommand::RunCommand { command, admin } => {
            let db = if admin || db_name.is_empty() {
                c.client.database("admin")
            } else {
                c.client.database(&db_name)
            };
            let reply = db.run_command(command).await.map_err(mongo_error)?;
            (flatten(&[reply]), None)
        }
    })
}

fn update_result(matched: u64, modified: u64, upserted: Option<Bson>) -> QueryResult {
    let mut text = format!(
        "{matched} correspondant{}, {modified} modifié{}",
        plural(matched as usize),
        plural(modified as usize)
    );
    let mut affected = modified;
    if let Some(id) = upserted {
        text.push_str(&format!(", inséré _id {}", shell::to_shell(&id, false)));
        affected += 1;
    }
    write_result(text, affected)
}

/// Run the units of `range` (all when `None`) in order, stopping at the
/// first error (a syntax error included).
pub async fn run_script(
    c: &MongoClient,
    text: &str,
    range: Option<Range<usize>>,
    scope: &mut NoSqlScope,
    max_rows: usize,
) -> Vec<NoSqlOutcome> {
    let mut out = Vec::new();
    for span in units(text).into_iter().filter(|s| selected(s, &range)) {
        let unit = &text[span.clone()];
        let start = Instant::now();
        let mut read_only = None;
        let (result, source) = match shell::parse_command(unit, span.start) {
            Err(e) => (Err(e.to_string()), None),
            Ok(cmd) => {
                read_only = read_only_note(&cmd).map(str::to_string);
                match execute(c, cmd, scope, max_rows).await {
                    Ok((mut r, source)) => {
                        r.execution_time_ms = start.elapsed().as_millis();
                        (Ok(r), source)
                    }
                    Err(e) => (Err(e.to_string()), None),
                }
            }
        };
        let failed = result.is_err();
        out.push(NoSqlOutcome {
            outcome: StatementOutcome {
                sql: unit.to_string(),
                result,
                elapsed_ms: start.elapsed().as_millis(),
            },
            read_only: if failed { None } else { read_only },
            source,
        });
        if failed {
            break;
        }
    }
    out
}

/// Why the result of `cmd` can't be edited although it shows documents.
fn read_only_note(cmd: &MongoCommand) -> Option<&'static str> {
    match cmd {
        MongoCommand::Find {
            projection: Some(p),
            ..
        } if !p.is_empty() => Some(super::PROJECTION_READ_ONLY),
        _ => None,
    }
}

pub fn complete(text: &str, cursor: usize, cx: &CompletionContext) -> Completions {
    complete::complete(text, cursor, cx)
}

pub use complete::replace_start;

/// The script reformatted (one command per unit, long arguments
/// indented); `None` when it doesn't parse. Comments are not kept.
pub fn format(text: &str) -> Option<String> {
    let commands = shell::parse_script(text)
        .into_iter()
        .map(|p| p.map(|p| shell::command_to_shell(&p.command)))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if commands.is_empty() {
        return None;
    }
    let mut out = commands.join("\n");
    out.push('\n');
    Some(out)
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

    #[test]
    fn units_and_selection() {
        let text = "use a\ndb.x.find(); db.y.drop()";
        assert_eq!(units(text), [0..5, 6..17, 19..30]);
        assert!(selected(&(6..17), &None));
        assert!(selected(&(6..17), &Some(10..10)));
        assert!(selected(&(6..17), &Some(17..17)));
        assert!(!selected(&(6..17), &Some(18..18)));
        assert!(selected(&(6..17), &Some(0..7)));
        assert!(!selected(&(6..17), &Some(0..6)));
    }

    #[test]
    fn warnings_follow_the_selection() {
        let text = "db.a.find()\ndb.a.deleteMany({})\ndb.b.drop()";
        let all = warnings(text, None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].unit, "db.a.deleteMany({})");
        assert!(all[0].reason.contains("Supprime tous les documents"));
        assert!(warnings(text, Some(0..0)).is_empty());
        assert_eq!(warnings(text, Some(13..13)).len(), 1);
        // Syntax errors are not warnings.
        assert!(warnings("db.a.drop(", None).is_empty());
    }

    #[test]
    fn format_reprints_every_unit() {
        let text = "use shop ; db.users.find( {a:1} ).limit( 5 ) // x\n\ndb.users.drop()";
        assert_eq!(
            format(text).unwrap(),
            "use shop\ndb.users.find({a: 1}).limit(5)\ndb.users.drop()\n"
        );
        assert_eq!(format("db.users.find({a:"), None);
        assert_eq!(format("  "), None);
        let again = format(&format(text).unwrap()).unwrap();
        assert_eq!(again, format(text).unwrap());
    }

    #[test]
    fn connection_uri_from_fields_or_uri() {
        let base = ConnectionConfig {
            db_type: crate::engine::models::DatabaseType::MongoDB,
            host: Some("db.local".into()),
            port: Some(27018),
            ..Default::default()
        };
        assert_eq!(connection_uri(&base), "mongodb://db.local:27018/");
        let v6 = ConnectionConfig {
            host: Some("::1".into()),
            port: None,
            ..base.clone()
        };
        assert_eq!(connection_uri(&v6), "mongodb://[::1]:27017/");
        let uri = ConnectionConfig {
            uri: Some("mongodb://root@h1,h2/?replicaSet=rs".into()),
            password: Some("p@ss".into()),
            ..base.clone()
        };
        assert_eq!(
            connection_uri(&uri),
            "mongodb://root:p%40ss@h1,h2/?replicaSet=rs"
        );
    }

    #[tokio::test]
    async fn options_carry_credentials_and_tls() {
        use crate::engine::models::{DatabaseType, SslMode};
        let fields = ConnectionConfig {
            db_type: DatabaseType::MongoDB,
            host: Some("127.0.0.1".into()),
            username: Some("root".into()),
            password: Some("pw".into()),
            ..Default::default()
        };
        let o = client_options(&fields).await.unwrap();
        let cred = o.credential.unwrap();
        assert_eq!(cred.username.as_deref(), Some("root"));
        assert_eq!(cred.password.as_deref(), Some("pw"));
        assert_eq!(cred.source.as_deref(), Some("admin"));
        assert_eq!(o.tls, Some(Tls::Disabled));
        let insecure = ConnectionConfig {
            ssl_mode: Some(SslMode::Require),
            auth_source: Some("shop".into()),
            ..fields.clone()
        };
        let o = client_options(&insecure).await.unwrap();
        assert_eq!(o.credential.unwrap().source.as_deref(), Some("shop"));
        match o.tls {
            Some(Tls::Enabled(t)) => assert_eq!(t.allow_invalid_certificates, Some(true)),
            other => panic!("{other:?}"),
        }
        let uri = ConnectionConfig {
            uri: Some("mongodb://u@127.0.0.1/?authSource=x&tls=true".into()),
            password: Some("pw".into()),
            ssl_ca: Some("ca.pem".into()),
            ..fields.clone()
        };
        let o = client_options(&uri).await.unwrap();
        let cred = o.credential.unwrap();
        assert_eq!(cred.username.as_deref(), Some("u"), "the URI's user");
        assert_eq!(cred.source.as_deref(), Some("x"));
        assert_eq!(cred.password.as_deref(), Some("pw"));
        match o.tls {
            Some(Tls::Enabled(t)) => {
                assert_eq!(t.ca_file_path, Some("ca.pem".into()));
                assert_eq!(t.allow_invalid_certificates, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_uri_without_user_ignores_a_stale_username() {
        let config = ConnectionConfig {
            db_type: crate::engine::models::DatabaseType::MongoDB,
            uri: Some("mongodb://127.0.0.1:47017/".into()),
            // ConnectionConfig::default()'s, or left by the parameters' form.
            username: Some("postgres".into()),
            ..Default::default()
        };
        let o = client_options(&config).await.unwrap();
        assert!(o.credential.is_none(), "{:?}", o.credential);
    }

    #[tokio::test]
    async fn unreachable_server_is_a_french_error() {
        let config = ConnectionConfig {
            db_type: crate::engine::models::DatabaseType::MongoDB,
            uri: Some("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=200".into()),
            ..Default::default()
        };
        let e = connect(&config).await.err().unwrap().to_string();
        assert!(e.starts_with("MongoDB : serveur injoignable"), "{e}");
    }

    #[test]
    fn aggregate_command_moves_batch_size() {
        let c = aggregate_command(
            "u",
            vec![doc! { "$match": {} }],
            Some(doc! { "allowDiskUse": true, "batchSize": 10 }),
        );
        assert_eq!(
            c,
            doc! { "aggregate": "u", "pipeline": [{ "$match": {} }], "allowDiskUse": true, "cursor": { "batchSize": 10 } }
        );
        assert_eq!(
            default_index_name(&doc! { "a": 1, "b": -1, "c": "text" }),
            "a_1_b_-1_c_text"
        );
    }
}
