use anyhow::Result;
use sqlx::mysql::{MySqlConnectOptions, MySqlSslMode};
use sqlx::pool::PoolOptions;
use sqlx::{mysql::MySqlRow, Column as SqlxColumn, MySql, MySqlPool, Row, TypeInfo};

use crate::engine::models::{
    Column, ConnectionConfig, DbAuthMethod, ForeignKeyInfo, IncomingForeignKey, IndexInfo,
    QueryResult, SchemaInfo, SslMode, NULL_CELL,
};

use super::utils::{
    fetch_rows_and_result, group_foreign_keys, group_incoming_foreign_keys, group_indexes,
    group_tables_by_schema, split_qualified, TxConnection,
};

/// Options built field by field: no URL, so any character works in the
/// password. With Google Cloud IAM, the user is the account's database user,
/// the token goes in clear text (over TLS: `mysql_clear_password`) and
/// `connect` adds it as password.
pub fn connect_options(config: &ConnectionConfig) -> MySqlConnectOptions {
    // NOTE: `VerifyCa` still checks the host name. sqlx 0.8.6's
    // `NoHostnameTlsVerifier` (sqlx-core/src/net/tls/tls_rustls.rs) only
    // ignores `CertificateError::NotValidForName`, while rustls 0.23 reports
    // a mismatch as `NotValidForNameContext`. Not patched here: `tls::explain`
    // says so and points to `Require` instead.
    let mut o = MySqlConnectOptions::new()
        .host(config.host.as_deref().unwrap_or("localhost"))
        .port(config.port.unwrap_or(3306))
        .username(&login_user(config))
        .ssl_mode(match config.effective_ssl_mode() {
            SslMode::Disable => MySqlSslMode::Disabled,
            SslMode::Prefer => MySqlSslMode::Preferred,
            SslMode::Require => MySqlSslMode::Required,
            SslMode::VerifyCa => MySqlSslMode::VerifyCa,
            SslMode::VerifyFull => MySqlSslMode::VerifyIdentity,
        });
    // Unset rather than empty, as the URL path used to: no default database.
    if !config.database.is_empty() {
        o = o.database(&config.database);
    }
    if iam(config) {
        o = o.enable_cleartext_plugin(true);
    } else if let Some(p) = &config.password {
        o = o.password(p);
    }
    if let Some(ca) = &config.ssl_ca {
        o = o.ssl_ca(ca);
    }
    o
}

fn iam(config: &ConnectionConfig) -> bool {
    config.effective_db_auth() == DbAuthMethod::GcpIam
}

/// The user name sent to the server.
fn login_user(config: &ConnectionConfig) -> String {
    let user = config.username.as_deref().unwrap_or("root");
    if iam(config) {
        super::gcp_iam::database_user(&config.db_type, user)
    } else {
        user.to_string()
    }
}

/// Connect to MySQL. In `Prefer` mode, a failed TLS handshake is retried
/// without TLS (see `tls::should_retry_plain`).
/// With Google Cloud IAM, signs in with a `gcloud` token, renewed while the
/// pool lives (at least `Require`: no plain-text retry).
pub async fn connect(config: &ConnectionConfig, pool: PoolOptions<MySql>) -> Result<MySqlPool> {
    let options = connect_options(config);
    if iam(config) {
        use super::gcp_iam;
        return gcp_iam::connect_renewing(
            pool,
            gcp_iam::account(config)?,
            gcp_iam::gcloud_provider(),
            gcp_iam::Timing::DEFAULT,
            move |token| options.clone().password(token.secret()),
        )
        .await;
    }
    match pool.clone().connect_with(options.clone()).await {
        Err(e) if super::tls::should_retry_plain(config.effective_ssl_mode(), &e) => {
            tracing::debug!("TLS failed in Prefer mode ({e}); retrying without TLS");
            pool.connect_with(options.ssl_mode(MySqlSslMode::Disabled))
                .await
                .map_err(|plain| anyhow::anyhow!("TLS : {e} ; sans chiffrement : {plain}"))
        }
        result => Ok(result?),
    }
}

/// Convert fetched rows into a `QueryResult`.
///
/// When the statement returned rows (SELECT, `... RETURNING`), `rows_affected`
/// is the number of rows returned; otherwise it is `affected`, the count
/// reported by the database (UPDATE/DELETE/INSERT).
fn rows_to_result(rows: &[MySqlRow], affected: u64) -> QueryResult {
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
/// count from the same stream. User SQL goes through the text protocol: the
/// prepared-statement protocol rejects `START TRANSACTION`, `LOCK TABLES`,
/// several statements in one string, etc.
async fn run_statement<'c, E>(
    executor: E,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    let (rows, done, truncated) = fetch_rows_and_result(executor, query, max_rows, false).await?;
    Ok(QueryResult {
        truncated,
        ..rows_to_result(&rows, done.rows_affected())
    })
}

