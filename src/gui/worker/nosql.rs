//! MongoDB and Redis work of the worker (plan 13): console runs, document
//! pages, key scans and values, writes, administration tasks and the
//! JSON / JSON Lines / CSV transfers. Never through the SQL paths.

use std::ops::Range;
use std::path::PathBuf;

use crate::engine::nosql::mongo::{self, CollectionRef, DocChange, DocQuery};
use crate::engine::nosql::redis::{self, KeyEdit, RedisType, RedisValue, ScanPage};
use crate::engine::nosql::{self, ImportOptions, NoSqlScope, TransferFormat};
use crate::engine::ops::rows::BatchReport;

use super::{err, Conn, Event, OpId, RunId, TabId, Worker};

/// Keys the explorer lists per Redis database at most (beyond, the « Clés »
/// tab browses them).
pub const EXPLORER_MAX_KEYS: usize = 5000;
/// SCAN `COUNT` hint of each step.
const SCAN_COUNT: usize = 1000;

/// An administration task of a NoSQL connection.
#[derive(Clone, Debug, PartialEq)]
pub enum NoSqlTask {
    CreateCollection {
        db: String,
        name: String,
    },
    DropCollection(CollectionRef),
    /// `deleteMany({})`.
    EmptyCollection(CollectionRef),
    DropDatabase(String),
    CreateIndex {
        coll: CollectionRef,
        keys: bson::Document,
        options: bson::Document,
    },
    DropIndex {
        coll: CollectionRef,
        name: String,
    },
    FlushDb(u8),
    CreateKey {
        db: u8,
        key: Vec<u8>,
        value: RedisValue,
        ttl: Option<i64>,
    },
    /// Rename, expiration, deletion… of a key (from the explorer or the
    /// « Clés » tab).
    EditKey {
        db: u8,
        key: Vec<u8>,
        edits: Vec<KeyEdit>,
    },
}

/// What a finished task makes stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refresh {
    /// The databases / collections of the connection.
    Schemas,
    /// A collection's documents (its tabs) and details.
    Collection(CollectionRef),
    /// A collection's indexes (index dialog, explorer).
    Indexes(CollectionRef),
    /// The keys of a Redis database (explorer, « Clés » tabs).
    RedisDb(u8),
}

impl NoSqlTask {
    pub fn refresh(&self) -> Refresh {
        match self {
            Self::CreateCollection { .. } | Self::DropCollection(_) | Self::DropDatabase(_) => {
                Refresh::Schemas
            }
            Self::EmptyCollection(c) => Refresh::Collection(c.clone()),
            Self::CreateIndex { coll, .. } | Self::DropIndex { coll, .. } => {
                Refresh::Indexes(coll.clone())
            }
            Self::FlushDb(db) | Self::CreateKey { db, .. } | Self::EditKey { db, .. } => {
                Refresh::RedisDb(*db)
            }
        }
    }

    /// The status-bar message of its success.
    pub fn success(&self) -> String {
        let key = |k: &[u8]| redis::display_bytes(k);
        match self {
            Self::CreateCollection { db, name } => format!("Collection {db}.{name} créée"),
            Self::DropCollection(c) => format!("Collection {c} supprimée"),
            Self::EmptyCollection(c) => format!("Collection {c} vidée"),
            Self::DropDatabase(db) => format!("Base {db} supprimée"),
            Self::CreateIndex { coll, .. } => format!("Index créé sur {coll}"),
            Self::DropIndex { coll, name } => format!("Index {name} de {coll} supprimé"),
            Self::FlushDb(db) => format!("Base db{db} vidée"),
            Self::CreateKey { key: k, .. } => format!("Clé {} créée", key(k)),
            Self::EditKey { key: k, edits, .. } => match edits.as_slice() {
                [KeyEdit::Delete] => format!("Clé {} supprimée", key(k)),
                [KeyEdit::Rename { to }] => format!("Clé {} renommée en {}", key(k), key(to)),
                [KeyEdit::Expire(None)] => format!("Clé {} rendue persistante", key(k)),
                [KeyEdit::Expire(Some(s))] => format!("Clé {} : expire dans {s} s", key(k)),
                _ => format!("Clé {} modifiée", key(k)),
            },
        }
    }
}

