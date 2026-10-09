//! Integration tests against real Postgres / MySQL / SQL Server servers.
//!
//! Every test is `#[ignore]` and returns early when its environment variable
//! is missing, so a plain `cargo test` stays offline. Run them with
//! `cargo test -- --ignored integration` and:
//!
//! - `SU_IT_PG=postgres://user:pw@host:port/db`
//! - `SU_IT_MYSQL=mysql://user:pw@host:port/db`
//! - `SU_IT_MSSQL=host=127.0.0.1;port=1433;user=sa;password=...;database=db`
//!   (the database must exist)
//!
//! The tests (re)create their own tables in schema `su_it` (MySQL: in the
//! connection's database), so they can be run repeatedly.

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{ConnectionConfig, DatabaseType, NULL_CELL};
use crate::engine::ops::query::run_script;
use crate::engine::ops::rows::{
    detect_system_columns, submit_changes, transaction_bounds, RowChanges,
};
use crate::engine::ops::schema::{table_ddl, table_details};
use crate::engine::ops::transfer::{import_csv, qualified};
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
    assert_eq!(out.len(), 3);
    for o in &out {
        assert!(o.result.is_ok(), "{}: {:?}", o.sql, o.result);
    }
    assert_eq!(out[0].result.as_ref().unwrap().rows_affected, 1);
    assert_eq!(out[1].result.as_ref().unwrap().rows_affected, 1);
    assert_eq!(
        out[2].result.as_ref().unwrap().rows,
        vec![vec!["y".to_string()]]
    );
    let out = run_script(
        conn,
        "SELECT 1; SELECT * FROM su_it_missing_table; SELECT 2",
        None,
    )
    .await;
    assert_eq!(out.len(), 2, "run_script stops after the first error");
    assert!(out[1].result.is_err());

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
    assert_eq!(fks[0].ref_table, "parent");

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
    let conn = DatabaseConnection::MySQL(sqlx::MySqlPool::connect(&dsn).await.unwrap());
    // One connection: a transaction leaked into the pool would be reused.
    let single = DatabaseConnection::MySQL(
        sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&dsn)
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
            ("CAST('{\"k\": 1}' AS JSON)", "{\"k\": 1}"),
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
