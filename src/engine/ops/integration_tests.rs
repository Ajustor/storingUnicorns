//! Integration tests against real Postgres / MySQL / SQL Server servers.
//!
//! Every test is `#[ignore]` and returns early when its environment variable
//! is missing, so a plain `cargo test` stays offline. Run them with
//! `cargo test -- --ignored integration` and:
//!
//! - `SU_IT_PG=postgres://user:pw@host:port/db` (a superuser: the Google
//!   Cloud IAM test creates a role)
//! - `SU_IT_MYSQL=mysql://user:pw@host:port/db`
//! - `SU_IT_MSSQL=host=127.0.0.1;port=1433;user=sa;password=...;database=db`
//!   (the database must exist)
//! - `SU_IT_MARIADB=mysql://user:pw@host:port/db`
//! - `SU_IT_COCKROACH=postgres://root@host:port/defaultdb` (insecure mode)
//! - `SU_IT_PG_TLS=host:port` + `SU_IT_PG_TLS_CA=<ca.pem>`: a PostgreSQL that
//!   only accepts TLS (`hostssl` + `hostnossl ... reject`), user `postgres`,
//!   password `a@b:c/d#e?f%g`, certificate for `localhost` signed by the CA.
//!
//! Servers for the last three (from Git Bash on Windows, prefix the
//! CockroachDB line with `MSYS_NO_PATHCONV=1`):
//!
//! ```text
//! docker run -d --name su-it-mariadb -p 43307:3306 -e MARIADB_ROOT_PASSWORD=pw -e MARIADB_DATABASE=it mariadb:11
//! docker run -d --name su-it-crdb -p 46257:26257 cockroachdb/cockroach:latest start-single-node --insecure
//! scripts/it-pg-tls.sh   # generates the CA + server cert, starts su-it-pg-tls on 45433
//! ```
//!
//! The tests (re)create their own tables in schema `su_it` (MySQL: in the
//! connection's database), so they can be run repeatedly.

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{ConnectionConfig, DatabaseType, SslMode, NULL_CELL};
use crate::engine::ops::query::run_script;
use crate::engine::ops::rows::{
    detect_system_columns, submit_changes, transaction_bounds, RowChanges,
};
use crate::engine::ops::schema::{table_ddl, table_details};
use crate::engine::ops::transfer::{import_csv, qualified};
use crate::engine::presets::parse_url;
use crate::engine::services::table_cache::TableCache;
use crate::engine::sql::paging::{build_count, build_select, DataQuery};
use crate::engine::sql::statements::quote_chars;

/// One server under test.
struct Dialect {
    db: DatabaseType,
    /// Schema holding the test tables (MySQL: the database).
    schema: String,
    /// Drops then creates `parent` and `child`, in order.
    setup: Vec<String>,
}

impl Dialect {
    fn quotes(&self) -> (char, char) {
        quote_chars(&self.db)
    }

    /// `"schema"."table"` as the GUI passes it.
    fn q(&self, table: &str) -> String {
        qualified(&self.schema, table, self.quotes())
    }