/// Execute a query on MySQL, keeping at most `max_rows` rows (all when
/// `None`); `truncated` is set when more rows were available.
pub async fn execute_query_limited(
    pool: &MySqlPool,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    run_statement(pool, query, max_rows).await
}

/// Execute a sequence of statements as one transaction on a single dedicated
/// connection. Statements include the user's `BEGIN`/`COMMIT`/`ROLLBACK`.
/// On any error the transaction is rolled back and the error is returned.
pub async fn execute_transaction(pool: &MySqlPool, statements: &[String]) -> Result<QueryResult> {
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

/// Get tables grouped by schema
pub async fn get_tables_by_schema(pool: &MySqlPool) -> Result<Vec<SchemaInfo>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT CAST(TABLE_SCHEMA AS CHAR), CAST(TABLE_NAME AS CHAR)
         FROM information_schema.tables
         WHERE TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
         ORDER BY TABLE_SCHEMA, TABLE_NAME",
    )
    .fetch_all(pool)
    .await?;
    Ok(group_tables_by_schema(rows))
}

/// `(schema, table)` of a possibly qualified/quoted name; the schema is
/// empty when not given, which the queries turn into `DATABASE()`.
fn split_table(table_name: &str) -> (String, String) {
    let (schema, table) = split_qualified(table_name);
    (schema.unwrap_or_default(), table)
}

/// Columns of a table as `(name, type, nullable, primary key)`, in order
/// (current database when the name is not qualified). `information_schema`
/// columns are cast to CHAR: MySQL 8 reports them with a binary collation,
/// which would not decode as text.
async fn column_rows(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<Vec<(String, String, bool, bool)>> {
    let (schema, table) = split_table(table_name);
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR),
                CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_KEY AS CHAR)
         FROM information_schema.COLUMNS
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
         ORDER BY ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, ty, nullable, key)| (name, ty, nullable == "YES", key == "PRI"))
        .collect())
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, _, nullable, _)| (name, nullable))
        .collect())
}

/// Get primary key columns for a table, in key order
pub async fn get_primary_keys(pool: &MySqlPool, table_name: &str) -> Result<Vec<String>> {
    let (schema, table) = split_table(table_name);
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT CAST(COLUMN_NAME AS CHAR)
         FROM information_schema.KEY_COLUMN_USAGE
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
           AND CONSTRAINT_NAME = 'PRIMARY'
         ORDER BY ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(name,)| name).collect())
}

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(pool: &MySqlPool, table_name: &str) -> Result<Vec<String>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, ..)| name)
        .collect())
}

/// Get full column details for a table (for schema modification). The type
/// is the full `COLUMN_TYPE` (`varchar(50)`, `int unsigned`).
pub async fn get_table_column_details(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<Vec<crate::engine::models::Column>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(
            |(name, type_name, nullable, is_primary_key)| crate::engine::models::Column {
                name,
                type_name,
                nullable,
                is_primary_key,
            },
        )
        .collect())
}

/// Get the indexes of a table from `information_schema.STATISTICS`
/// (current database when the name is not qualified).
pub async fn get_indexes(pool: &MySqlPool, table_name: &str) -> Result<Vec<IndexInfo>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT CAST(INDEX_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), NON_UNIQUE
         FROM information_schema.STATISTICS
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
         ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(group_indexes(rows.into_iter().map(
        |(name, column, non_unique)| {
            let primary = name == "PRIMARY";
            // NULL column: a functional key part.
            let column = column.unwrap_or_else(|| "<expression>".to_string());
            (name, column, non_unique == 0, primary)
        },
    )))
}

/// Get the foreign keys of a table from `information_schema.KEY_COLUMN_USAGE`
/// (current database when the name is not qualified). The referenced table
/// is qualified with its schema when it lives in another one.
pub async fn get_foreign_keys(pool: &MySqlPool, table_name: &str) -> Result<Vec<ForeignKeyInfo>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(CONSTRAINT_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR),
                CAST(TABLE_SCHEMA AS CHAR), CAST(REFERENCED_TABLE_SCHEMA AS CHAR),
                CAST(REFERENCED_TABLE_NAME AS CHAR), CAST(REFERENCED_COLUMN_NAME AS CHAR)
         FROM information_schema.KEY_COLUMN_USAGE
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
           AND REFERENCED_TABLE_NAME IS NOT NULL
         ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(group_foreign_keys(rows.into_iter().map(
        |(name, column, own_schema, ref_schema, ref_table, ref_column)| {
            let ref_table = if ref_schema == own_schema {
                ref_table
            } else {
                format!("{ref_schema}.{ref_table}")
            };
            (name, column, ref_table, ref_column)
        },
    )))
}

