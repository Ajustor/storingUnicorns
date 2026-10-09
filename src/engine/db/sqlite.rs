use anyhow::Result;
use sqlx::{sqlite::SqliteRow, Column as SqlxColumn, Row, SqlitePool, TypeInfo, ValueRef};

use crate::engine::models::{
    Column, ForeignKeyInfo, IndexInfo, QueryResult, SchemaInfo, NULL_CELL,
};

use super::utils::{fetch_rows_and_result, is_dml, split_qualified, TxConnection};

/// Connect to SQLite
pub async fn connect(conn_str: &str) -> Result<SqlitePool> {
    let pool = SqlitePool::connect(conn_str).await?;
    Ok(pool)
}

/// Convert fetched rows into a `QueryResult`.
///
/// When the statement returned rows (SELECT, `... RETURNING`), `rows_affected`
/// is the number of rows returned; otherwise it is `affected`, the count
/// reported by the database (UPDATE/DELETE/INSERT).
fn rows_to_result(rows: &[SqliteRow], affected: u64) -> QueryResult {
    if rows.is_empty() {
        return QueryResult {
            rows_affected: affected,
            ..QueryResult::default()
        };
    }

    let columns: Vec<Column> = rows[0]
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_info().name().to_string(),
            nullable: true,
            is_primary_key: false,
        })
        .collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| (0..columns.len()).map(|i| get_value(row, i)).collect())
        .collect();

    QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        ..QueryResult::default()
    }
}

/// Execute one statement exactly once, collecting its rows and affected-row
/// count from the same stream.
async fn run_statement<'c, E>(
    executor: E,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let (rows, done, truncated) = fetch_rows_and_result(executor, query, max_rows, true).await?;
    // SQLite reports `sqlite3_changes()`, which is NOT reset by statements
    // that modify nothing (SELECT, DDL, COMMIT...): it keeps the count of the
    // last INSERT/UPDATE/DELETE on the connection. Only trust it for DML.
    let affected = if is_dml(query) {
        done.rows_affected()
    } else {
        0
    };
    Ok(QueryResult {
        truncated,
        ..rows_to_result(&rows, affected)
    })
}

/// Execute a query on SQLite, keeping at most `max_rows` rows (all when
/// `None`); `truncated` is set when more rows were available.
pub async fn execute_query_limited(
    pool: &SqlitePool,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    let mut conn = pool.acquire().await?;
    // A connection only notices a schema change made by another one when it
    // next runs a statement; until then it prepares against its old schema
    // and reports stale column names (e.g. after a RENAME COLUMN). Touching
    // the schema first makes it reload.
    sqlx::query("SELECT 1 FROM sqlite_master LIMIT 1")
        .persistent(false)
        .fetch_optional(&mut *conn)
        .await?;
    run_statement(&mut *conn, query, max_rows).await
}

/// Execute a sequence of statements as one transaction on a single dedicated
/// connection. The statements include the user's `BEGIN`/`COMMIT`/`ROLLBACK`.
/// On any error the transaction is rolled back and the error is returned.
/// Returns the last statement that returned rows; if none did, the last one
/// that changed rows (or an empty result).
pub async fn execute_transaction(pool: &SqlitePool, statements: &[String]) -> Result<QueryResult> {
    // Closed instead of pooled if the block does not run to completion.
    let mut conn = TxConnection::new(pool.acquire().await?);
    // Prefer the last statement that returned rows; if none did, fall back to
    // the last one that changed rows, so trailing `COMMIT`/`ROLLBACK` don't
    // hide the DML count.
    let mut last_rows: Option<QueryResult> = None;
    let mut last_changed: Option<QueryResult> = None;

    for stmt in statements {
        match run_statement(&mut *conn, stmt, None).await {
            Ok(result) => {
                if !result.rows.is_empty() {
                    last_rows = Some(result);
                } else if result.rows_affected > 0 {
                    last_changed = Some(result);
                }
            }
            Err(e) => {
                // Roll back on the same connection before bubbling up. If
                // that fails the transaction may still be open: the guard
                // closes the connection instead of pooling it.
                if sqlx::query("ROLLBACK").execute(&mut *conn).await.is_ok() {
                    conn.release();
                }
                return Err(e);
            }
        }
    }

    conn.release();
    Ok(last_rows.or(last_changed).unwrap_or_default())
}

/// Get tables grouped by schema (SQLite uses "main" as default schema)
pub async fn get_tables_by_schema(pool: &SqlitePool) -> Result<Vec<SchemaInfo>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master 
         WHERE type='table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    Ok(vec![SchemaInfo {
        name: "main".to_string(),
        tables: rows.into_iter().map(|r| r.0).collect(),
        expanded: false,
    }])
}

