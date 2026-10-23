//! MongoDB and Redis: the engine contract shared by the GUI and the TUI
//! (plan `2026-10-23-13-nosql.md`). The SQL code paths (`ops::query`,
//! `ops::rows`, `sql::*`…) are never used for these engines: they go through
//! this module, `mongo` and `redis`.
#![allow(dead_code)] // used by the GUI and the TUI (plan 13, Tasks 4 and 5)

pub mod mongo;
pub mod redis;

use std::ops::Range;
use std::path::Path;

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{ConnectionConfig, DatabaseType, SchemaInfo};
use crate::engine::ops::query::StatementOutcome;
use crate::engine::sql::completion::{ColumnInfo, Completions, TableRef};

/// A field missing from a document, as opposed to a field holding `null`
/// (`NULL_CELL`). Like `NULL_CELL`, it can't be typed.
pub const ABSENT_CELL: &str = "\u{E000}ABSENT";

/// Whether `cell` is a missing field (`ABSENT_CELL`).
pub fn is_absent(cell: &str) -> bool {
    cell == ABSENT_CELL
}

/// Current database of a NoSQL console: a MongoDB database name (empty when
/// none is chosen), or a Redis database index ("0".."15").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoSqlScope {
    pub database: String,
}

impl NoSqlScope {
    /// The connection's default database (Redis: "0" when unset).
    pub fn initial(config: &ConnectionConfig) -> Self {
        let database = config.database.trim().to_string();
        let database = match config.db_type {
            DatabaseType::Redis if database.is_empty() => "0".to_string(),
            _ => database,
        };
        Self { database }
    }
}

/// Where the rows of a result come from (makes console results editable).
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Collection {
        coll: mongo::CollectionRef,
        docs: Vec<bson::Document>,
    },
    Key {
        db: u8,
        key: Vec<u8>,
        kind: redis::RedisType,
    },
}

/// One executed unit of a NoSQL console.
#[derive(Debug)]
pub struct NoSqlOutcome {
    pub outcome: StatementOutcome,
    pub source: Option<Source>,
    /// Why rows that look like documents / a value can't be edited (a
    /// projection, `HSCAN … NOVALUES`…), shown by the interfaces.
    pub read_only: Option<String>,
}

/// Why the rows of a MongoDB find with a projection are read-only.
pub const PROJECTION_READ_ONLY: &str =
    "Lecture seule : la projection masque des champs (modifier ces lignes les effacerait)";

/// A destructive command to confirm before it is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// The unit's text.
    pub unit: String,
    /// Why, in French: « Supprime tous les documents de shop.users ».
    pub reason: String,
}

/// Byte ranges of the executable units of a console script.
pub fn units(db: &DatabaseType, text: &str) -> Vec<Range<usize>> {
    match db {
        DatabaseType::MongoDB => mongo::units(text),
        DatabaseType::Redis => redis::units(text),
        _ => Vec::new(),
    }
}

/// The unit holding `cursor` (or ending right before it).
pub fn unit_at(db: &DatabaseType, text: &str, cursor: usize) -> Option<Range<usize>> {
    let units = units(db, text);
    units
        .iter()
        .find(|r| r.start <= cursor && cursor < r.end)
        .or_else(|| units.iter().rev().find(|r| r.end <= cursor))
        .cloned()
}

/// Destructive commands among the units of `range` (all when `None`). Parse
/// errors are not warnings: the run reports them.
pub fn warnings(db: &DatabaseType, text: &str, range: Option<Range<usize>>) -> Vec<Warning> {
    match db {
        DatabaseType::MongoDB => mongo::warnings(text, range),
        DatabaseType::Redis => redis::warnings(text, range),
        _ => Vec::new(),
    }
}

/// Run the units of `range` (all when `None`) in order, stopping at the
/// first error. Commands are run as given: confirm `warnings` first.
pub async fn run_script(
    conn: &DatabaseConnection,
    text: &str,
    range: Option<Range<usize>>,
    scope: &mut NoSqlScope,
    max_rows: usize,
) -> Vec<NoSqlOutcome> {
    match conn {
        DatabaseConnection::MongoDB(c) => mongo::run_script(c, text, range, scope, max_rows).await,
        DatabaseConnection::Redis(c) => redis::run_script(c, text, range, scope, max_rows).await,
        _ => vec![NoSqlOutcome {
            outcome: StatementOutcome {
                sql: text.to_string(),
                result: Err(format!("{} n'est pas un moteur NoSQL", conn.db_type())),
                elapsed_ms: 0,
            },
            source: None,
            read_only: None,
        }],
    }
}

/// What the NoSQL completion knows about the connection.
pub struct CompletionContext<'a> {
    pub scope: &'a NoSqlScope,
    /// MongoDB: databases → collections.
    pub schemas: &'a [SchemaInfo],
    /// Sampled fields of a collection (`schema` = database), once loaded.
    pub columns: &'a dyn Fn(&TableRef) -> Option<Vec<ColumnInfo>>,
    /// Redis: the keys known (loaded by the explorer).
    pub keys: &'a [String],
}

pub fn complete(
    db: &DatabaseType,
    text: &str,
    cursor: usize,
    cx: &CompletionContext,
) -> Completions {
    match db {
        DatabaseType::MongoDB => mongo::complete(text, cursor, cx),
        DatabaseType::Redis => redis::complete(text, cursor, cx),
        _ => Completions::default(),
    }
}