    /// A quoted column name.
    fn col(&self, name: &str) -> String {
        let (a, b) = self.quotes();
        format!("{a}{name}{b}")
    }
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> crate::engine::models::QueryResult {
    conn.execute_query(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Single value of a one-row, one-column query.
async fn scalar(conn: &DatabaseConnection, sql: &str) -> String {
    exec(conn, sql).await.rows[0][0].clone()
}

/// Cells of a new row: `None` cells are left to their defaults.
fn new_row(cells: &[Option<&str>]) -> Vec<Option<String>> {
    cells.iter().map(|c| c.map(str::to_string)).collect()
}

/// Rows of `parent` as `(a, name)`, ordered by `a`.
async fn parent_rows(conn: &DatabaseConnection, d: &Dialect) -> Vec<(String, String)> {
    exec(
        conn,
        &format!("SELECT a, name FROM {} ORDER BY a", d.q("parent")),
    )
    .await
    .rows
    .into_iter()
    .map(|r| (r[0].clone(), r[1].clone()))
    .collect()
}

/// The whole scenario, shared by every server.
async fn exercise(conn: &DatabaseConnection, d: &Dialect) {
    for sql in &d.setup {
        exec(conn, sql).await;
    }
    let parent = d.q("parent");
    let child = d.q("child");
    exec(
        conn,
        &format!(
            "INSERT INTO {parent} (a, b, name) VALUES (1, 10, 'p1'), (2, 20, 'p2'), (3, 30, 'p3')"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO {child} (ca, cb, note) VALUES (10, 1, 'c1'), (20, 2, 'c2'), (30, 3, 'c3')"
        ),
    )
    .await;

    // --- DML affected-row counts ---
    let r = exec(
        conn,
        &format!("UPDATE {parent} SET name = name WHERE a <= 2"),
    )
    .await;
    assert_eq!(r.rows_affected, 2, "UPDATE of 2 rows (values unchanged)");
    let r = exec(
        conn,
        &format!("DELETE FROM {child} WHERE note IN ('c2', 'c3')"),
    )
    .await;
    assert_eq!(r.rows_affected, 2, "DELETE of 2 rows");

    // --- Row cap ---
    let select = format!("SELECT * FROM {parent} ORDER BY a");
    let r = conn.execute_query_limited(&select, Some(2)).await.unwrap();
    assert_eq!(r.rows.len(), 2);
    assert!(r.truncated);
    let r = conn.execute_query_limited(&select, Some(3)).await.unwrap();
    assert_eq!(r.rows.len(), 3);
    assert!(!r.truncated);
    // The connection is still usable after a capped read.
    assert_eq!(
        scalar(conn, &format!("SELECT COUNT(*) FROM {parent}")).await,
        "3"
    );

    // --- run_script: several statements, each its own outcome ---
    let script = format!(
        "INSERT INTO {parent} (a, b, name) VALUES (4, 40, 'p4');\n\
         UPDATE {parent} SET name = 'y' WHERE a = 4;\n\
         SELECT name FROM {parent} WHERE a = 4"
    );
    let out = run_script(conn, &script, Some(100)).await;
    for o in &out {
        assert!(o.result.is_ok(), "{}: {:?}", o.sql, o.result);
    }
    // SQL Server runs the script as one T-SQL batch: an outcome per result
    // set (see `integration_mssql_script_batches`).
    let tsql = matches!(d.db, DatabaseType::SQLServer | DatabaseType::Azure);
    if !tsql {
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].result.as_ref().unwrap().rows_affected, 1);
        assert_eq!(out[1].result.as_ref().unwrap().rows_affected, 1);
    }
    assert_eq!(
        out.last().unwrap().result.as_ref().unwrap().rows,
        vec![vec!["y".to_string()]]
    );
    let out = run_script(
        conn,
        "SELECT 1; SELECT * FROM su_it_missing_table; SELECT 2",
        None,
    )
    .await;
    let failing = if tsql { 0 } else { 1 };
    assert_eq!(
        out.len(),
        failing + 1,
        "run_script stops after the first error"
    );
    assert!(out[failing].result.is_err());

    // --- Transaction blocks ---
    let (begin, commit) = transaction_bounds(&d.db);
    let block = vec![
        begin.to_string(),
        format!("INSERT INTO {parent} (a, b, name) VALUES (5, 50, 'p5')"),
        commit.to_string(),
    ];
    conn.execute_transaction(&block).await.unwrap();
    let block = vec![
        begin.to_string(),
        format!("INSERT INTO {parent} (a, b, name) VALUES (6, 60, 'p6')"),
        // Duplicate (a, b): unique violation.
        format!("INSERT INTO {parent} (a, b, name) VALUES (1, 10, 'dup')"),
        commit.to_string(),
    ];
    assert!(conn.execute_transaction(&block).await.is_err());
    let count = format!("SELECT COUNT(*) FROM {parent} WHERE a IN (5, 6)");
    assert_eq!(
        scalar(conn, &count).await,
        "1",
        "block with an error must roll back"
    );
    // Nothing left open on the connection.
    exec(
        conn,
        &format!("INSERT INTO {parent} (a, b, name) VALUES (9, 90, 'p9')"),
    )
    .await;
    exec(conn, &format!("DELETE FROM {parent} WHERE a = 9")).await;
    assert_eq!(
        scalar(conn, &format!("SELECT COUNT(*) FROM {parent} WHERE a = 9")).await,
        "0"
    );

    // --- Metadata, with qualified quoted names ---
    let schemas = conn.get_tables_by_schema().await.unwrap();
    let ours = schemas
        .iter()
        .find(|s| s.name == d.schema)
        .unwrap_or_else(|| panic!("schema {} not listed", d.schema));
    assert!(ours.tables.iter().any(|t| t == "parent"));
    assert!(ours.tables.iter().any(|t| t == "child"));

    let unquoted = format!("{}.parent", d.schema);
    for name in [parent.as_str(), unquoted.as_str()] {
        let cols = conn.get_table_column_details(name).await.unwrap();
        let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "a", "b", "name"], "columns of {name}");
        assert!(cols[0].is_primary_key && !cols[1].is_primary_key);
        assert!(!cols[1].nullable && cols[3].nullable);
        let nullability = conn.get_column_nullability(name).await.unwrap();
        assert_eq!(nullability.get("a"), Some(&false), "nullability of {name}");
        assert_eq!(nullability.get("name"), Some(&true));
        assert_eq!(
            conn.get_primary_keys(name).await.unwrap(),
            ["id"],
            "pk of {name}"
        );
    }

    let indexes = conn.get_indexes(&parent).await.unwrap();
    let pk = indexes.iter().find(|i| i.primary).expect("primary index");
    assert_eq!(pk.columns, ["id"]);
    let uq = indexes
        .iter()
        .find(|i| i.name == "parent_ab_uq")
        .unwrap_or_else(|| panic!("unique index missing: {indexes:?}"));
    assert!(uq.unique && !uq.primary);
    assert_eq!(uq.columns, ["a", "b"]);
    let child_indexes = conn.get_indexes(&child).await.unwrap();
    let ix = child_indexes
        .iter()
        .find(|i| i.name == "child_note_ix")
        .unwrap_or_else(|| panic!("index missing: {child_indexes:?}"));
    assert!(!ix.unique);
    assert_eq!(ix.columns, ["note"]);

    let fks = conn.get_foreign_keys(&child).await.unwrap();
    assert_eq!(fks.len(), 1, "{fks:?}");
    assert_eq!(fks[0].name, "child_parent_fk");
    assert_eq!(fks[0].columns, ["cb", "ca"], "FK columns in key order");
    assert_eq!(fks[0].ref_columns, ["a", "b"]);
    // Qualified even in the table's own schema.
    assert_eq!(fks[0].ref_table, format!("{}.parent", d.schema));

    let details = table_details(conn, &TableCache::default(), &child)
        .await
        .unwrap();
    assert_eq!(details.columns.len(), 4);
    assert_eq!(details.foreign_keys, fks);

    // --- DDL: re-create both tables under other names from it ---
    let cache = TableCache::default();
    let mut ddl = String::new();
    for table in [&parent, &child] {
        let one = table_ddl(conn, &cache, &d.db, table).await.unwrap();
        assert!(one.contains("CREATE TABLE"), "{one}");
        ddl.push_str(&one);
        ddl.push('\n');
    }
    assert!(
        ddl.contains("parent_ab_uq") && ddl.contains("child_note_ix"),
        "{ddl}"
    );
    assert!(ddl.contains("child_parent_fk"), "{ddl}");
    let copy = ddl.replace("parent", "pcopy").replace("child", "ccopy");
    let out = run_script(conn, &copy, None).await;
    for o in &out {
        assert!(
            o.result.is_ok(),
            "DDL not executable: {}: {:?}",
            o.sql,
            o.result
        );
    }
    let copy_fks = conn.get_foreign_keys(&d.q("ccopy")).await.unwrap();
    assert_eq!(copy_fks[0].columns, ["cb", "ca"]);
    assert_eq!(copy_fks[0].ref_columns, ["a", "b"]);
    assert_eq!(
        conn.get_indexes(&d.q("pcopy"))
            .await
            .unwrap()
            .iter()
            .filter(|i| i.unique)
            .count(),
        2,
        "pk + unique index re-created"
    );
    let cols = conn.get_table_column_details(&d.q("pcopy")).await.unwrap();
    let orig = conn.get_table_column_details(&parent).await.unwrap();
    let types = |c: &[crate::engine::models::Column]| {
        c.iter()
            .map(|c| (c.name.clone(), c.type_name.clone(), c.nullable))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        types(&cols),
        types(&orig),
        "re-created columns keep their types"
    );
    exec(conn, &format!("DROP TABLE {}", d.q("ccopy"))).await;
    exec(conn, &format!("DROP TABLE {}", d.q("pcopy"))).await;

    // --- Paging, executed for real ---
    let mut page = DataQuery {
        table: parent.clone(),
        filter: "a >= 2".into(),
        order_by: format!("{} DESC", d.col("a")),
        page: 0,
        page_size: 2,
    };
    let r = exec(conn, &build_select(&page, &d.db)).await;
    let a_idx = r.columns.iter().position(|c| c.name == "a").unwrap();
    let firsts: Vec<_> = r.rows.iter().map(|r| r[a_idx].clone()).collect();
    assert_eq!(firsts, ["5", "4"]);
    page.page = 1;
    let r = exec(conn, &build_select(&page, &d.db)).await;
    let seconds: Vec<_> = r.rows.iter().map(|r| r[a_idx].clone()).collect();
    assert_eq!(seconds, ["3", "2"]);
    page.order_by.clear();
    page.page = 0;
    assert_eq!(exec(conn, &build_select(&page, &d.db)).await.rows.len(), 2);
    assert_eq!(scalar(conn, &build_count(&page)).await, "4");

    // --- submit_changes ---
    let columns = conn.get_table_column_details(&parent).await.unwrap();
    let system = detect_system_columns(&columns);
    assert_eq!(system, [0], "id is generated");
    let rows = exec(
        conn,
        &format!("SELECT id, a, b, name FROM {parent} ORDER BY a"),
    )
    .await
    .rows;
    let row_of = |a: &str| rows.iter().find(|r| r[1] == a).unwrap().clone();
    let mut edited = row_of("1");
    edited[3] = "S1".into();
    let mut nulled = row_of("2");
    nulled[3] = NULL_CELL.into();
    let changes = RowChanges {
        updates: vec![
            (row_of("1"), edited),
            (row_of("2"), nulled),
            (row_of("3"), row_of("3")), // no-op, skipped
        ],
        inserts: vec![new_row(&[None, Some("7"), Some("70"), Some("p'7")])],
        deletes: vec![row_of("5")],
    };
    let applied = submit_changes(conn, &d.db, &parent, &columns, &changes)
        .await
        .unwrap();
    assert_eq!(applied, 4);
    assert_eq!(
        parent_rows(conn, d).await,
        [
            ("1".into(), "S1".into()),
            ("2".into(), NULL_CELL.into()),
            ("3".into(), "p3".into()),
            ("4".into(), "y".into()),
            ("7".into(), "p'7".into()),
        ]
    );
    // An error rolls the whole submit back.
    let rows = exec(
        conn,
        &format!("SELECT id, a, b, name FROM {parent} ORDER BY a"),
    )
    .await
    .rows;
    let mut edited = rows[2].clone();
    edited[3] = "bad".into();
    let changes = RowChanges {
        updates: vec![(rows[2].clone(), edited)],
        inserts: vec![new_row(&[None, Some("1"), Some("10"), Some("dup")])],
        deletes: vec![rows[3].clone()],
    };
    assert!(submit_changes(conn, &d.db, &parent, &columns, &changes)
        .await
        .is_err());
    let after = parent_rows(conn, d).await;
    assert_eq!(after.len(), 5, "delete rolled back");
    assert_eq!(after[2], ("3".into(), "p3".into()), "update rolled back");
    // A row deleted since it was loaded: the submit fails and nothing of it
    // is applied, the connection stays usable.
    let rows = exec(
        conn,
        &format!("SELECT id, a, b, name FROM {parent} ORDER BY a"),
    )
    .await
    .rows;
    let mut ghost = rows[0].clone();
    ghost[0] = "987654".into();
    let mut ghost_edit = ghost.clone();
    ghost_edit[3] = "ghost".into();
    let mut edited = rows[1].clone();
    edited[3] = "changed".into();
    let changes = RowChanges {
        updates: vec![(rows[1].clone(), edited), (ghost, ghost_edit)],
        inserts: vec![new_row(&[None, Some("6"), Some("60"), Some("p6")])],
        deletes: vec![rows[2].clone()],
    };
    let err = submit_changes(conn, &d.db, &parent, &columns, &changes)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("(0 ligne)"), "{err}");
    assert_eq!(parent_rows(conn, d).await, after, "nothing applied");
    // A key that is not unique for the database ((b, name) presented as the
    // key).
    let by_a: Vec<_> = columns
        .iter()
        .map(|c| crate::engine::models::Column {
            is_primary_key: c.name == "b" || c.name == "name",
            ..c.clone()
        })
        .collect();
    exec(
        conn,
        &format!("INSERT INTO {parent} (a, b, name) VALUES (70, 30, 'p3')"),
    )
    .await;
    let rows = exec(
        conn,
        &format!("SELECT id, a, b, name FROM {parent} WHERE b = 30 ORDER BY a"),
    )
    .await
    .rows;
    let mut edited = rows[0].clone();
    edited[3] = "both".into();
    let changes = RowChanges {
        updates: vec![(rows[0].clone(), edited)],
        ..Default::default()
    };
    let err = submit_changes(conn, &d.db, &parent, &by_a, &changes)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("(2 lignes)"), "{err}");
    assert_eq!(
        scalar(
            conn,
            &format!("SELECT COUNT(*) FROM {parent} WHERE name = 'both'")
        )
        .await,
        "0"
    );
    exec(conn, &format!("DELETE FROM {parent} WHERE a = 70")).await;

    // --- New rows: untouched cells get their defaults / identity ---
    let dflt = d.q("dflt");
    let dcols = conn.get_table_column_details(&dflt).await.unwrap();
    let changes = RowChanges {
        inserts: vec![
            new_row(&[None, None, None]),
            new_row(&[None, None, Some("7")]),
            new_row(&[None, Some("done"), Some(NULL_CELL)]),
        ],
        ..Default::default()
    };
    assert_eq!(
        submit_changes(conn, &d.db, &dflt, &dcols, &changes)
            .await
            .unwrap(),
        3
    );
    let r = exec(conn, &format!("SELECT status, n FROM {dflt} ORDER BY id")).await;
    assert_eq!(
        r.rows,
        [["new", "5"], ["new", "7"], ["done", NULL_CELL]],
        "defaults applied to untouched cells"
    );

    // --- CSV import (upsert): update by id, insert unknown id ---
    let id1 = scalar(conn, &format!("SELECT id FROM {parent} WHERE a = 1")).await;
    let csv = format!("id,a,b,name\n{id1},1,10,imported\n99999,8,80,new\n");
    let stats = import_csv(conn, &parent, &csv, d.quotes(), |_, _| {})
        .await
        .unwrap();
    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    assert_eq!((stats.updated, stats.inserted), (1, 1));
    let rows = parent_rows(conn, d).await;
    assert!(rows.contains(&("1".into(), "imported".into())));
    assert!(rows.contains(&("8".into(), "new".into())));
    // Re-importing an unchanged row must still count as an update.
    let stats = import_csv(
        conn,
        &parent,
        &csv[..csv.rfind("99999").unwrap()],
        d.quotes(),
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(
        (stats.updated, stats.inserted),
        (1, 0),
        "{:?}",
        stats.errors
    );
}