/// The console command emptying `coll` (its name as a JSON string, so that
/// any character survives).
pub fn delete_all_command(coll: &CollectionRef) -> String {
    let name = serde_json::to_string(&coll.coll).unwrap_or_else(|_| "\"\"".into());
    format!("db.getCollection({name}).deleteMany({{}})")
}

async fn run_task(conn: &Conn, task: NoSqlTask) -> Result<String, String> {
    let success = task.success();
    match task {
        NoSqlTask::CreateCollection { db, name } => {
            mongo::create_collection(conn.mongo().map_err(err)?, &db, &name)
                .await
                .map_err(err)?
        }
        NoSqlTask::DropCollection(c) => mongo::drop_collection(conn.mongo().map_err(err)?, &c)
            .await
            .map_err(err)?,
        NoSqlTask::EmptyCollection(c) => {
            let mut scope = NoSqlScope {
                database: c.db.clone(),
            };
            let out = nosql::run_script(conn, &delete_all_command(&c), None, &mut scope, 1).await;
            if let Some(e) = out.into_iter().find_map(|o| o.outcome.result.err()) {
                return Err(e);
            }
        }
        NoSqlTask::DropDatabase(db) => mongo::drop_database(conn.mongo().map_err(err)?, &db)
            .await
            .map_err(err)?,
        NoSqlTask::CreateIndex {
            coll,
            keys,
            options,
        } => {
            let name = mongo::create_index(conn.mongo().map_err(err)?, &coll, keys, options)
                .await
                .map_err(err)?;
            return Ok(format!("Index {name} créé sur {coll}"));
        }
        NoSqlTask::DropIndex { coll, name } => {
            mongo::drop_index(conn.mongo().map_err(err)?, &coll, &name)
                .await
                .map_err(err)?
        }
        NoSqlTask::FlushDb(db) => redis::flush_db(conn.redis().map_err(err)?, db)
            .await
            .map_err(err)?,
        NoSqlTask::CreateKey {
            db,
            key,
            value,
            ttl,
        } => redis::create_key(conn.redis().map_err(err)?, db, &key, &value, ttl)
            .await
            .map_err(err)?,
        NoSqlTask::EditKey { db, key, edits } => {
            redis::apply_edits(conn.redis().map_err(err)?, db, &key, &edits)
                .await
                .map_err(err)?;
        }
    }
    Ok(success)
}

/// SCAN `db` from the start until `max` keys are found or the scan ends:
/// the keys and the cursor where it stopped (0: every key listed).
async fn scan_up_to(conn: &Conn, db: u8, pattern: &str, max: usize) -> Result<ScanPage, String> {
    let client = conn.redis().map_err(err)?;
    let mut page = ScanPage::default();
    loop {
        let step = redis::scan(client, db, pattern, None, page.cursor, SCAN_COUNT)
            .await
            .map_err(err)?;
        page.keys.extend(step.keys);
        page.cursor = step.cursor;
        if page.cursor == 0 || page.keys.len() >= max {
            return Ok(page);
        }
    }
}