/// Insert a new row into a SQLite table
pub async fn insert_row(
    pool: &SqlitePool,
    table_name: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
) -> Result<u64> {
    let (columns_part, values_part) =
        super::utils::build_insert_parts(columns, values, system_columns, '"', '"');

    if columns_part.is_empty() {
        return Err(anyhow::anyhow!("No columns to insert"));
    }

    let query = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name, columns_part, values_part
    );

    tracing::debug!("SQLite INSERT query: {}", query);
    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// `PRAGMA [schema.]pragma('arg')`, the schema (`main`, `temp` or an
/// attached database) quoted, the argument escaped.
fn pragma_on(schema: Option<&str>, pragma: &str, arg: &str) -> String {
    let arg = arg.replace('\'', "''");
    match schema {
        Some(schema) => format!(
            "PRAGMA \"{}\".{pragma}('{arg}')",
            schema.replace('"', "\"\"")
        ),
        None => format!("PRAGMA {pragma}('{arg}')"),
    }
}

/// `PRAGMA table_info` of a bare, qualified or quoted table name (`t`,
/// `main.t`, `"main"."t"`), as `(name, type, nullable, pk position)` where
/// the pk position is 0 outside the primary key.
async fn table_info(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<Vec<(String, String, bool, i64)>> {
    let (schema, table) = split_qualified(table_name);
    let rows: Vec<SqliteRow> = sqlx::query(&pragma_on(schema.as_deref(), "table_info", &table))
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|row| {
            let notnull: i64 = row.try_get("notnull")?;
            Ok((
                row.try_get("name")?,
                row.try_get("type")?,
                notnull == 0,
                row.try_get("pk")?,
            ))
        })
        .collect()
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    Ok(table_info(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, _, nullable, _)| (name, nullable))
        .collect())
}

/// Get primary key columns for a table, in key order
pub async fn get_primary_keys(pool: &SqlitePool, table_name: &str) -> Result<Vec<String>> {
    let mut keys: Vec<(i64, String)> = table_info(pool, table_name)
        .await?
        .into_iter()
        .filter(|(.., pk)| *pk > 0)
        .map(|(name, _, _, pk)| (pk, name))
        .collect();
    keys.sort();
    Ok(keys.into_iter().map(|(_, name)| name).collect())
}

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(pool: &SqlitePool, table_name: &str) -> Result<Vec<String>> {
    Ok(table_info(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, ..)| name)
        .collect())
}