/// A transaction block interrupted while it runs (the GUI's cancel drops
/// the future) must leave nothing open: the next statement on `conn` is
/// committed on its own and the block's INSERT is rolled back, as seen from
/// the `other`, independent connection. `slow` runs for several seconds;
/// `no_open_tx` is a query returning `expected` when the session that runs
/// it has no open transaction.
async fn cancelled_transaction_leaves_nothing_open(
    conn: &DatabaseConnection,
    other: &DatabaseConnection,
    d: &Dialect,
    slow: &str,
    (no_open_tx, expected): (&str, &str),
) {
    let parent = d.q("parent");
    let (begin, commit) = transaction_bounds(&d.db);
    let block = vec![
        begin.to_string(),
        format!("INSERT INTO {parent} (a, b, name) VALUES (80, 800, 'cancelled')"),
        slow.to_string(),
        commit.to_string(),
    ];
    let run = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        conn.execute_transaction(&block),
    )
    .await;
    assert!(run.is_err(), "the block must be interrupted while running");
    let start = std::time::Instant::now();
    assert_eq!(
        scalar(conn, no_open_tx).await,
        expected,
        "no transaction open after the interruption"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(3),
        "the next query must not wait for the interrupted one"
    );

    exec(
        conn,
        &format!("INSERT INTO {parent} (a, b, name) VALUES (81, 810, 'after')"),
    )
    .await;
    let check = async {
        (
            scalar(
                other,
                &format!("SELECT COUNT(*) FROM {parent} WHERE a = 80"),
            )
            .await,
            scalar(
                other,
                &format!("SELECT COUNT(*) FROM {parent} WHERE a = 81"),
            )
            .await,
        )
    };
    let counts = tokio::time::timeout(std::time::Duration::from_secs(20), check)
        .await
        .expect("blocked by a transaction left open by the cancelled block");
    assert_eq!(
        counts,
        ("0".to_string(), "1".to_string()),
        "cancelled INSERT rolled back, later INSERT committed"
    );
    exec(conn, &format!("DELETE FROM {parent} WHERE a IN (80, 81)")).await;
}