impl Worker {
    /// Run the units of `range` (all when `None`) of a NoSQL console in
    /// `scope`; the event carries the scope as the script left it.
    pub fn nosql_script(
        &mut self,
        tab: TabId,
        conn: Conn,
        text: String,
        range: Option<Range<usize>>,
        mut scope: NoSqlScope,
        max_rows: usize,
    ) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcomes = nosql::run_script(&conn, &text, range, &mut scope, max_rows).await;
            Event::NoSqlScript {
                tab,
                run,
                outcomes,
                scope,
            }
        });
        run
    }

    /// One page of documents (« Documents » tab).
    pub fn find_page(&mut self, tab: TabId, conn: Conn, q: DocQuery) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcome = match conn.mongo() {
                Ok(c) => mongo::find_page(c, &q).await.map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::DocPage { tab, run, outcome }
        });
        run
    }

    /// The documents matching `q`'s filter.
    pub fn doc_count(&mut self, tab: TabId, conn: Conn, q: DocQuery) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = match conn.mongo() {
                Ok(c) => mongo::count(c, &q).await.map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::Count { tab, run, outcome }
        });
        run
    }

    /// Run an aggregation pipeline (« Documents » tab, aggregation mode).
    pub fn aggregate(
        &mut self,
        tab: TabId,
        conn: Conn,
        coll: CollectionRef,
        pipeline: String,
        max_rows: usize,
    ) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcome = match conn.mongo() {
                Ok(c) => mongo::aggregate(c, &coll, &pipeline, max_rows)
                    .await
                    .map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::Aggregated { tab, run, outcome }
        });
        run
    }

    /// Apply document writes (a Submit, the JSON editor); `Submitted`
    /// carries how many were applied.
    pub fn apply_doc_changes(
        &mut self,
        tab: TabId,
        conn: Conn,
        coll: CollectionRef,
        changes: Vec<DocChange>,
    ) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = match conn.mongo() {
                Ok(c) => mongo::apply_changes(c, &coll, &changes).await.map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::Submitted { tab, run, outcome }
        });
        run
    }

    /// One SCAN step of the « Clés » tab.
    #[allow(clippy::too_many_arguments)]
    pub fn scan_keys(
        &mut self,
        tab: TabId,
        conn: Conn,
        db: u8,
        pattern: String,
        kind: Option<RedisType>,
        cursor: u64,
    ) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcome = match conn.redis() {
                Ok(c) => redis::scan(c, db, &pattern, kind.as_ref(), cursor, SCAN_COUNT)
                    .await
                    .map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::KeyPage { tab, run, outcome }
        });
        run
    }

    /// The type / TTL and one page of the value of `key`.
    pub fn read_key(
        &mut self,
        tab: TabId,
        conn: Conn,
        db: u8,
        key: Vec<u8>,
        from: u64,
        limit: usize,
    ) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = async {
                let c = conn.redis().map_err(err)?;
                let Some(info) = redis::key_info(c, db, &key).await.map_err(err)? else {
                    return Ok(None);
                };
                let page = redis::read_value(c, db, &key, from, limit)
                    .await
                    .map_err(err)?;
                Ok(Some((info, page)))
            }
            .await;
            Event::KeyValue {
                tab,
                run,
                key,
                outcome,
            }
        });
        run
    }

    /// Apply edits to the value of `key`; `Submitted` carries how many.
    pub fn apply_key_edits(
        &mut self,
        tab: TabId,
        conn: Conn,
        db: u8,
        key: Vec<u8>,
        edits: Vec<KeyEdit>,
    ) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = match conn.redis() {
                Ok(c) => redis::apply_edits(c, db, &key, &edits).await.map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::Submitted { tab, run, outcome }
        });
        run
    }

    /// The keys of Redis database `db` the explorer shows (at most
    /// `EXPLORER_MAX_KEYS`).
    pub fn explorer_keys(&mut self, name: String, conn: Conn, db: u8) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = scan_up_to(&conn, db, "*", EXPLORER_MAX_KEYS).await;
            Event::Keys {
                name,
                db,
                run,
                outcome,
            }
        });
        run
    }

    /// The indexes of `coll` (index dialog).
    pub fn indexes(&mut self, name: String, conn: Conn, coll: CollectionRef) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = match conn.mongo() {
                Ok(c) => mongo::list_indexes(c, &coll).await.map_err(err),
                Err(e) => Err(err(e)),
            };
            Event::Indexes {
                name,
                coll,
                run,
                outcome,
            }
        });
        run
    }

    /// Run an administration task of connection `name`.
    pub fn nosql_task(&self, name: String, conn: Conn, task: NoSqlTask) {
        self.spawn(async move {
            let refresh = task.refresh();
            let outcome = run_task(&conn, task).await;
            Event::NoSqlDone {
                name,
                refresh,
                outcome,
            }
        });
    }

    /// Export each collection to `dir/<db>.<coll>.<ext>` (`filter` applies
    /// to all of them).
    pub fn export_collections(
        &mut self,
        name: String,
        conn: Conn,
        colls: Vec<CollectionRef>,
        dir: PathBuf,
        format: TransferFormat,
        filter: String,
    ) -> OpId {
        let op = self.next_run();
        let mut progress = self.progress(op);
        self.spawn(async move {
            let mut report = BatchReport {
                total: colls.len(),
                ..Default::default()
            };
            match (tokio::fs::create_dir_all(&dir).await, conn.mongo()) {
                (Err(e), _) => report.errors.push(format!("{}: {e}", dir.display())),
                (_, Err(e)) => report.errors.push(err(e)),
                (Ok(()), Ok(c)) => {
                    for coll in &colls {
                        let path = dir.join(export_file_name(coll, format));
                        match mongo::export_collection(
                            c,
                            coll,
                            &filter,
                            &path,
                            format,
                            &mut progress,
                        )
                        .await
                        {
                            Ok(n) => {
                                report.succeeded += 1;
                                report.rows_affected += n as u64;
                            }
                            Err(e) => report.errors.push(format!("{coll}: {e}")),
                        }
                    }
                }
            }
            Event::Batch {
                op,
                name,
                kind: "Export",
                report,
            }
        });
        op
    }

    /// Import each file into its collection (format by extension).
    pub fn import_collections(
        &mut self,
        name: String,
        conn: Conn,
        files: Vec<(PathBuf, CollectionRef)>,
        opts: ImportOptions,
    ) -> OpId {
        let op = self.next_run();
        let mut progress = self.progress(op);
        self.spawn(async move {
            let mut report = BatchReport {
                total: files.len(),
                ..Default::default()
            };
            match conn.mongo() {
                Err(e) => report.errors.push(err(e)),
                Ok(c) => {
                    for (path, coll) in &files {
                        let Some(format) = TransferFormat::from_path(path) else {
                            report
                                .errors
                                .push(format!("{} : format inconnu", path.display()));
                            continue;
                        };
                        match mongo::import_collection(c, coll, path, format, &opts, &mut progress)
                            .await
                        {
                            Ok(stats) => {
                                report.rows_affected += stats.succeeded() as u64;
                                match stats.errors.first() {
                                    None => report.succeeded += 1,
                                    Some(e) => report.errors.push(format!(
                                        "{coll} : {} erreur(s), première : {e}",
                                        stats.errors.len()
                                    )),
                                }
                            }
                            Err(e) => report.errors.push(format!("{coll}: {e}")),
                        }
                    }
                }
            }
            Event::Batch {
                op,
                name,
                kind: "Import",
                report,
            }
        });
        op
    }

    /// Export the keys of `db` matching `pattern` to `path`.
    pub fn export_keys(
        &mut self,
        name: String,
        conn: Conn,
        db: u8,
        pattern: String,
        path: PathBuf,
        format: TransferFormat,
    ) -> OpId {
        let op = self.next_run();
        let mut progress = self.progress(op);
        self.spawn(async move {
            let mut report = BatchReport {
                total: 1,
                ..Default::default()
            };
            let outcome = match conn.redis() {
                Ok(c) => redis::export_keys(c, db, &pattern, &path, format, &mut progress)
                    .await
                    .map_err(err),
                Err(e) => Err(err(e)),
            };
            match outcome {
                Ok(n) => {
                    report.succeeded = 1;
                    report.rows_affected = n as u64;
                }
                Err(e) => report.errors.push(format!("db{db}: {e}")),
            }
            Event::Batch {
                op,
                name,
                kind: "Export",
                report,
            }
        });
        op
    }

    /// Import the keys of `path` into `db`.
    pub fn import_keys(
        &mut self,
        name: String,
        conn: Conn,
        db: u8,
        path: PathBuf,
        opts: ImportOptions,
    ) -> OpId {
        let op = self.next_run();
        let mut progress = self.progress(op);
        self.spawn(async move {
            let mut report = BatchReport {
                total: 1,
                ..Default::default()
            };
            let outcome = match (conn.redis(), TransferFormat::from_path(&path)) {
                (Err(e), _) => Err(err(e)),
                (_, None) => Err(format!("{} : format inconnu", path.display())),
                (Ok(c), Some(format)) => {
                    redis::import_keys(c, db, &path, format, &opts, &mut progress)
                        .await
                        .map_err(err)
                }
            };
            match outcome {
                Ok(stats) => {
                    report.rows_affected = stats.succeeded() as u64;
                    match stats.errors.first() {
                        None => report.succeeded = 1,
                        Some(e) => report.errors.push(format!(
                            "db{db} : {} erreur(s), première : {e}",
                            stats.errors.len()
                        )),
                    }
                }
                Err(e) => report.errors.push(format!("db{db}: {e}")),
            }
            Event::Batch {
                op,
                name,
                kind: "Import",
                report,
            }
        });
        op
    }
}

