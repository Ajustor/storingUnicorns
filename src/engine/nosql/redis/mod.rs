//! Redis engine (plan 13). Task 1 sets the signatures; the bodies are
//! Task 3's: until then, async functions fail with "non implémenté" and pure
//! functions return empty values. (The `redis` crate is `::redis` here.)
#![allow(unused_variables)] // stubs (plan 13, Task 3)

pub mod cli;

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use anyhow::Result;
use tokio::sync::Mutex;

use super::{
    not_implemented, CompletionContext, ImportOptions, NoSqlOutcome, NoSqlScope, TransferFormat,
    Warning,
};
use crate::engine::models::{ConnectionConfig, QueryResult, SchemaInfo};
use crate::engine::ops::rows::RowChanges;
use crate::engine::ops::transfer::ImportStats;
use crate::engine::sql::completion::Completions;

const ENGINE: &str = "Redis";

/// An open Redis connection: the client, and one auto-reconnecting manager
/// per database browsed.
pub struct RedisClient {
    /// Boxed: `ConnectionInfo` would make every `DatabaseConnection` large.
    pub(crate) client: Box<::redis::Client>,
    pub(crate) managers: Mutex<HashMap<u8, ::redis::aio::ConnectionManager>>,
    /// Database of the connection settings.
    pub(crate) default_db: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedisType {
    String,
    List,
    Set,
    ZSet,
    Hash,
    Stream,
    /// A module type (`ReJSON-RL`…): read only.
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyInfo {
    pub key: Vec<u8>,
    pub kind: RedisType,
    /// Seconds to live; `None` = persistent.
    pub ttl: Option<i64>,
    /// Length / cardinality / bytes, when known.
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanPage {
    pub keys: Vec<KeyInfo>,
    /// Next SCAN cursor; 0 = done.
    pub cursor: u64,
}

/// PING, AUTH (optional ACL user), TLS.
pub async fn connect(config: &ConnectionConfig) -> Result<RedisClient> {
    Err(not_implemented(ENGINE))
}

pub async fn test(c: &RedisClient) -> Result<()> {
    Err(not_implemented(ENGINE))
}

/// One `SchemaInfo` per database ("db0"…; `tables` empty): `CONFIG GET
/// databases` (16 when refused) + `INFO keyspace`; the configured database
/// is always listed.
pub async fn databases(c: &RedisClient) -> Result<Vec<SchemaInfo>> {
    Err(not_implemented(ENGINE))
}

/// One SCAN step, with each key's type, TTL and size (pipelined).
pub async fn scan(
    c: &RedisClient,
    db: u8,
    pattern: &str,
    kind: Option<&RedisType>,
    cursor: u64,
    count: usize,
) -> Result<ScanPage> {
    Err(not_implemented(ENGINE))
}

pub async fn key_info(c: &RedisClient, db: u8, key: &[u8]) -> Result<Option<KeyInfo>> {
    Err(not_implemented(ENGINE))
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RedisValue {
    String(Vec<u8>),
    List(Vec<Vec<u8>>),
    Set(Vec<Vec<u8>>),
    ZSet(Vec<(Vec<u8>, f64)>),
    Hash(Vec<(Vec<u8>, Vec<u8>)>),
    Stream(Vec<StreamEntry>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValuePage {
    pub value: RedisValue,
    /// Elements in the whole value.
    pub total: u64,
    /// Where the next page starts: an offset (list, zset, stream) or a
    /// SCAN cursor (set, hash); `None` = last page.
    pub next: Option<u64>,
}

/// Up to `limit` elements of `key` from `from` (offset or cursor).
pub async fn read_value(
    c: &RedisClient,
    db: u8,
    key: &[u8],
    from: u64,
    limit: usize,
) -> Result<ValuePage> {
    Err(not_implemented(ENGINE))
}

/// Grid of a value; columns: valeur | #, valeur | membre | membre, score |
/// champ, valeur | id, champs.
pub fn value_result(v: &RedisValue) -> QueryResult {
    QueryResult::default()
}

/// One change to a key.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyEdit {
    SetString(Vec<u8>),
    ListSet {
        index: i64,
        value: Vec<u8>,
    },
    ListPush {
        left: bool,
        value: Vec<u8>,
    },
    ListRemove {
        index: i64,
    },
    SetAdd(Vec<u8>),
    SetRemove(Vec<u8>),
    ZAdd {
        member: Vec<u8>,
        score: f64,
    },
    ZRemove(Vec<u8>),
    HSet {
        field: Vec<u8>,
        value: Vec<u8>,
    },
    HDel(Vec<u8>),
    XAdd {
        id: Option<String>,
        fields: Vec<(Vec<u8>, Vec<u8>)>,
    },
    XDel(String),
    /// Seconds to live; `None` = persistent.
    Expire(Option<i64>),
    Rename {
        to: Vec<u8>,
    },
    Delete,
}

/// The command (argv) of an edit.
pub fn edit_to_command(key: &[u8], e: &KeyEdit) -> Vec<Vec<u8>> {
    Vec::new()
}

/// The edits of a value grid's pending changes.
pub fn grid_edits(
    key: &KeyInfo,
    page: &ValuePage,
    changes: &RowChanges,
) -> Result<Vec<KeyEdit>, String> {
    Err(not_implemented(ENGINE).to_string())
}

/// Apply `edits` to `key`; returns how many were applied.
pub async fn apply_edits(c: &RedisClient, db: u8, key: &[u8], edits: &[KeyEdit]) -> Result<usize> {
    Err(not_implemented(ENGINE))
}

/// Create `key` (refused when it exists).
pub async fn create_key(
    c: &RedisClient,
    db: u8,
    key: &[u8],
    value: &RedisValue,
    ttl: Option<i64>,
) -> Result<()> {
    Err(not_implemented(ENGINE))
}

pub async fn flush_db(c: &RedisClient, db: u8) -> Result<()> {
    Err(not_implemented(ENGINE))
}

/// A node of the key tree: a prefix group, or a key (`key` indexes the
/// keys given to `key_tree`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyNode {
    pub label: String,
    /// Full prefix of a group (`user:42:`), the key itself for a key.
    pub prefix: String,
    pub children: Vec<KeyNode>,
    pub key: Option<usize>,
    /// Keys under this node.
    pub count: usize,
}

/// Keys grouped by their `sep`-separated prefixes: groups then keys,
/// sorted ignoring case.
pub fn key_tree(keys: &[KeyInfo], sep: char) -> Vec<KeyNode> {
    Vec::new()
}

/// Bytes as text: UTF-8 when valid, else escaped like redis-cli (`\xff`).
pub fn display_bytes(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Inverse of `display_bytes`.
pub fn parse_display(s: &str) -> Result<Vec<u8>, String> {
    Err(not_implemented(ENGINE).to_string())
}

/// Write the keys matching `pattern` to `path`; returns their number.
pub async fn export_keys(
    c: &RedisClient,
    db: u8,
    pattern: &str,
    path: &Path,
    format: TransferFormat,
    progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<usize> {
    Err(not_implemented(ENGINE))
}

pub async fn import_keys(
    c: &RedisClient,
    db: u8,
    path: &Path,
    format: TransferFormat,
    opts: &ImportOptions,
    progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<ImportStats> {
    Err(not_implemented(ENGINE))
}

/// A value as JSON (binary as `{"$base64": …}`).
pub fn value_to_json(v: &RedisValue) -> serde_json::Value {
    serde_json::Value::Null
}

pub fn value_from_json(kind: &RedisType, j: &serde_json::Value) -> Result<RedisValue, String> {
    Err(not_implemented(ENGINE).to_string())
}

/// Byte ranges of the commands of a console script (one per line).
pub fn units(text: &str) -> Vec<Range<usize>> {
    Vec::new()
}

/// Commands to confirm among the units of `range` (all when `None`).
pub fn warnings(text: &str, range: Option<Range<usize>>) -> Vec<Warning> {
    Vec::new()
}

pub async fn run_script(
    c: &RedisClient,
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