/// Get full column details for a table (for schema modification)
pub async fn get_table_column_details(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<Vec<crate::engine::models::Column>> {
    Ok(table_info(pool, table_name)
        .await?
        .into_iter()
        .map(
            |(name, type_name, nullable, pk)| crate::engine::models::Column {
                name,
                type_name,
                nullable,
                is_primary_key: pk > 0,
            },
        )
        .collect())
}

/// Get the indexes of a table (`PRAGMA index_list` + `PRAGMA index_info`).
/// An `INTEGER PRIMARY KEY` is the rowid and has no index, so it is not listed.
pub async fn get_indexes(pool: &SqlitePool, table_name: &str) -> Result<Vec<IndexInfo>> {
    let (schema, table) = split_qualified(table_name);
    let schema = schema.as_deref();
    let rows: Vec<SqliteRow> = sqlx::query(&pragma_on(schema, "index_list", &table))
        .fetch_all(pool)
        .await?;

    let mut indexes = Vec::new();
    for row in rows {
        let name: String = row.try_get("name")?;
        let unique: i64 = row.try_get("unique")?;
        let origin: String = row.try_get("origin")?;
        let info: Vec<SqliteRow> = sqlx::query(&pragma_on(schema, "index_info", &name))
            .fetch_all(pool)
            .await?;
        let columns = info
            .iter()
            .map(|r| {
                // NULL name: an expression column.
                let col: Option<String> = r.try_get("name")?;
                Ok(col.unwrap_or_else(|| "<expression>".to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        indexes.push(IndexInfo {
            name,
            columns,
            unique: unique != 0,
            primary: origin == "pk",
        });
    }

    Ok(indexes)
}

/// Get the foreign keys of a table (`PRAGMA foreign_key_list`, one row per
/// column, grouped by `id`). SQLite does not expose constraint names, so
/// `name` is empty.
pub async fn get_foreign_keys(pool: &SqlitePool, table_name: &str) -> Result<Vec<ForeignKeyInfo>> {
    let (schema, table) = split_qualified(table_name);
    let rows: Vec<SqliteRow> =
        sqlx::query(&pragma_on(schema.as_deref(), "foreign_key_list", &table))
            .fetch_all(pool)
            .await?;

    let mut keys: Vec<(i64, ForeignKeyInfo)> = Vec::new();
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let ref_table: String = row.try_get("table")?;
        let from: String = row.try_get("from")?;
        // NULL `to`: the referenced table's primary key, left implicit.
        let to: Option<String> = row.try_get("to")?;
        let pos = match keys.iter().position(|(k, _)| *k == id) {
            Some(pos) => pos,
            None => {
                keys.push((
                    id,
                    ForeignKeyInfo {
                        name: String::new(),
                        columns: Vec::new(),
                        ref_table,
                        ref_columns: Vec::new(),
                    },
                ));
                keys.len() - 1
            }
        };
        let fk = &mut keys[pos].1;
        fk.columns.push(from);
        fk.ref_columns.push(to.unwrap_or_default());
    }

    Ok(keys.into_iter().map(|(_, fk)| fk).collect())
}

/// Test the connection
pub async fn test(pool: &SqlitePool) -> Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

/// Close the connection
pub async fn close(pool: SqlitePool) {
    pool.close().await;
}

/// Text of a cell; NULL of any type (an untyped NULL decodes as "" through
/// `String`) is checked first and returned as `NULL_CELL`.
fn get_value(row: &SqliteRow, index: usize) -> String {
    match row.try_get_raw(index) {
        Ok(raw) if !raw.is_null() => {}
        _ => return NULL_CELL.to_string(),
    }
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .or_else(|_| {
            row.try_get::<chrono::DateTime<chrono::Utc>, _>(index)
                .map(|v| v.to_rfc3339())
        })
        .or_else(|_| {
            row.try_get::<chrono::NaiveDateTime, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<chrono::NaiveDate, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<chrono::NaiveTime, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<Vec<u8>, _>(index)
                .map(|v| String::from_utf8_lossy(&v).into_owned())
        })
        .unwrap_or_else(|_| NULL_CELL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn execute_query(pool: &SqlitePool, query: &str) -> Result<QueryResult> {
        execute_query_limited(pool, query, None).await
    }

    /// A shared in-memory pool: max_connections(1) ensures every acquisition
    /// reuses the same connection (and therefore the same in-memory database).
    async fn mem_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (a INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn count(pool: &SqlitePool) -> i64 {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM t")
            .fetch_all(pool)
            .await
            .unwrap();
        rows[0].0
    }

    #[tokio::test]
    async fn null_of_any_type_is_shown_as_null() {
        let pool = mem_pool().await;
        let r = execute_query(
            &pool,
            "SELECT NULL AS a, CAST(NULL AS TEXT) AS b, '' AS c, 'NULL' AS d",
        )
        .await
        .unwrap();
        assert_eq!(r.rows, vec![vec![NULL_CELL, NULL_CELL, "", "NULL"]]);
    }

    #[tokio::test]
    async fn transaction_commit_persists_rows() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (1)".to_string(),
            "INSERT INTO t VALUES (2)".to_string(),
            "COMMIT".to_string(),
        ];
        execute_transaction(&pool, &stmts).await.unwrap();
        assert_eq!(count(&pool).await, 2);
    }

    #[tokio::test]
    async fn transaction_rolls_back_on_error() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (1)".to_string(),
            "INSERT INTO nonexistent_table VALUES (2)".to_string(), // fails
            "COMMIT".to_string(),
        ];
        let result = execute_transaction(&pool, &stmts).await;
        assert!(result.is_err(), "expected the transaction to fail");
        assert_eq!(count(&pool).await, 0, "failed transaction must roll back");
    }

    #[tokio::test]
    async fn transaction_returns_last_select_result() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (42)".to_string(),
            "SELECT a FROM t".to_string(),
            "COMMIT".to_string(),
        ];
        let result = execute_transaction(&pool, &stmts).await.unwrap();
        assert_eq!(result.rows, vec![vec!["42".to_string()]]);
    }

    async fn seed(pool: &SqlitePool) {
        sqlx::query("INSERT INTO t VALUES (1), (2), (3)")
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_reports_rows_affected() {
        let pool = mem_pool().await;
        seed(&pool).await;
        let result = execute_query(&pool, "UPDATE t SET a = a + 10 WHERE a >= 2")
            .await
            .unwrap();
        assert_eq!(result.rows_affected, 2);
        assert!(result.rows.is_empty());
    }

    #[tokio::test]
    async fn delete_reports_rows_affected() {
        let pool = mem_pool().await;
        seed(&pool).await;
        let result = execute_query(&pool, "DELETE FROM t WHERE a = 1")
            .await
            .unwrap();
        assert_eq!(result.rows_affected, 1);
        assert_eq!(count(&pool).await, 2);
    }

    #[tokio::test]
    async fn select_rows_affected_equals_row_count() {
        let pool = mem_pool().await;
        seed(&pool).await;
        let result = execute_query(&pool, "SELECT a FROM t ORDER BY a")
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 3);
        assert_eq!(result.rows_affected, 3);
    }

    #[tokio::test]
    async fn transaction_reports_last_statement_rows_affected() {
        let pool = mem_pool().await;
        seed(&pool).await;
        let stmts = [
            "BEGIN".to_string(),
            "DELETE FROM t WHERE a <= 2".to_string(),
            "COMMIT".to_string(),
        ];
        let result = execute_transaction(&pool, &stmts).await.unwrap();
        assert_eq!(result.rows_affected, 2);
    }

    #[tokio::test]
    async fn empty_select_after_dml_reports_zero() {
        let pool = mem_pool().await;
        seed(&pool).await;
        execute_query(&pool, "DELETE FROM t WHERE a = 1")
            .await
            .unwrap();
        let result = execute_query(&pool, "SELECT a FROM t WHERE a > 100")
            .await
            .unwrap();
        assert_eq!(result.rows_affected, 0);
    }

    #[tokio::test]
    async fn transaction_prefers_last_row_set_over_later_dml() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (7)".to_string(),
            "SELECT a FROM t".to_string(),
            "INSERT INTO t VALUES (8)".to_string(),
            "COMMIT".to_string(),
        ];
        let result = execute_transaction(&pool, &stmts).await.unwrap();
        assert_eq!(result.rows, vec![vec!["7".to_string()]]);
        assert_eq!(result.rows_affected, 1);
    }

    /// Every metadata function accepts bare, qualified and quoted names,
    /// for `main` and for an attached schema.
    #[tokio::test]
    async fn metadata_accepts_qualified_quoted_names() {
        let pool = mem_pool().await;
        for sql in [
            "CREATE TABLE p (a INTEGER NOT NULL, b INTEGER NOT NULL, x TEXT, PRIMARY KEY (b, a))",
            "CREATE TABLE \"we'ird\" (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, \
             FOREIGN KEY (b, a) REFERENCES p (b, a))",
            "CREATE INDEX \"we'ird_ix\" ON \"we'ird\" (a)",
            "ATTACH DATABASE ':memory:' AS aux",
            "CREATE TABLE aux.p (z TEXT PRIMARY KEY, y INT NOT NULL)",
            "CREATE INDEX aux.p_y ON p (y)",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        for name in ["p", "main.p", "\"main\".\"p\"", "\"p\""] {
            let cols = get_table_column_details(&pool, name).await.unwrap();
            assert_eq!(cols.len(), 3, "{name}");
            assert_eq!(
                get_primary_keys(&pool, name).await.unwrap(),
                ["b", "a"],
                "{name}"
            );
            let nullability = get_column_nullability(&pool, name).await.unwrap();
            assert_eq!(nullability.get("x"), Some(&true), "{name}");
            assert_eq!(
                get_table_columns(&pool, name).await.unwrap(),
                ["a", "b", "x"]
            );
            let indexes = get_indexes(&pool, name).await.unwrap();
            assert!(
                indexes.iter().any(|i| i.primary && i.columns == ["b", "a"]),
                "{name}"
            );
        }
        let weird = "\"main\".\"we'ird\"";
        assert_eq!(
            get_table_column_details(&pool, weird).await.unwrap().len(),
            3
        );
        let fks = get_foreign_keys(&pool, weird).await.unwrap();
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].columns, ["b", "a"]);
        assert_eq!(fks[0].ref_table, "p");
        let indexes = get_indexes(&pool, weird).await.unwrap();
        assert_eq!(indexes[0].columns, ["a"]);

        // The attached schema's `p` is a different table.
        let aux = "\"aux\".\"p\"";
        let cols = get_table_column_details(&pool, aux).await.unwrap();
        let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["z", "y"]);
        assert_eq!(get_primary_keys(&pool, aux).await.unwrap(), ["z"]);
        let indexes = get_indexes(&pool, aux).await.unwrap();
        assert!(indexes
            .iter()
            .any(|i| i.name == "p_y" && i.columns == ["y"]));
        assert!(get_foreign_keys(&pool, aux).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn several_statements_keep_the_first_row_set() {
        let pool = mem_pool().await;
        let r = execute_query(
            &pool,
            "INSERT INTO t VALUES (5); SELECT a FROM t; SELECT 2 AS b, 3 AS c",
        )
        .await
        .unwrap();
        assert_eq!(r.columns.len(), 1);
        assert_eq!(r.rows, vec![vec!["5".to_string()]]);
    }

    #[tokio::test]
    async fn row_cap_truncates_and_flags() {
        let pool = mem_pool().await;
        for i in 0..10 {
            sqlx::query(&format!("INSERT INTO t VALUES ({i})"))
                .execute(&pool)
                .await
                .unwrap();
        }
        let r = execute_query_limited(&pool, "SELECT a FROM t ORDER BY a", Some(3))
            .await
            .unwrap();
        assert_eq!(r.rows.len(), 3);
        assert!(r.truncated);
        let r = execute_query_limited(&pool, "SELECT a FROM t", Some(10))
            .await
            .unwrap();
        assert_eq!(r.rows.len(), 10);
        assert!(!r.truncated);
        let r = execute_query_limited(&pool, "SELECT a FROM t", None)
            .await
            .unwrap();
        assert!(!r.truncated);
    }

    #[tokio::test]
    async fn row_cap_still_runs_later_statements_of_the_batch() {
        let pool = mem_pool().await;
        // Enough rows that the driver can't have produced them all before the
        // cap is reached.
        let sql =
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 50000) \
                   SELECT x FROM n; INSERT INTO t VALUES (99)";
        let r = execute_query_limited(&pool, sql, Some(1)).await.unwrap();
        assert_eq!(r.rows.len(), 1);
        assert!(r.truncated);
        let r = execute_query(&pool, "SELECT a FROM t WHERE a = 99")
            .await
            .unwrap();
        assert_eq!(
            r.rows.len(),
            1,
            "the INSERT after the capped SELECT must run"
        );
    }

    #[tokio::test]
    async fn column_names_follow_a_rename() {
        let pool = mem_pool().await;
        execute_query(&pool, "INSERT INTO t VALUES (1)")
            .await
            .unwrap();
        let before = execute_query(&pool, "SELECT * FROM t").await.unwrap();
        assert_eq!(before.columns[0].name, "a");
        execute_query(&pool, "ALTER TABLE t RENAME COLUMN a TO b")
            .await
            .unwrap();
        // Same SQL again: a cached statement must not keep the old names.
        let after = execute_query(&pool, "SELECT * FROM t").await.unwrap();
        assert_eq!(after.columns[0].name, "b");
    }

    /// Aborting a transaction block mid-way (GUI cancel) must not hand the
    /// connection back to the pool with the transaction still open: the next
    /// statement would run inside it (never committed) and the database
    /// would stay locked for every other connection.
    #[tokio::test]
    async fn aborted_transaction_does_not_leak_an_open_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}?mode=rwc", dir.path().join("t.db").display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (a INTEGER)")
            .execute(&pool)
            .await
            .unwrap();

        // The second statement streams rows for a long time.
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (1)".to_string(),
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c \
             WHERE x < 100000000) SELECT x FROM c"
                .to_string(),
            "COMMIT".to_string(),
        ];
        let task_pool = pool.clone();
        let handle = tokio::spawn(async move { execute_transaction(&task_pool, &stmts).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!handle.is_finished(), "the block must still be running");
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());

        // Same pool: an autocommit INSERT.
        execute_query(&pool, "INSERT INTO t VALUES (2)")
            .await
            .unwrap();

        // A separate, fresh pool sees it committed (and the aborted INSERT
        // rolled back), without hitting a lock.
        let fresh = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT a FROM t ORDER BY a")
            .fetch_all(&fresh)
            .await
            .unwrap();
        assert_eq!(rows, vec![(2,)]);
        sqlx::query("INSERT INTO t VALUES (3)")
            .execute(&fresh)
            .await
            .expect("the database must not stay locked");
    }

    #[tokio::test]
    async fn column_names_follow_a_rename_made_on_another_connection() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}?mode=rwc", dir.path().join("t.db").display());
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        execute_query(&pool, "CREATE TABLE t (a INTEGER)")
            .await
            .unwrap();
        execute_query(&pool, "INSERT INTO t VALUES (1)")
            .await
            .unwrap();
        // Hold one connection: the pool serves the queries with the other.
        let mut held = pool.acquire().await.unwrap();
        let before = execute_query(&pool, "SELECT * FROM t").await.unwrap();
        assert_eq!(before.columns[0].name, "a");
        sqlx::query("ALTER TABLE t RENAME COLUMN a TO b")
            .execute(&mut *held)
            .await
            .unwrap();
        let after = execute_query(&pool, "SELECT * FROM t").await.unwrap();
        assert_eq!(after.columns[0].name, "b");
    }
}