/// Get the foreign keys of other tables pointing at a table, from
/// `information_schema.KEY_COLUMN_USAGE` (`REFERENCED_TABLE_*` = the table,
/// current database when the name is not qualified). The referencing table
/// is always qualified with its schema (`schema.table`).
pub async fn get_incoming_foreign_keys(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<Vec<IncomingForeignKey>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(CONSTRAINT_NAME AS CHAR), CAST(TABLE_SCHEMA AS CHAR),
                CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR),
                CAST(REFERENCED_COLUMN_NAME AS CHAR)
         FROM information_schema.KEY_COLUMN_USAGE
         WHERE REFERENCED_TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE())
           AND REFERENCED_TABLE_NAME = ?
         ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(group_incoming_foreign_keys(rows.into_iter().map(
        |(name, own_schema, own_table, column, ref_column)| {
            (
                name,
                format!("{own_schema}.{own_table}"),
                column,
                ref_column,
            )
        },
    )))
}

/// Test the connection
pub async fn test(pool: &MySqlPool) -> Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

/// Close the connection
pub async fn close(pool: MySqlPool) {
    pool.close().await;
}

/// Text of a value. Rows come from the text protocol (`run_statement`), so
/// every non-NULL value already is its textual form, exactly as MySQL prints
/// it (DATETIME without offset, DECIMAL scale kept, JSON as stored, BIGINT
/// UNSIGNED in full); BLOB-like values are shown lossily.
fn get_value(row: &MySqlRow, index: usize) -> String {
    use sqlx::ValueRef;
    match row.try_get_raw(index) {
        Ok(raw) if !raw.is_null() => {}
        _ => return NULL_CELL.to_string(),
    }
    row.try_get_unchecked::<&[u8], _>(index)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_else(|_| NULL_CELL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{ConnectionConfig, DatabaseType, SslMode};
    use sqlx::mysql::MySqlSslMode;

    fn mysql(c: ConnectionConfig) -> ConnectionConfig {
        ConnectionConfig {
            db_type: DatabaseType::MySQL,
            ..c
        }
    }

    #[test]
    fn an_empty_database_is_not_sent() {
        let o = connect_options(&mysql(ConnectionConfig {
            database: String::new(),
            ..Default::default()
        }));
        assert_eq!(o.get_database(), None);
    }

    #[test]
    fn options_come_from_the_fields() {
        let c = mysql(ConnectionConfig {
            host: Some("db.example.com".into()),
            port: Some(3307),
            username: Some("alice".into()),
            password: Some("a@b:c/d#e?f%g".into()),
            database: "app".into(),
            ssl_mode: Some(SslMode::VerifyFull),
            ssl_ca: Some("C:/certs/ca.pem".into()),
            ..Default::default()
        });
        let o = connect_options(&c);
        assert_eq!(o.get_host(), "db.example.com");
        assert_eq!(o.get_port(), 3307);
        assert_eq!(o.get_username(), "alice");
        assert_eq!(o.get_database(), Some("app"));
        assert!(matches!(o.get_ssl_mode(), MySqlSslMode::VerifyIdentity));
        // No getter for the password or the CA: check the Debug output.
        let debug = format!("{o:?}");
        assert!(
            debug.contains(r#"password: Some("a@b:c/d#e?f%g")"#),
            "{debug}"
        );
        assert!(debug.contains("C:/certs/ca.pem"), "{debug}");
    }

    #[test]
    fn defaults_and_ssl_mapping() {
        let o = connect_options(&mysql(ConnectionConfig {
            host: None,
            port: None,
            username: None,
            ..Default::default()
        }));
        assert_eq!(o.get_host(), "localhost");
        assert_eq!(o.get_port(), 3306);
        assert_eq!(o.get_username(), "root");
        assert!(matches!(o.get_ssl_mode(), MySqlSslMode::Preferred));
        for (mode, want) in [
            (SslMode::Disable, MySqlSslMode::Disabled),
            (SslMode::Prefer, MySqlSslMode::Preferred),
            (SslMode::Require, MySqlSslMode::Required),
            (SslMode::VerifyCa, MySqlSslMode::VerifyCa),
            (SslMode::VerifyFull, MySqlSslMode::VerifyIdentity),
        ] {
            let c = mysql(ConnectionConfig {
                ssl_mode: Some(mode),
                ..Default::default()
            });
            assert_eq!(
                std::mem::discriminant(&connect_options(&c).get_ssl_mode()),
                std::mem::discriminant(&want)
            );
        }
    }

    #[test]
    fn gcp_iam_options_use_cleartext_over_tls() {
        let c = mysql(ConnectionConfig {
            username: Some("jane@example.com".into()),
            password: Some("old".into()),
            db_auth: Some(crate::engine::models::DbAuthMethod::GcpIam),
            ..Default::default()
        });
        let o = connect_options(&c);
        assert_eq!(o.get_username(), "jane");
        assert!(matches!(o.get_ssl_mode(), MySqlSslMode::Required));
        let debug = format!("{o:?}");
        assert!(debug.contains("password: None"), "{debug}");
        assert!(debug.contains("enable_cleartext_plugin: true"), "{debug}");
        // Not with a password.
        let o = connect_options(&ConnectionConfig { db_auth: None, ..c });
        assert!(format!("{o:?}").contains("enable_cleartext_plugin: false"));
    }
}