/// Every `(expression, expected text)` decodes to the expected string.
async fn check_decoding(conn: &DatabaseConnection, cases: &[(&str, &str)]) {
    let select = cases
        .iter()
        .enumerate()
        .map(|(i, (expr, _))| format!("{expr} AS c{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let r = exec(conn, &format!("SELECT {select}")).await;
    let mut failures = Vec::new();
    for (i, (expr, expected)) in cases.iter().enumerate() {
        if r.rows[0][i] != *expected {
            failures.push(format!("{expr}: got {:?}, want {expected:?}", r.rows[0][i]));
        }
    }
    assert!(
        failures.is_empty(),
        "decoding failures:\n{}",
        failures.join("\n")
    );
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Password of the special-character tests: every URL delimiter.
const SPECIAL_PASSWORD: &str = "a@b:c/d#e?f%g";

/// The connection the dialog builds from `dsn` pasted as a URL.
fn config_from_url(dsn: &str) -> ConnectionConfig {
    let p = parse_url(dsn).unwrap_or_else(|e| panic!("{dsn}: {e}"));
    ConnectionConfig {
        name: "it".into(),
        db_type: p.db_type,
        host: p.host,
        port: p.port,
        username: p.username,
        password: p.password,
        database: p.database.unwrap_or_default(),
        ssl_mode: p.ssl_mode,
        ssl_ca: p.ssl_ca,
        flavor: p.flavor,
        ..Default::default()
    }
}

#[tokio::test]
#[ignore]
async fn integration_postgres() {
    let Some(dsn) = env("SU_IT_PG") else {
        eprintln!("SU_IT_PG not set, skipping");
        return;
    };
    let conn = DatabaseConnection::Postgres(sqlx::PgPool::connect(&dsn).await.unwrap());
    // One connection: a transaction leaked into the pool would be reused.
    let single = DatabaseConnection::Postgres(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&dsn)
            .await
            .unwrap(),
    );
    let d = Dialect {
        db: DatabaseType::Postgres,
        schema: "su_it".into(),
        setup: vec![
            "DROP SCHEMA IF EXISTS su_it CASCADE".into(),
            "CREATE SCHEMA su_it".into(),
            "CREATE TABLE su_it.parent (id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
             a INT NOT NULL, b INT NOT NULL, name VARCHAR(50))"
                .into(),
            "CREATE UNIQUE INDEX parent_ab_uq ON su_it.parent (a, b)".into(),
            "CREATE TABLE su_it.child (id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
             ca INT, cb INT, note VARCHAR(20), \
             CONSTRAINT child_parent_fk FOREIGN KEY (cb, ca) REFERENCES su_it.parent (a, b))"
                .into(),
            "CREATE INDEX child_note_ix ON su_it.child (note)".into(),
            "CREATE TYPE su_it.mood AS ENUM ('ok', 'ko')".into(),
            "CREATE TABLE su_it.dflt (id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
             status VARCHAR(10) NOT NULL DEFAULT 'new', n INT DEFAULT 5)"
                .into(),
        ],
    };
    exercise(&conn, &d).await;
    cancelled_transaction_leaves_nothing_open(
        &single,
        &conn,
        &d,
        "SELECT pg_sleep(5)",
        ("SELECT txid_current_if_assigned() IS NULL", "true"),
    )
    .await;
    // A function body full of `;` stays one statement.
    let out = run_script(
        &conn,
        "CREATE FUNCTION su_it.f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;\n\
         SELECT su_it.f() AS v",
        None,
    )
    .await;
    assert_eq!(out.len(), 2);
    assert_eq!(
        out[1].result.as_ref().unwrap().rows,
        vec![vec!["1".to_string()]]
    );
    check_decoding(
        &conn,
        &[
            ("CAST(7 AS SMALLINT)", "7"),
            ("'ok'::su_it.mood", "ok"),
            (
                "INTERVAL '1 year 2 months 3 days 01:02:03'",
                "1 year 2 mons 3 days 01:02:03",
            ),
            ("CAST(9000000000 AS BIGINT)", "9000000000"),
            ("CAST(1.5 AS REAL)", "1.5"),
            ("CAST(2.25 AS DOUBLE PRECISION)", "2.25"),
            ("CAST(12.30 AS NUMERIC(10,2))", "12.30"),
            ("TRUE", "true"),
            ("ARRAY['a', 'b']::text[]", "{a,b}"),
            ("ARRAY[1, 2]::int[]", "{1,2}"),
            (
                "'00000000-0000-0000-0000-000000000001'::uuid",
                "00000000-0000-0000-0000-000000000001",
            ),
            ("'{\"k\": 1}'::jsonb", "{\"k\":1}"),
            ("DATE '2024-01-02'", "2024-01-02"),
            ("TIMESTAMP '2024-01-02 03:04:05'", "2024-01-02 03:04:05"),
            ("'x'::char(1)", "x"),
            ("'1 day'::interval", "1 day"),
            ("NULL::int", NULL_CELL),
            ("'NULL'::text", "NULL"),
        ],
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn integration_mysql() {
    let Some(dsn) = env("SU_IT_MYSQL") else {
        eprintln!("SU_IT_MYSQL not set, skipping");
        return;
    };
    run_mysql_suite(&dsn).await;
}

#[tokio::test]
#[ignore]
async fn integration_mariadb() {
    let Some(dsn) = env("SU_IT_MARIADB") else {
        eprintln!("SU_IT_MARIADB not set, skipping");
        return;
    };
    // Same suite as MySQL: MariaDB speaks its protocol.
    run_mysql_suite(&dsn).await;
}

/// The MySQL scenario, also run against MariaDB.
async fn run_mysql_suite(dsn: &str) {
    let conn = DatabaseConnection::MySQL(sqlx::MySqlPool::connect(dsn).await.unwrap());
    // One connection: a transaction leaked into the pool would be reused.
    let single = DatabaseConnection::MySQL(
        sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .connect(dsn)
            .await
            .unwrap(),
    );
    let schema = scalar(&conn, "SELECT DATABASE()").await;
    let d = Dialect {
        db: DatabaseType::MySQL,
        schema,
        setup: vec![
            "DROP TABLE IF EXISTS child, ccopy, parent, pcopy, dflt".into(),
            "CREATE TABLE dflt (id INT AUTO_INCREMENT PRIMARY KEY, \
             status VARCHAR(10) NOT NULL DEFAULT 'new', n INT DEFAULT 5)"
                .into(),
            "CREATE TABLE parent (id INT AUTO_INCREMENT PRIMARY KEY, \
             a INT NOT NULL, b INT NOT NULL, name VARCHAR(50))"
                .into(),
            "CREATE UNIQUE INDEX parent_ab_uq ON parent (a, b)".into(),
            "CREATE TABLE child (id INT AUTO_INCREMENT PRIMARY KEY, \
             ca INT, cb INT, note VARCHAR(20), \
             CONSTRAINT child_parent_fk FOREIGN KEY (cb, ca) REFERENCES parent (a, b))"
                .into(),
            "CREATE INDEX child_note_ix ON child (note)".into(),
        ],
    };
    exercise(&conn, &d).await;
    cancelled_transaction_leaves_nothing_open(
        &single,
        &conn,
        &d,
        "SELECT SLEEP(5)",
        (
            "SELECT COUNT(*) FROM information_schema.innodb_trx              WHERE trx_mysql_thread_id = CONNECTION_ID()",
            "0",
        ),
    )
    .await;
    // Bare names resolve in the current database.
    let cols = conn.get_table_column_details("parent").await.unwrap();
    assert_eq!(cols.len(), 4);
    assert_eq!(conn.get_primary_keys("parent").await.unwrap(), ["id"]);
    assert_eq!(conn.get_foreign_keys("child").await.unwrap().len(), 1);
    // The text protocol accepts several statements; the first row set wins.
    let r = exec(&conn, "SELECT 1 AS a; SELECT 2 AS b, 3 AS c").await;
    assert_eq!(r.rows, vec![vec!["1".to_string()]]);
    let r = exec(
        &conn,
        "UPDATE parent SET name = name WHERE a = 1; UPDATE parent SET name = name",
    )
    .await;
    assert_eq!(r.rows_affected, 1 + 6);
    check_decoding(
        &conn,
        &[
            ("CAST(7 AS SIGNED)", "7"),
            (
                "CAST(18446744073709551615 AS UNSIGNED)",
                "18446744073709551615",
            ),
            ("CAST(12.30 AS DECIMAL(10,2))", "12.30"),
            ("CAST(1.5 AS DOUBLE)", "1.5"),
            ("CAST(1.5 AS FLOAT)", "1.5"),
            ("DATE '2024-01-02'", "2024-01-02"),
            ("TIMESTAMP '2024-01-02 03:04:05'", "2024-01-02 03:04:05"),
            // A JSON value on MySQL, LONGTEXT on MariaDB (no `CAST(... AS JSON)`).
            ("JSON_OBJECT('k', 1)", "{\"k\": 1}"),
            ("b'1'", "\u{1}"),
            ("NULL", NULL_CELL),
            ("'NULL'", "NULL"),
        ],
    )
    .await;
    // Small integer column types.
    exec(&conn, "DROP TABLE IF EXISTS su_types").await;
    exec(
        &conn,
        "CREATE TABLE su_types (t TINYINT, s SMALLINT, m MEDIUMINT, u INT UNSIGNED, \
         bu BIGINT UNSIGNED, bo BOOLEAN, y YEAR, e ENUM('x','y'))",
    )
    .await;
    exec(
        &conn,
        "INSERT INTO su_types VALUES (-1, -2, -3, 4, 5, TRUE, 2024, 'y')",
    )
    .await;
    let r = exec(&conn, "SELECT * FROM su_types").await;
    assert_eq!(r.rows[0], ["-1", "-2", "-3", "4", "5", "1", "2024", "y"]);
    exec(&conn, "DROP TABLE su_types").await;
    // A password full of URL delimiters reaches the server untouched.
    exec(&conn, "DROP USER IF EXISTS 'su_special'@'%'").await;
    exec(
        &conn,
        &format!("CREATE USER 'su_special'@'%' IDENTIFIED BY '{SPECIAL_PASSWORD}'"),
    )
    .await;
    let special = ConnectionConfig {
        username: Some("su_special".into()),
        password: Some(SPECIAL_PASSWORD.into()),
        database: String::new(),
        ..config_from_url(dsn)
    };
    let c = DatabaseConnection::connect(&special).await.unwrap();
    assert_eq!(scalar(&c, "SELECT 1").await, "1");
}

/// `host=...;port=...;user=...;password=...;database=...`
fn mssql_config(spec: &str) -> ConnectionConfig {
    let mut config = ConnectionConfig {
        name: "it".into(),
        db_type: DatabaseType::SQLServer,
        ..Default::default()
    };
    for part in spec.split(';').filter(|p| !p.is_empty()) {
        let (key, value) = part.split_once('=').expect("key=value");
        match key.trim() {
            "host" => config.host = Some(value.into()),
            "port" => config.port = Some(value.parse().expect("port")),
            "user" => config.username = Some(value.into()),
            "password" => config.password = Some(value.into()),
            "database" => config.database = value.into(),
            other => panic!("unknown SU_IT_MSSQL key {other}"),
        }
    }
    config
}

#[tokio::test]
#[ignore]
async fn integration_mssql() {
    let Some(spec) = env("SU_IT_MSSQL") else {
        eprintln!("SU_IT_MSSQL not set, skipping");
        return;
    };
    let conn = DatabaseConnection::connect(&mssql_config(&spec))
        .await
        .unwrap();
    let d = Dialect {
        db: DatabaseType::SQLServer,
        schema: "su_it".into(),
        setup: vec![
            "DROP TABLE IF EXISTS su_it.child, su_it.ccopy, su_it.parent, su_it.pcopy, \
             su_it.dflt"
                .into(),
            "IF SCHEMA_ID('su_it') IS NULL EXEC('CREATE SCHEMA su_it')".into(),
            "CREATE TABLE su_it.dflt (id INT IDENTITY(1,1) PRIMARY KEY, \
             status VARCHAR(10) NOT NULL DEFAULT 'new', n INT DEFAULT 5)"
                .into(),
            "CREATE TABLE su_it.parent (id INT IDENTITY(1,1) PRIMARY KEY, \
             a INT NOT NULL, b INT NOT NULL, name NVARCHAR(50))"
                .into(),
            "CREATE UNIQUE INDEX parent_ab_uq ON su_it.parent (a, b)".into(),
            "CREATE TABLE su_it.child (id INT IDENTITY(1,1) PRIMARY KEY, \
             ca INT, cb INT, note VARCHAR(20), \
             CONSTRAINT child_parent_fk FOREIGN KEY (cb, ca) REFERENCES su_it.parent (a, b))"
                .into(),
            "CREATE INDEX child_note_ix ON su_it.child (note)".into(),
        ],
    };
    exercise(&conn, &d).await;
    // The single shared client must survive an interrupted batch: reconnected,
    // nothing left open, the abandoned batch not committed.
    let other = DatabaseConnection::connect(&mssql_config(&spec))
        .await
        .unwrap();
    cancelled_transaction_leaves_nothing_open(
        &conn,
        &other,
        &d,
        "WAITFOR DELAY '00:00:05'",
        ("SELECT @@TRANCOUNT", "0"),
    )
    .await;
    // Same for a plain long query interrupted mid-way.
    let slow = conn.execute_query("WAITFOR DELAY '00:00:10'; SELECT 1 AS one");
    let run = tokio::time::timeout(std::time::Duration::from_millis(500), slow).await;
    assert!(run.is_err(), "the query must be interrupted while running");
    let start = std::time::Instant::now();
    assert_eq!(scalar(&conn, "SELECT 42").await, "42");
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(scalar(&conn, "SELECT @@TRANCOUNT").await, "0");
    // A server error is not an interruption: the session (and its temporary
    // tables) is kept.
    exec(&conn, "CREATE TABLE #su_tmp (x INT)").await;
    assert!(conn
        .execute_query("SELECT * FROM su_it_missing")
        .await
        .is_err());
    let block = [
        "BEGIN TRANSACTION".to_string(),
        "INSERT INTO #su_tmp VALUES (1)".to_string(),
        "INSERT INTO #su_tmp VALUES ('x')".to_string(),
        "COMMIT".to_string(),
    ];
    assert!(conn.execute_transaction(&block).await.is_err());
    assert_eq!(scalar(&conn, "SELECT COUNT(*) FROM #su_tmp").await, "0");
    assert_eq!(scalar(&conn, "SELECT @@TRANCOUNT").await, "0");
    let ddl = table_ddl(&conn, &TableCache::default(), &d.db, &d.q("parent"))
        .await
        .unwrap();
    assert!(ddl.contains("nvarchar(50)"), "{ddl}");
    check_decoding(
        &conn,
        &[
            ("CAST(7 AS TINYINT)", "7"),
            ("CAST(-7 AS SMALLINT)", "-7"),
            ("CAST(9000000000 AS BIGINT)", "9000000000"),
            ("CAST(1 AS BIT)", "true"),
            ("CAST(1.5 AS REAL)", "1.5"),
            ("CAST(2.25 AS FLOAT)", "2.25"),
            ("CAST(12.30 AS DECIMAL(10,2))", "12.30"),
            ("CAST(12.5 AS MONEY)", "12.5"),
            (
                "CAST('2024-01-02 03:04:05 +02:00' AS DATETIMEOFFSET)",
                "2024-01-02T03:04:05+02:00",
            ),
            ("CAST('03:04:05' AS TIME)", "03:04:05"),
            ("CAST('<a/>' AS XML)", "<a/>"),
            (
                "CAST('00000000-0000-0000-0000-000000000001' AS UNIQUEIDENTIFIER)",
                "00000000-0000-0000-0000-000000000001",
            ),
            ("CAST('2024-01-02' AS DATE)", "2024-01-02"),
            (
                "CAST('2024-01-02 03:04:05' AS DATETIME2)",
                "2024-01-02 03:04:05",
            ),
            ("N'héllo'", "héllo"),
            ("CAST(NULL AS INT)", NULL_CELL),
            ("'NULL'", "NULL"),
        ],
    )
    .await;
}

fn host_port(spec: &str) -> (String, u16) {
    let (h, p) = spec.rsplit_once(':').expect("host:port");
    (h.to_string(), p.parse().expect("port"))
}

#[tokio::test]
#[ignore]
async fn integration_pg_tls() {
    let (Some(spec), Some(ca)) = (env("SU_IT_PG_TLS"), env("SU_IT_PG_TLS_CA")) else {
        eprintln!("SU_IT_PG_TLS / SU_IT_PG_TLS_CA not set, skipping");
        return;
    };
    let (host, port) = host_port(&spec);
    let config = |mode: SslMode, ca: Option<&str>| ConnectionConfig {
        name: "tls".into(),
        db_type: DatabaseType::Postgres,
        host: Some(host.clone()),
        port: Some(port),
        username: Some("postgres".into()),
        password: Some(SPECIAL_PASSWORD.into()),
        database: "postgres".into(),
        ssl_mode: Some(mode),
        ssl_ca: ca.map(Into::into),
        ..Default::default()
    };
    let refused = |c: ConnectionConfig| async move {
        match DatabaseConnection::connect(&c).await {
            Ok(_) => panic!("{:?} must be refused", c.ssl_mode),
            Err(e) => format!("{e:#}"),
        }
    };
    let in_tls = "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()";

    let e = refused(config(SslMode::Disable, None)).await;
    assert!(e.contains("exige une connexion chiffrée"), "{e}");

    // Encrypted, unverified; the special-character password went through
    // untouched.
    let conn = DatabaseConnection::connect(&config(SslMode::Require, None))
        .await
        .unwrap();
    assert_eq!(scalar(&conn, in_tls).await, "true");
    // The default mode negotiates TLS too.
    let conn = DatabaseConnection::connect(&config(SslMode::Prefer, None))
        .await
        .unwrap();
    assert_eq!(scalar(&conn, in_tls).await, "true");

    let e = refused(config(SslMode::VerifyFull, None)).await;
    assert!(e.contains("Certificat du serveur non reconnu"), "{e}");

    let conn = DatabaseConnection::connect(&config(SslMode::VerifyFull, Some(&ca)))
        .await
        .unwrap();
    assert_eq!(scalar(&conn, in_tls).await, "true");

    // The certificate names `localhost`, not the address.
    if host == "localhost" {
        let by_ip = ConnectionConfig {
            host: Some("127.0.0.1".into()),
            ..config(SslMode::VerifyFull, Some(&ca))
        };
        let e = refused(by_ip).await;
        assert!(e.contains("ne correspond pas à 127.0.0.1"), "{e}");
    }
}

#[tokio::test]
#[ignore]
async fn integration_cockroach() {
    let Some(dsn) = env("SU_IT_COCKROACH") else {
        eprintln!("SU_IT_COCKROACH not set, skipping");
        return;
    };
    // Insecure node: the default `Prefer` falls back to plain text.
    let conn = DatabaseConnection::connect(&config_from_url(&dsn))
        .await
        .unwrap();
    exec(&conn, "DROP TABLE IF EXISTS su_crdb").await;
    exec(
        &conn,
        "CREATE TABLE su_crdb (id INT PRIMARY KEY, name STRING)",
    )
    .await;
    exec(&conn, "INSERT INTO su_crdb VALUES (1, 'a')").await;
    let schemas = conn.get_tables_by_schema().await.unwrap();
    assert!(
        schemas
            .iter()
            .any(|s| s.tables.iter().any(|t| t.contains("su_crdb"))),
        "{schemas:?}"
    );
    let cache = TableCache::default();
    // Details must not fail even where Cockroach lacks catalog pieces.
    let d = table_details(&conn, &cache, "public.su_crdb")
        .await
        .unwrap();
    assert!(
        d.columns.iter().any(|c| c.name == "name"),
        "{:?}",
        d.columns
    );
    table_ddl(&conn, &cache, &DatabaseType::Postgres, "public.su_crdb")
        .await
        .unwrap();
    assert_eq!(
        scalar(&conn, "SELECT name FROM su_crdb WHERE id = 1").await,
        "a"
    );
}

/// Google Cloud IAM renewal, with a fake token provider whose "token" is a
/// password the test changes on the server: once renewed, the pool's new
/// connections sign in with the new one; the renewal stops with the pool.
#[tokio::test]
#[ignore]
async fn integration_gcp_iam_token_renewal() {
    use crate::engine::db::gcp_iam::{connect_renewing, Provider, Timing, Token};
    use sqlx::Connection;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let Some(dsn) = env("SU_IT_PG") else {
        eprintln!("SU_IT_PG not set, skipping");
        return;
    };
    let admin = sqlx::PgPool::connect(&dsn).await.unwrap();
    let set_password = |token: &str| format!("ALTER ROLE su_iam_it PASSWORD '{token}'");
    sqlx::query("DROP ROLE IF EXISTS su_iam_it")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("CREATE ROLE su_iam_it LOGIN PASSWORD 'tok-1'")
        .execute(&admin)
        .await
        .unwrap();

    // `current` is what the provider hands out; `served` what it handed out.
    let current = Arc::new(Mutex::new("tok-1".to_string()));
    let served = Arc::new(Mutex::new(Vec::<String>::new()));
    let provider: Provider = {
        let (current, served) = (current.clone(), served.clone());
        Arc::new(move |account| {
            assert_eq!(account, "jane@example.com");
            let token = current.lock().unwrap().clone();
            served.lock().unwrap().push(token.clone());
            Box::pin(async move {
                Ok(Token::new(
                    token,
                    Instant::now() + Duration::from_millis(300),
                ))
            })
        })
    };
    let timing = Timing {
        renew_before: Duration::ZERO,
        min_delay: Duration::from_millis(200),
        retry: Duration::from_millis(100),
        max_retry: Duration::from_millis(100),
    };
    let base: sqlx::postgres::PgConnectOptions = dsn.parse().unwrap();
    let base = base.username("su_iam_it");
    let options = {
        let base = base.clone();
        move |t: &Token| base.clone().password(t.secret())
    };
    let connect = || {
        connect_renewing(
            sqlx::postgres::PgPoolOptions::new().max_connections(1),
            "jane@example.com".to_string(),
            provider.clone(),
            timing,
            options.clone(),
        )
    };
    let who = "SELECT current_user::text";
    let pool = connect().await.unwrap();
    let user: String = sqlx::query_scalar(who).fetch_one(&pool).await.unwrap();
    assert_eq!(user, "su_iam_it");

    // The token changes on the server; wait for the pool to get it.
    *current.lock().unwrap() = "tok-2".into();
    sqlx::query(&set_password("tok-2"))
        .execute(&admin)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while served.lock().unwrap().last().map(String::as_str) != Some("tok-2") {
        assert!(
            Instant::now() < deadline,
            "no renewal: {:?}",
            served.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The old token no longer signs in…
    let old = sqlx::PgConnection::connect_with(&base.clone().password("tok-1")).await;
    assert!(old.is_err());
    // …but a new connection of the pool does.
    pool.acquire().await.unwrap().close().await.unwrap();
    let user: String = sqlx::query_scalar(who).fetch_one(&pool).await.unwrap();
    assert_eq!(user, "su_iam_it");

    // Closing the pool stops the renewal.
    pool.close().await;
    let count = served.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(served.lock().unwrap().len(), count);

    // So does dropping a connection without closing it.
    let conn = DatabaseConnection::Postgres(connect().await.unwrap());
    assert_eq!(scalar(&conn, who).await, "su_iam_it");
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(conn);
    let count = served.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(served.lock().unwrap().len(), count);

    sqlx::query("DROP ROLE su_iam_it")
        .execute(&admin)
        .await
        .unwrap();
}

/// Incoming foreign keys and the row-matching conditions, on two schemas
/// `s1` / `s2` (MySQL: databases) that `setup` (re)creates with:
/// `s1.single (id)`, `s1.parent (a, b)` keyed on `(a, b)`, `s1.child` with
/// `child_single_fk (pid) → single (id)` and `child_parent_fk (cb, ca) →
/// parent (a, b)`, `s2.other` with `other_single_fk (sid) → s1.single (id)`;
/// one row each: `single.id = 1`, `parent = (1, 2)`, `child = (pid 1, ca 2,
/// cb 1)`, `other.sid = 1`.
async fn check_incoming_fks(conn: &DatabaseConnection, db: DatabaseType, setup: &[&str]) {
    use crate::engine::db::utils::split_qualified;
    use crate::engine::sql::relations::{follow, referencing};

    for sql in setup {
        exec(conn, sql).await;
    }
    let quotes = quote_chars(&db);
    let q = |name: &str| match split_qualified(name) {
        (Some(schema), table) => qualified(&schema, &table, quotes),
        (None, table) => qualified("su_it_fk1", &table, quotes),
    };
    let count = |table: String, condition: String| async move {
        scalar(
            conn,
            &format!("SELECT COUNT(*) FROM {} WHERE {condition}", q(&table)),
        )
        .await
    };

    // Simple key, referenced from its own schema and from another one.
    let single = conn
        .get_incoming_foreign_keys(&q("su_it_fk1.single"))
        .await
        .unwrap();
    let got: Vec<_> = single
        .iter()
        .map(|k| {
            (
                k.name.as_str(),
                k.table.as_str(),
                k.columns.clone(),
                k.ref_columns.clone(),
            )
        })
        .collect();
    let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        got,
        [
            (
                "child_single_fk",
                "su_it_fk1.child",
                v(&["pid"]),
                v(&["id"])
            ),
            (
                "other_single_fk",
                "su_it_fk2.other",
                v(&["sid"]),
                v(&["id"])
            ),
        ]
    );
    for fk in &single {
        let row = |c: &str| (c == "id").then(|| "1".to_string());
        let (table, condition) = referencing(fk, &row, &db).unwrap();
        assert_eq!(count(table, condition).await, "1", "{fk:?}");
    }

    // Composite key, in key order (not column order), through the details.
    let details = table_details(conn, &TableCache::default(), &q("su_it_fk1.parent"))
        .await
        .unwrap();
    assert_eq!(details.incoming_error, None);
    assert_eq!(details.incoming.len(), 1, "{:?}", details.incoming);
    let fk = &details.incoming[0];
    assert_eq!(fk.name, "child_parent_fk");
    assert_eq!(fk.table, "su_it_fk1.child");
    assert_eq!(fk.columns, ["cb", "ca"]);
    assert_eq!(fk.ref_columns, ["a", "b"]);
    let parent_row = |c: &str| match c {
        "a" => Some("1".to_string()),
        "b" => Some("2".to_string()),
        _ => None,
    };
    let (table, condition) = referencing(fk, &parent_row, &db).unwrap();
    assert_eq!(count(table, condition).await, "1");

    // Following the child's outgoing keys finds the referenced rows.
    let child_row = |c: &str| match c {
        "pid" | "cb" => Some("1".to_string()),
        "ca" => Some("2".to_string()),
        _ => None,
    };
    let outgoing = conn.get_foreign_keys(&q("su_it_fk1.child")).await.unwrap();
    let mut targets: Vec<_> = outgoing.iter().map(|k| k.ref_table.as_str()).collect();
    targets.sort();
    assert_eq!(targets, ["su_it_fk1.parent", "su_it_fk1.single"]);
    let other = conn.get_foreign_keys(&q("su_it_fk2.other")).await.unwrap();
    assert_eq!(other[0].ref_table, "su_it_fk1.single");
    for fk in &outgoing {
        let (table, condition) = follow(fk, &child_row, &db).unwrap();
        assert_eq!(count(table, condition).await, "1", "{fk:?}");
    }

    // Nothing points at the leaf tables.
    assert!(conn
        .get_incoming_foreign_keys(&q("su_it_fk2.other"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore]
async fn integration_incoming_fks_postgres() {
    let Some(dsn) = env("SU_IT_PG") else {
        eprintln!("SU_IT_PG not set, skipping");
        return;
    };
    let conn = DatabaseConnection::Postgres(sqlx::PgPool::connect(&dsn).await.unwrap());
    check_incoming_fks(
        &conn,
        DatabaseType::Postgres,
        &[
            "DROP SCHEMA IF EXISTS su_it_fk2 CASCADE",
            "DROP SCHEMA IF EXISTS su_it_fk1 CASCADE",
            "CREATE SCHEMA su_it_fk1",
            "CREATE SCHEMA su_it_fk2",
            "CREATE TABLE su_it_fk1.single (id INT PRIMARY KEY)",
            "CREATE TABLE su_it_fk1.parent (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (a, b))",
            "CREATE TABLE su_it_fk1.child (id INT PRIMARY KEY, ca INT, cb INT, pid INT, \
             CONSTRAINT child_single_fk FOREIGN KEY (pid) REFERENCES su_it_fk1.single (id), \
             CONSTRAINT child_parent_fk FOREIGN KEY (cb, ca) REFERENCES su_it_fk1.parent (a, b))",
            "CREATE TABLE su_it_fk2.other (id INT PRIMARY KEY, sid INT, \
             CONSTRAINT other_single_fk FOREIGN KEY (sid) REFERENCES su_it_fk1.single (id))",
            "INSERT INTO su_it_fk1.single VALUES (1)",
            "INSERT INTO su_it_fk1.parent VALUES (1, 2)",
            "INSERT INTO su_it_fk1.child VALUES (1, 2, 1, 1)",
            "INSERT INTO su_it_fk2.other VALUES (1, 1)",
        ],
    )
    .await;
    // A partitioned referencing table: its partitions hold cloned
    // constraints (`conparentid` set), listed once through the parent.
    for sql in [
        "CREATE TABLE su_it_fk1.part (id INT, sid INT, \
         CONSTRAINT part_single_fk FOREIGN KEY (sid) REFERENCES su_it_fk1.single (id)) \
         PARTITION BY RANGE (id)",
        "CREATE TABLE su_it_fk1.part_1 PARTITION OF su_it_fk1.part FOR VALUES FROM (0) TO (100)",
        "CREATE TABLE su_it_fk1.part_2 PARTITION OF su_it_fk1.part FOR VALUES FROM (100) TO (200)",
    ] {
        exec(&conn, sql).await;
    }
    let tables: Vec<_> = conn
        .get_incoming_foreign_keys("su_it_fk1.single")
        .await
        .unwrap()
        .into_iter()
        .map(|k| k.table)
        .collect();
    assert_eq!(
        tables,
        ["su_it_fk1.child", "su_it_fk1.part", "su_it_fk2.other"]
    );
}

#[tokio::test]
#[ignore]
async fn integration_incoming_fks_mysql() {
    let Some(dsn) = env("SU_IT_MYSQL") else {
        eprintln!("SU_IT_MYSQL not set, skipping");
        return;
    };
    let conn = DatabaseConnection::MySQL(sqlx::MySqlPool::connect(&dsn).await.unwrap());
    check_incoming_fks(
        &conn,
        DatabaseType::MySQL,
        &[
            "DROP DATABASE IF EXISTS su_it_fk2",
            "DROP DATABASE IF EXISTS su_it_fk1",
            "CREATE DATABASE su_it_fk1",
            "CREATE DATABASE su_it_fk2",
            "CREATE TABLE su_it_fk1.single (id INT PRIMARY KEY)",
            "CREATE TABLE su_it_fk1.parent (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (a, b))",
            "CREATE TABLE su_it_fk1.child (id INT PRIMARY KEY, ca INT, cb INT, pid INT, \
             CONSTRAINT child_single_fk FOREIGN KEY (pid) REFERENCES su_it_fk1.single (id), \
             CONSTRAINT child_parent_fk FOREIGN KEY (cb, ca) REFERENCES su_it_fk1.parent (a, b))",
            "CREATE TABLE su_it_fk2.other (id INT PRIMARY KEY, sid INT, \
             CONSTRAINT other_single_fk FOREIGN KEY (sid) REFERENCES su_it_fk1.single (id))",
            "INSERT INTO su_it_fk1.single VALUES (1)",
            "INSERT INTO su_it_fk1.parent VALUES (1, 2)",
            "INSERT INTO su_it_fk1.child VALUES (1, 2, 1, 1)",
            "INSERT INTO su_it_fk2.other VALUES (1, 1)",
        ],
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn integration_incoming_fks_mssql() {
    let Some(spec) = env("SU_IT_MSSQL") else {
        eprintln!("SU_IT_MSSQL not set, skipping");
        return;
    };
    let conn = DatabaseConnection::connect(&mssql_config(&spec))
        .await
        .unwrap();
    check_incoming_fks(
        &conn,
        DatabaseType::SQLServer,
        &[
            "DROP TABLE IF EXISTS su_it_fk2.other, su_it_fk1.child, su_it_fk1.parent, \
             su_it_fk1.single",
            "IF SCHEMA_ID('su_it_fk1') IS NULL EXEC('CREATE SCHEMA su_it_fk1')",
            "IF SCHEMA_ID('su_it_fk2') IS NULL EXEC('CREATE SCHEMA su_it_fk2')",
            "CREATE TABLE su_it_fk1.single (id INT PRIMARY KEY)",
            "CREATE TABLE su_it_fk1.parent (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (a, b))",
            "CREATE TABLE su_it_fk1.child (id INT PRIMARY KEY, ca INT, cb INT, pid INT, \
             CONSTRAINT child_single_fk FOREIGN KEY (pid) REFERENCES su_it_fk1.single (id), \
             CONSTRAINT child_parent_fk FOREIGN KEY (cb, ca) REFERENCES su_it_fk1.parent (a, b))",
            "CREATE TABLE su_it_fk2.other (id INT PRIMARY KEY, sid INT, \
             CONSTRAINT other_single_fk FOREIGN KEY (sid) REFERENCES su_it_fk1.single (id))",
            "INSERT INTO su_it_fk1.single VALUES (1)",
            "INSERT INTO su_it_fk1.parent VALUES (1, 2)",
            "INSERT INTO su_it_fk1.child VALUES (1, 2, 1, 1)",
            "INSERT INTO su_it_fk2.other VALUES (1, 1)",
        ],
    )
    .await;
}

/// Keys and values holding `\`, `'` and non-Latin text round-trip through
/// `submit_changes` (insert / update / delete) and the conditions of
/// `follow` / `referencing`. `setup` (re)creates, in `schema`, `lp (k text
/// PRIMARY KEY, v text)` and `lc (id INT PRIMARY KEY, pk text REFERENCES
/// lp (k))`, empty.
async fn check_tricky_literals(
    conn: &DatabaseConnection,
    db: DatabaseType,
    schema: &str,
    setup: &[&str],
) {
    use crate::engine::db::utils::split_qualified;
    use crate::engine::sql::relations::{follow, referencing};

    for sql in setup {
        exec(conn, sql).await;
    }
    let quotes = quote_chars(&db);
    let q = |name: &str| match split_qualified(name) {
        (Some(s), t) => qualified(&s, &t, quotes),
        (None, t) => qualified(schema, &t, quotes),
    };
    let (lp, lc) = (q("lp"), q("lc"));
    let p_cols = conn.get_table_column_details(&lp).await.unwrap();
    let c_cols = conn.get_table_column_details(&lc).await.unwrap();
    let db = &db;
    let submit = |table: String, columns, changes: RowChanges| async move {
        submit_changes(conn, db, &table, columns, &changes)
            .await
            .unwrap_or_else(|e| panic!("{table}: {e:#}"))
    };
    let keys = ["x\\' OR 1=1 -- ", "ends\\", "C:\\temp", "東京 🦄"];
    let some = |v: &str| Some(v.to_string());

    for key in keys {
        let changes = RowChanges {
            inserts: vec![vec![some(key), some(key)]],
            ..Default::default()
        };
        submit(lp.clone(), &p_cols, changes).await;
    }
    let stored = exec(conn, &format!("SELECT k, v FROM {lp}")).await.rows;
    for key in keys {
        assert!(
            stored.iter().any(|r| r[0] == key && r[1] == key),
            "{key:?} in {stored:?}"
        );
    }

    for (i, key) in keys.iter().enumerate() {
        // Update identified by the tricky key, to another tricky value.
        let new_value = format!("{key}'\\");
        let changes = RowChanges {
            updates: vec![(
                vec![key.to_string(), key.to_string()],
                vec![key.to_string(), new_value.clone()],
            )],
            ..Default::default()
        };
        assert_eq!(submit(lp.clone(), &p_cols, changes).await, 1);
        let id = (i + 1).to_string();
        let changes = RowChanges {
            inserts: vec![vec![some(&id), some(key)]],
            ..Default::default()
        };
        submit(lc.clone(), &c_cols, changes).await;

        // Child → parent, then parent → children.
        let fk = &conn.get_foreign_keys(&lc).await.unwrap()[0];
        let child_row = |c: &str| (c == "pk").then(|| key.to_string());
        let (table, condition) = follow(fk, &child_row, db).unwrap();
        let r = exec(
            conn,
            &format!("SELECT v FROM {} WHERE {condition}", q(&table)),
        )
        .await;
        assert_eq!(r.rows, vec![vec![new_value.clone()]], "{condition}");
        let incoming = &conn.get_incoming_foreign_keys(&lp).await.unwrap()[0];
        let parent_row = |c: &str| (c == "k").then(|| key.to_string());
        let (table, condition) = referencing(incoming, &parent_row, db).unwrap();
        let r = exec(
            conn,
            &format!("SELECT id FROM {} WHERE {condition}", q(&table)),
        )
        .await;
        assert_eq!(r.rows, vec![vec![id.clone()]], "{condition}");

        // Delete both through their keys.
        let changes = RowChanges {
            deletes: vec![vec![id.clone(), key.to_string()]],
            ..Default::default()
        };
        assert_eq!(submit(lc.clone(), &c_cols, changes).await, 1);
        let changes = RowChanges {
            deletes: vec![vec![key.to_string(), new_value]],
            ..Default::default()
        };
        assert_eq!(submit(lp.clone(), &p_cols, changes).await, 1);
    }
    assert_eq!(
        scalar(conn, &format!("SELECT COUNT(*) FROM {lp}")).await,
        "0"
    );
}

#[tokio::test]
#[ignore]
async fn integration_tricky_literals_mysql() {
    let Some(dsn) = env("SU_IT_MYSQL") else {
        eprintln!("SU_IT_MYSQL not set, skipping");
        return;
    };
    let conn = DatabaseConnection::MySQL(sqlx::MySqlPool::connect(&dsn).await.unwrap());
    check_tricky_literals(
        &conn,
        DatabaseType::MySQL,
        "su_it_lit",
        &[
            "DROP DATABASE IF EXISTS su_it_lit",
            "CREATE DATABASE su_it_lit CHARACTER SET utf8mb4",
            "CREATE TABLE su_it_lit.lp (k VARCHAR(50) PRIMARY KEY, v VARCHAR(60))",
            "CREATE TABLE su_it_lit.lc (id INT PRIMARY KEY, pk VARCHAR(50), \
             FOREIGN KEY (pk) REFERENCES su_it_lit.lp (k))",
        ],
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn integration_tricky_literals_postgres() {
    let Some(dsn) = env("SU_IT_PG") else {
        eprintln!("SU_IT_PG not set, skipping");
        return;
    };
    let conn = DatabaseConnection::Postgres(sqlx::PgPool::connect(&dsn).await.unwrap());
    check_tricky_literals(
        &conn,
        DatabaseType::Postgres,
        "su_it_lit",
        &[
            "DROP SCHEMA IF EXISTS su_it_lit CASCADE",
            "CREATE SCHEMA su_it_lit",
            "CREATE TABLE su_it_lit.lp (k VARCHAR(50) PRIMARY KEY, v VARCHAR(60))",
            "CREATE TABLE su_it_lit.lc (id INT PRIMARY KEY, pk VARCHAR(50) \
             REFERENCES su_it_lit.lp (k))",
        ],
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn integration_tricky_literals_mssql() {
    let Some(spec) = env("SU_IT_MSSQL") else {
        eprintln!("SU_IT_MSSQL not set, skipping");
        return;
    };
    let conn = DatabaseConnection::connect(&mssql_config(&spec))
        .await
        .unwrap();
    check_tricky_literals(
        &conn,
        DatabaseType::SQLServer,
        "su_it_lit",
        &[
            "DROP TABLE IF EXISTS su_it_lit.lc, su_it_lit.lp",
            "IF SCHEMA_ID('su_it_lit') IS NULL EXEC('CREATE SCHEMA su_it_lit')",
            "CREATE TABLE su_it_lit.lp (k NVARCHAR(50) PRIMARY KEY, v NVARCHAR(60))",
            "CREATE TABLE su_it_lit.lc (id INT PRIMARY KEY, pk NVARCHAR(50) \
             REFERENCES su_it_lit.lp (k))",
        ],
    )
    .await;
}

/// Scripts on SQL Server run as T-SQL batches split on `GO`, as in SSMS:
/// the user's script shape (variables declared before a transaction, used
/// by a SELECT after its COMMIT), one outcome per result set, script line
/// numbers in errors, THROW rolling back and leaving the client usable.
#[tokio::test]
#[ignore]
async fn integration_mssql_script_batches() {
    use crate::engine::ops::query::{run_all, StatementOutcome, TSQL_UNTERMINATED};
    let Some(spec) = env("SU_IT_MSSQL") else {
        eprintln!("SU_IT_MSSQL not set, skipping");
        return;
    };
    let conn = DatabaseConnection::connect(&mssql_config(&spec))
        .await
        .unwrap();
    for sql in [
        "DROP TABLE IF EXISTS su_it_b.link, su_it_b.acte",
        "IF SCHEMA_ID('su_it_b') IS NULL EXEC('CREATE SCHEMA su_it_b')",
        "CREATE TABLE su_it_b.acte (Id INT IDENTITY(1,1) PRIMARY KEY, \
         CodeActe NVARCHAR(10) NOT NULL, CreatedBy NVARCHAR(36))",
        "CREATE TABLE su_it_b.link (Id INT IDENTITY(1,1) PRIMARY KEY, \
         NgapActeId INT NOT NULL, Spec NVARCHAR(10))",
    ] {
        exec(&conn, sql).await;
    }
    let errors = |outcomes: &[StatementOutcome]| {
        outcomes
            .iter()
            .filter_map(|o| o.result.as_ref().err().cloned())
            .collect::<Vec<_>>()
    };
    let count = |filter: &str| format!("SELECT COUNT(*) FROM su_it_b.link WHERE {filter}");

    // The user's script: a variable declared before the transaction is
    // used by the SELECT after its COMMIT.
    let script = "-- reprise CEP\n\
        SET XACT_ABORT ON;\n\
        SET NOCOUNT ON;\n\
        BEGIN TRANSACTION;\n\
        DECLARE @UserId NVARCHAR(36) = 'u-1';\n\
        IF NOT EXISTS (SELECT 1 FROM su_it_b.acte WHERE CodeActe = N'CEP')\n\
            INSERT INTO su_it_b.acte (CodeActe, CreatedBy) VALUES (N'CEP', @UserId);\n\
        DECLARE @ActeCepId INT = (SELECT [Id] FROM su_it_b.acte WHERE [CodeActe] = N'CEP');\n\
        DECLARE @Specialites TABLE (Code NVARCHAR(10));\n\
        INSERT INTO @Specialites VALUES (N'S1'), (N'S2');\n\
        IF EXISTS (SELECT 1 FROM su_it_b.link WHERE NgapActeId = -1) THROW 50001, N'impossible', 1;\n\
        INSERT INTO su_it_b.link (NgapActeId, Spec) SELECT @ActeCepId, Code FROM @Specialites;\n\
        COMMIT;\n\
        SELECT (SELECT COUNT(*) FROM su_it_b.link WHERE [NgapActeId] = @ActeCepId) AS Liens;\n";
    let out = run_script(&conn, script, Some(1000)).await;
    assert!(errors(&out).is_empty(), "{:?}", errors(&out));
    assert_eq!(out.len(), 1, "one result set");
    let r = out[0].result.as_ref().unwrap();
    assert_eq!(r.rows, [["2"]]);
    assert!(
        out[0].sql.starts_with("SELECT (SELECT COUNT(*)"),
        "{}",
        out[0].sql
    );
    assert_eq!(scalar(&conn, "SELECT @@TRANCOUNT").await, "0");

    // Several SELECTs: one outcome each, titled and enriched by its SELECT;
    // an empty result set keeps its columns.
    let out = run_script(
        &conn,
        "DECLARE @n INT = 1;\nSELECT * FROM su_it_b.acte;\nSELECT @n + 1 AS deux;\n\
         SELECT Id FROM su_it_b.link WHERE 1 = 0;",
        None,
    )
    .await;
    assert!(errors(&out).is_empty(), "{:?}", errors(&out));
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].sql, "SELECT * FROM su_it_b.acte");
    let acte = out[0].result.as_ref().unwrap();
    assert_eq!(acte.primary_key, ["Id"]);
    let code = acte.columns.iter().find(|c| c.name == "CodeActe").unwrap();
    assert!(!code.nullable);
    assert_eq!(out[1].result.as_ref().unwrap().rows, [["2"]]);
    let empty = out[2].result.as_ref().unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(empty.columns.len(), 1);

    // A batch without result sets: the count when the server reports it,
    // « executed » under NOCOUNT. Session settings stay, as in SSMS: the
    // script above left NOCOUNT on.
    let out = run_script(&conn, "UPDATE su_it_b.link SET Spec = Spec", None).await;
    assert!(out[0].result.as_ref().unwrap().count_unknown, "{out:?}");
    exec(&conn, "SET NOCOUNT OFF").await;
    let out = run_script(&conn, "UPDATE su_it_b.link SET Spec = Spec", None).await;
    let r = out[0].result.as_ref().unwrap();
    assert_eq!((r.rows_affected, r.count_unknown), (2, false));
    let out = run_script(
        &conn,
        "SET NOCOUNT ON;\nUPDATE su_it_b.link SET Spec = Spec;\nSET NOCOUNT OFF;",
        None,
    )
    .await;
    assert!(out[0].result.as_ref().unwrap().count_unknown, "{out:?}");
    let out = run_script(
        &conn,
        "SET NOCOUNT ON\nGO\nUPDATE su_it_b.link SET Spec = Spec\nGO\nSET NOCOUNT OFF",
        None,
    )
    .await;
    assert_eq!(out.len(), 3);
    assert!(out[1].result.as_ref().unwrap().count_unknown, "{out:?}");

    // GO separates batches: the second one does not see the first one's
    // variables, and its error names the script line.
    let out = run_script(
        &conn,
        "DECLARE @x INT = 1;\nSELECT @x AS x;\nGO\n\n-- plus loin\nSELECT @x AS x;",
        None,
    )
    .await;
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].result.as_ref().unwrap().rows, [["1"]]);
    let e = out[1].result.as_ref().unwrap_err();
    assert!(
        e.contains("@x") && e.ends_with("(ligne 6 du script)"),
        "{e}"
    );
    // `GO 3` repeats its batch.
    let out = run_script(
        &conn,
        "INSERT INTO su_it_b.link VALUES (0, 'r')\nGO 3",
        None,
    )
    .await;
    assert_eq!(out.len(), 3);
    assert_eq!(scalar(&conn, &count("NgapActeId = 0")).await, "3");

    // THROW inside a transaction: error with its line, all rolled back,
    // nothing left open, the client still usable (its temp tables kept).
    exec(&conn, "CREATE TABLE #su_keep (x INT)").await;
    let out = run_script(
        &conn,
        "SELECT 1 AS avant;\nGO\nBEGIN TRAN;\nINSERT INTO su_it_b.link VALUES (9, 'x');\n\
         THROW 50001, N'refusé', 1;\nCOMMIT;\nGO\nSELECT 2 AS apres;",
        None,
    )
    .await;
    assert_eq!(out.len(), 2, "stops at the failing batch: {out:?}");
    let e = out[1].result.as_ref().unwrap_err();
    assert_eq!(e, "refusé (ligne 5 du script)");
    assert_eq!(scalar(&conn, "SELECT @@TRANCOUNT").await, "0");
    assert_eq!(scalar(&conn, &count("NgapActeId = 9")).await, "0");
    assert_eq!(scalar(&conn, "SELECT COUNT(*) FROM #su_keep").await, "0");
    // A constraint error in a transaction is not committed by its COMMIT.
    let out = run_script(
        &conn,
        "BEGIN TRAN;\nINSERT INTO su_it_b.link VALUES (8, 'x');\n\
         INSERT INTO su_it_b.link VALUES (NULL, 'x');\nCOMMIT;",
        None,
    )
    .await;
    let e = out[0].result.as_ref().unwrap_err();
    assert!(e.ends_with("(ligne 3 du script)"), "{e}");
    assert_eq!(scalar(&conn, &count("NgapActeId = 8")).await, "0");
    // A transaction left open at the end of the script is rolled back.
    let out = run_script(
        &conn,
        "BEGIN TRAN;\nINSERT INTO su_it_b.link VALUES (7, 'x');",
        None,
    )
    .await;
    let e = out.last().unwrap().result.as_ref().unwrap_err();
    assert_eq!(e, TSQL_UNTERMINATED);
    assert_eq!(scalar(&conn, "SELECT @@TRANCOUNT").await, "0");
    assert_eq!(scalar(&conn, &count("NgapActeId = 7")).await, "0");
    // A transaction spanning batches is committed by its last one.
    let out = run_script(
        &conn,
        "BEGIN TRAN
GO
INSERT INTO su_it_b.link VALUES (6, 'x')
GO
COMMIT",
        None,
    )
    .await;
    assert_eq!(errors(&out), Vec::<String>::new());
    assert_eq!(out[1].result.as_ref().unwrap().rows_affected, 1);
    assert_eq!(scalar(&conn, &count("NgapActeId = 6")).await, "1");

    // The TUI's F5 (run_all) runs batches too: the last result set.
    let r = run_all(&conn, "DECLARE @v INT = 4\nGO\nSELECT 5 AS v")
        .await
        .unwrap();
    assert_eq!(r.rows, [["5"]]);
}

/// Manual transaction mode on SQL Server: scripts run as batches on the
/// dedicated session, inside the transaction left open for the user.
#[tokio::test]
#[ignore]
async fn integration_mssql_manual_tx_batches() {
    use crate::engine::ops::tx::ManualTx;
    use std::sync::Arc;
    let Some(spec) = env("SU_IT_MSSQL") else {
        eprintln!("SU_IT_MSSQL not set, skipping");
        return;
    };
    let config = mssql_config(&spec);
    let shared = Arc::new(DatabaseConnection::connect(&config).await.unwrap());
    for sql in [
        "DROP TABLE IF EXISTS su_it_mb.t",
        "IF SCHEMA_ID('su_it_mb') IS NULL EXEC('CREATE SCHEMA su_it_mb')",
        "CREATE TABLE su_it_mb.t (id INT PRIMARY KEY)",
    ] {
        exec(&shared, sql).await;
    }
    let tx = ManualTx::open(&config, shared.clone()).await.unwrap();
    let out = tx
        .run_script(
            "DECLARE @n INT = 1;\nINSERT INTO su_it_mb.t VALUES (@n);\n\
             SELECT id FROM su_it_mb.t WHERE id = @n;",
            None,
        )
        .await;
    assert!(out.iter().all(|o| o.result.is_ok()), "{out:?}");
    assert_eq!(out.last().unwrap().result.as_ref().unwrap().rows, [["1"]]);
    let status = tx.status();
    assert!(status.open);
    assert_eq!(status.statements, 3);
    // Not visible from the shared session before the commit.
    let visible = "SELECT COUNT(*) FROM su_it_mb.t WITH (READPAST)";
    assert_eq!(scalar(&shared, visible).await, "0");
    // GO: the second batch fails with its script line, the transaction is
    // kept (XACT_ABORT off) and can still be committed.
    let out = tx.run_script("SELECT 1 AS un\nGO\nSELECT @n", None).await;
    assert_eq!(out.len(), 2);
    let e = out[1].result.as_ref().unwrap_err();
    assert!(e.ends_with("(ligne 3 du script)"), "{e}");
    assert!(tx.status().open);
    assert_eq!(tx.commit().await.unwrap(), 4);
    assert_eq!(
        scalar(&shared, "SELECT COUNT(*) FROM su_it_mb.t").await,
        "1"
    );
    // A script with its own BEGIN … COMMIT, run while none is open,
    // commits by itself.
    let out = tx
        .run_script(
            "DECLARE @n INT = 2;\nBEGIN TRAN;\nINSERT INTO su_it_mb.t VALUES (@n);\n\
             COMMIT;\nSELECT @n AS n;",
            None,
        )
        .await;
    assert!(out.iter().all(|o| o.result.is_ok()), "{out:?}");
    assert!(!tx.status().open);
    assert_eq!(
        scalar(&shared, "SELECT COUNT(*) FROM su_it_mb.t").await,
        "2"
    );
}