/// Start (bytes) of the text a suggestion of [`complete`] replaces, up to
/// `cursor`: MongoDB, the word with the `$` typed before it (`$g` → `$gt`);
/// Redis, the whole argument (`user:4` → `user:42`, quotes included). Both
/// interfaces replace `completion_start..cursor` with `Suggestion::insert`.
pub fn completion_start(db: &DatabaseType, text: &str, cursor: usize) -> usize {
    match db {
        DatabaseType::Redis => redis::replace_start(text, cursor),
        _ => mongo::replace_start(text, cursor),
    }
}

/// The console text reformatted (MongoDB); `None` when the engine has no
/// formatter (Redis) or the text doesn't parse.
pub fn format(db: &DatabaseType, text: &str) -> Option<String> {
    match db {
        DatabaseType::MongoDB => mongo::format(text),
        _ => None,
    }
}

/// NoSQL export / import file formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferFormat {
    /// One JSON array.
    Json,
    /// One JSON value per line (mongoexport's default).
    JsonLines,
    Csv,
}

impl TransferFormat {
    pub const ALL: [TransferFormat; 3] = [Self::Json, Self::JsonLines, Self::Csv];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::JsonLines => "jsonl",
            Self::Csv => "csv",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON (tableau)",
            Self::JsonLines => "JSON Lines",
            Self::Csv => "CSV",
        }
    }

    /// The format of a file, from its extension (`.ndjson` = JSON Lines).
    pub fn from_path(p: &Path) -> Option<Self> {
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "json" => Some(Self::Json),
            "jsonl" | "ndjson" => Some(Self::JsonLines),
            "csv" => Some(Self::Csv),
            _ => None,
        }
    }
}

/// What a MongoDB CSV round trip loses, shown by both interfaces' export /
/// import dialogs (`flatten_paths` / `csv_row_to_doc`).
pub const CSV_NULL_HINT: &str = "CSV : null et champ absent s'exportent tous deux vides ; une chaîne vide s'écrit \"\" ; à l'import, vide = champ absent, null = valeur nulle.";

/// Options of a NoSQL import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// MongoDB: replace the documents with the same `_id`; Redis: overwrite
    /// existing keys.
    pub upsert: bool,
    /// CSV: keep every value as a string (no type inference).
    pub keep_strings: bool,
    /// CSV: read ISO 8601 values as dates (unless `keep_strings`).
    pub parse_dates: bool,
}

/// Error of a stub not implemented yet (plan 13, Tasks 2 and 3).
pub(crate) fn not_implemented(engine: &str) -> anyhow::Error {
    anyhow::anyhow!("{engine} : pas encore disponible (non implémenté)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_is_neither_null_nor_text() {
        assert!(is_absent(ABSENT_CELL));
        assert!(!is_absent(crate::engine::models::NULL_CELL));
        assert!(!is_absent("ABSENT"));
        assert!(!is_absent(""));
    }

    #[test]
    fn initial_scope_is_the_default_database() {
        let mongo = ConnectionConfig {
            db_type: DatabaseType::MongoDB,
            database: " shop ".into(),
            ..Default::default()
        };
        assert_eq!(NoSqlScope::initial(&mongo).database, "shop");
        let all = ConnectionConfig {
            database: String::new(),
            ..mongo
        };
        assert_eq!(NoSqlScope::initial(&all).database, "");
        let redis = ConnectionConfig {
            db_type: DatabaseType::Redis,
            database: String::new(),
            ..Default::default()
        };
        assert_eq!(NoSqlScope::initial(&redis).database, "0");
        let redis3 = ConnectionConfig {
            database: "3".into(),
            ..redis
        };
        assert_eq!(NoSqlScope::initial(&redis3).database, "3");
    }

    #[test]
    fn transfer_formats_by_extension() {
        for f in TransferFormat::ALL {
            let p = std::path::PathBuf::from(format!("a.b.{}", f.extension()));
            assert_eq!(TransferFormat::from_path(&p), Some(f));
        }
        assert_eq!(
            TransferFormat::from_path(Path::new("x.NDJSON")),
            Some(TransferFormat::JsonLines)
        );
        assert_eq!(TransferFormat::from_path(Path::new("x.sql")), None);
        assert_eq!(TransferFormat::from_path(Path::new("x")), None);
    }

    #[test]
    fn sql_engines_have_no_nosql_units() {
        assert!(units(&DatabaseType::Postgres, "SELECT 1").is_empty());
        assert!(warnings(&DatabaseType::MySQL, "DROP TABLE t", None).is_empty());
        assert_eq!(format(&DatabaseType::Redis, "GET k"), None);
    }

    #[tokio::test]
    async fn run_script_on_a_sql_connection_is_an_error() {
        let conn = crate::engine::ops::test_support::sqlite_mem(&[]).await;
        let mut scope = NoSqlScope {
            database: String::new(),
        };
        let out = run_script(&conn, "db.c.find()", None, &mut scope, 10).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].outcome.result.is_err());
    }

    /// The TLS stacks of both drivers find a rustls crypto provider (one
    /// only, `ring`: two would make rustls panic): a handshake against a
    /// server that hangs up fails with an error, not a panic.
    #[tokio::test]
    async fn tls_handshakes_fail_cleanly() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });
        let redis = ::redis::Client::open(format!("rediss://127.0.0.1:{port}/#insecure")).unwrap();
        assert!(redis.get_multiplexed_async_connection().await.is_err());
        let mongo = mongodb::Client::with_uri_str(format!(
            "mongodb://127.0.0.1:{port}/?tls=true&serverSelectionTimeoutMS=300&connectTimeoutMS=300"
        ))
        .await
        .unwrap();
        let ping = mongo
            .database("admin")
            .run_command(bson::doc! { "ping": 1 })
            .await;
        assert!(ping.is_err());
    }
}