/// `<db>.<coll>.<ext>`, made safe for the file system.
pub fn export_file_name(coll: &CollectionRef, format: TransferFormat) -> String {
    use crate::engine::services::export_import::safe_file_stem;
    format!(
        "{}.{}",
        safe_file_stem(&format!("{}.{}", coll.db, coll.coll)),
        format.extension()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll(db: &str, c: &str) -> CollectionRef {
        CollectionRef {
            db: db.into(),
            coll: c.into(),
        }
    }

    #[test]
    fn tasks_say_what_they_make_stale() {
        let c = coll("shop", "users");
        assert_eq!(
            NoSqlTask::DropCollection(c.clone()).refresh(),
            Refresh::Schemas
        );
        assert_eq!(
            NoSqlTask::EmptyCollection(c.clone()).refresh(),
            Refresh::Collection(c.clone())
        );
        assert_eq!(
            NoSqlTask::DropIndex {
                coll: c.clone(),
                name: "a_1".into()
            }
            .refresh(),
            Refresh::Indexes(c.clone())
        );
        assert_eq!(NoSqlTask::FlushDb(3).refresh(), Refresh::RedisDb(3));
        let delete = NoSqlTask::EditKey {
            db: 0,
            key: b"user:1".to_vec(),
            edits: vec![KeyEdit::Delete],
        };
        assert_eq!(delete.refresh(), Refresh::RedisDb(0));
        assert_eq!(delete.success(), "Clé user:1 supprimée");
        assert_eq!(
            NoSqlTask::EmptyCollection(c).success(),
            "Collection shop.users vidée"
        );
    }

    #[test]
    fn emptying_a_collection_quotes_its_name() {
        assert_eq!(
            delete_all_command(&coll("shop", "fs.chunks")),
            r#"db.getCollection("fs.chunks").deleteMany({})"#
        );
        assert_eq!(
            delete_all_command(&coll("shop", "a\"b")),
            r#"db.getCollection("a\"b").deleteMany({})"#
        );
    }

    #[test]
    fn export_files_are_named_after_the_collection() {
        assert_eq!(
            export_file_name(&coll("shop", "users"), TransferFormat::JsonLines),
            "shop.users.jsonl"
        );
        assert_eq!(
            export_file_name(&coll("shop", "a/b"), TransferFormat::Csv),
            "shop.a_b.csv"
        );
    }

    #[test]
    fn engine_errors_come_back_as_events() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let conn =
            std::sync::Arc::new(rt.block_on(crate::engine::ops::test_support::sqlite_mem(&[])));
        let mut w = Worker::new(|| {});
        // A SQL connection has no MongoDB / Redis client: an error, no panic.
        let id = w.find_page(
            1,
            conn.clone(),
            DocQuery {
                coll: coll("a", "b"),
                filter: String::new(),
                sort: String::new(),
                projection: String::new(),
                skip: 0,
                limit: 10,
            },
        );
        let Event::DocPage { tab, run, outcome } = super::super::tests::next_event(&mut w) else {
            panic!("expected DocPage")
        };
        assert_eq!((tab, run), (1, id));
        assert!(outcome.is_err());
        w.read_key(2, conn, 0, b"k".to_vec(), 0, 10);
        let Event::KeyValue { tab, outcome, .. } = super::super::tests::next_event(&mut w) else {
            panic!("expected KeyValue")
        };
        assert_eq!(tab, 2);
        assert!(outcome.is_err());
    }

    /// The first event `pick` accepts (others are skipped), within 30 s.
    fn wait_for<T>(w: &mut Worker, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        let start = std::time::Instant::now();
        loop {
            for ev in w.poll() {
                if let Some(t) = pick(ev) {
                    return t;
                }
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "no event"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Open `config` through the worker, as the app does.
    fn open(w: &mut Worker, config: crate::engine::models::ConnectionConfig) -> Conn {
        w.connect(config);
        wait_for(w, |ev| match ev {
            Event::Connected { conn, .. } => Some(conn),
            Event::ConnectFailed { error, .. } => panic!("connection: {error}"),
            _ => None,
        })
    }

    fn script(w: &mut Worker, conn: &Conn, text: &str, database: &str) -> Vec<NoSqlOutcomeView> {
        let scope = NoSqlScope {
            database: database.into(),
        };
        w.nosql_script(9, conn.clone(), text.into(), None, scope, 100);
        wait_for(w, |ev| match ev {
            Event::NoSqlScript { outcomes, .. } => Some(
                outcomes
                    .into_iter()
                    .map(|o| (o.outcome.result.map(|r| r.rows.len()), o.source.is_some()))
                    .collect(),
            ),
            _ => None,
        })
    }

    /// Rows (or the error) and whether a `Source` came with them.
    type NoSqlOutcomeView = (Result<usize, String>, bool);

    /// The worker against a MongoDB server: connection, a page, a Submit.
    /// `SU_IT_MONGO=mongodb://127.0.0.1:47017 cargo test -- --ignored integration_mongo_worker`
    #[test]
    #[ignore]
    fn integration_mongo_worker() {
        let Ok(uri) = std::env::var("SU_IT_MONGO") else {
            eprintln!("SU_IT_MONGO not set: skipped");
            return;
        };
        let (uri, password) = crate::engine::presets::split_uri_password(&uri);
        let mut w = Worker::new(|| {});
        let conn = open(
            &mut w,
            crate::engine::models::ConnectionConfig {
                name: "it".into(),
                db_type: crate::engine::models::DatabaseType::MongoDB,
                uri: Some(uri),
                // Not the default "postgres": the URI carries the user.
                username: None,
                password,
                database: "su_it_gui".into(),
                ..Default::default()
            },
        );
        // Whether it existed or not.
        script(
            &mut w,
            &conn,
            "db.getCollection(\"docs\").drop()",
            "su_it_gui",
        );
        let out = script(
            &mut w,
            &conn,
            "db.getCollection(\"docs\").insertMany([{_id: 1, a: 1}, {_id: 2, a: 2}])",
            "su_it_gui",
        );
        assert!(out.iter().all(|(r, _)| r.is_ok()), "{out:?}");
        let coll = coll("su_it_gui", "docs");
        let q = DocQuery {
            coll: coll.clone(),
            filter: String::new(),
            sort: "{_id: 1}".into(),
            projection: String::new(),
            skip: 0,
            limit: 500,
        };
        w.find_page(1, conn.clone(), q.clone());
        let page = wait_for(&mut w, |ev| match ev {
            Event::DocPage { outcome, .. } => Some(outcome.expect("page")),
            _ => None,
        });
        assert_eq!(page.docs.len(), 2);
        // The edit of a cell, as the « Documents » tab submits it.
        let a = page
            .result
            .columns
            .iter()
            .position(|c| c.name == "a")
            .unwrap();
        let mut edits = crate::gui::grid::changes::PendingEdits::default();
        edits.set(
            &page.result.rows,
            crate::gui::grid::changes::RowRef::Base(0),
            a,
            "10".into(),
        );
        let writes = mongo::doc_changes(&page, &edits.to_row_changes(&page.result.rows)).unwrap();
        w.apply_doc_changes(1, conn.clone(), coll, writes);
        let applied = wait_for(&mut w, |ev| match ev {
            Event::Submitted { outcome, .. } => Some(outcome),
            _ => None,
        });
        assert_eq!(applied, Ok(1));
        w.find_page(1, conn, q);
        let page = wait_for(&mut w, |ev| match ev {
            Event::DocPage { outcome, .. } => Some(outcome.expect("page")),
            _ => None,
        });
        assert_eq!(page.result.rows[0][a], "10");
    }

    /// The worker against a Redis server: connection, keys, a value, a
    /// write. `SU_IT_REDIS=redis://127.0.0.1:46379 cargo test -- --ignored
    /// integration_redis_worker`
    #[test]
    #[ignore]
    fn integration_redis_worker() {
        let Ok(url) = std::env::var("SU_IT_REDIS") else {
            eprintln!("SU_IT_REDIS not set: skipped");
            return;
        };
        let p = crate::engine::presets::parse_url(&url).unwrap();
        let mut w = Worker::new(|| {});
        let conn = open(
            &mut w,
            crate::engine::models::ConnectionConfig {
                name: "it".into(),
                db_type: crate::engine::models::DatabaseType::Redis,
                host: p.host,
                port: p.port,
                username: p.username,
                password: p.password,
                database: "15".into(),
                ssl_mode: p.ssl_mode,
                ..Default::default()
            },
        );
        let out = script(
            &mut w,
            &conn,
            "DEL su_it:gui:l\nRPUSH su_it:gui:l a b",
            "15",
        );
        assert!(out.iter().all(|(r, _)| r.is_ok()), "{out:?}");
        w.scan_keys(1, conn.clone(), 15, "su_it:gui:*".into(), None, 0);
        let mut keys = Vec::new();
        loop {
            let page = wait_for(&mut w, |ev| match ev {
                Event::KeyPage { outcome, .. } => Some(outcome.expect("scan")),
                _ => None,
            });
            keys.extend(page.keys);
            let cursor = page.cursor;
            if cursor == 0 {
                break;
            }
            w.scan_keys(1, conn.clone(), 15, "su_it:gui:*".into(), None, cursor);
        }
        assert!(keys.iter().any(|k| k.key == b"su_it:gui:l"));
        w.apply_key_edits(
            1,
            conn.clone(),
            15,
            b"su_it:gui:l".to_vec(),
            vec![KeyEdit::ListPush {
                left: false,
                value: b"c".to_vec(),
            }],
        );
        let applied = wait_for(&mut w, |ev| match ev {
            Event::Submitted { outcome, .. } => Some(outcome),
            _ => None,
        });
        assert_eq!(applied, Ok(1));
        w.read_key(1, conn, 15, b"su_it:gui:l".to_vec(), 0, 500);
        let (info, page) = wait_for(&mut w, |ev| match ev {
            Event::KeyValue { outcome, .. } => Some(outcome.expect("read").expect("exists")),
            _ => None,
        });
        assert_eq!(info.kind, RedisType::List);
        assert_eq!(page.total, 3);
    }
}
