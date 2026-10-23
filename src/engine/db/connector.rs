use anyhow::Result;
use sqlx::pool::PoolOptions;
use sqlx::{Database, MySqlPool, PgPool, SqlitePool};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::engine::models::{
    ConnectionConfig, DatabaseType, DbAuthMethod, ForeignKeyInfo, IncomingForeignKey, IndexInfo,
    QueryResult, SchemaInfo,
};

pub use super::sqlserver::SqlServerClient;
use super::{azure, gcp_iam, mysql, postgres, sqlite, sqlserver, tls, utils};
use crate::engine::nosql::mongo::{self, CollectionRef, MongoClient};
use crate::engine::nosql::redis::{self, RedisClient};

/// Error of a SQL operation asked of a MongoDB / Redis connection.
pub fn not_sql(db: &DatabaseType) -> anyhow::Error {
    anyhow::anyhow!("Opération SQL indisponible pour {db}")
}

/// Documents sampled to list a collection's fields.
const SAMPLE_SIZE: u32 = 200;

/// The collection a table name of a MongoDB connection designates.
fn collection(table_name: &str) -> Result<CollectionRef> {
    CollectionRef::parse(table_name).ok_or_else(|| {
        anyhow::anyhow!("Collection invalide : « {table_name} » (attendu base.collection)")
    })
}

/// Redis has no columns, keys or indexes.
fn no_tables() -> anyhow::Error {
    anyhow::anyhow!("Redis : pas de tables ni de colonnes")
}

/// A known TLS failure gets a French hint; the driver's message stays in
/// parentheses for diagnosis. Other errors are unchanged.
fn explain_tls(e: anyhow::Error, config: &ConnectionConfig) -> anyhow::Error {
    let host = config.host.as_deref().unwrap_or("localhost");
    match tls::explain(&format!("{e:#}"), host, config.effective_ssl_mode()) {
        Some(hint) => anyhow::anyhow!("{hint}\n({e:#})"),
        None => e,
    }
}

/// A login refused with Google Cloud IAM gets a hint about the IAM user on
/// the instance; other errors are unchanged.
fn explain_iam(e: anyhow::Error, config: &ConnectionConfig) -> anyhow::Error {
    if config.effective_db_auth() != DbAuthMethod::GcpIam {
        return e;
    }
    let account = config.username.as_deref().unwrap_or_default();
    match gcp_iam::login_hint(&format!("{e:#}"), &config.db_type, account) {
        Some(hint) => anyhow::anyhow!(
            "{hint}
({e:#})"
        ),
        None => e,
    }
}

/// Pool settings: the sqlx defaults, or with `opened` a single session kept
/// for the pool's whole life (never closed for being idle or old) whose
/// openings are counted in `opened`.
fn pool_options<DB: Database>(opened: Option<&Arc<AtomicU64>>) -> PoolOptions<DB> {
    let Some(opened) = opened else {
        return PoolOptions::new();
    };
    let opened = opened.clone();
    PoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .after_connect(move |_, _| {
            opened.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        })
}

/// Unified database connection handle
pub enum DatabaseConnection {
    Postgres(PgPool),
    MySQL(MySqlPool),
    SQLite(SqlitePool),
    SQLServer(SqlServerClient),
    Azure(SqlServerClient),
    MongoDB(MongoClient),
    Redis(RedisClient),
}

impl DatabaseConnection {
    /// Connect to a database using the provided configuration
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        Self::connect_with(config, None).await
    }

    /// A connection that always runs on one same database session, as a
    /// transaction left open between statements needs. `opened` counts the
    /// sessions it opened: when it grows after the first one, the session
    /// was lost and replaced (the server rolled back its transaction).
    pub async fn connect_dedicated(
        config: &ConnectionConfig,
        opened: Arc<AtomicU64>,
    ) -> Result<Self> {
        Self::connect_with(config, Some(opened)).await
    }

    async fn connect_with(
        config: &ConnectionConfig,
        opened: Option<Arc<AtomicU64>>,
    ) -> Result<Self> {
        let opened = opened.as_ref();
        match config.db_type {
            DatabaseType::Postgres => {
                tls::check_ca_for(config.effective_ssl_mode(), config.ssl_ca.as_deref())
                    .map_err(anyhow::Error::msg)?;
                let pool = postgres::connect(config, pool_options(opened))
                    .await
                    .map_err(|e| explain_iam(explain_tls(e, config), config))?;
                Ok(DatabaseConnection::Postgres(pool))
            }
            DatabaseType::MySQL => {
                tls::check_ca_for(config.effective_ssl_mode(), config.ssl_ca.as_deref())
                    .map_err(anyhow::Error::msg)?;
                let pool = mysql::connect(config, pool_options(opened))
                    .await
                    .map_err(|e| explain_iam(explain_tls(e, config), config))?;
                Ok(DatabaseConnection::MySQL(pool))
            }
            DatabaseType::SQLite => {
                let pool =
                    sqlite::connect(&config.to_connection_string(), pool_options(opened)).await?;
                Ok(DatabaseConnection::SQLite(pool))
            }
            // One client per connection: dedicated already.
            DatabaseType::SQLServer | DatabaseType::Azure => {
                let client = match opened {
                    None if config.db_type == DatabaseType::Azure => azure::connect(config).await?,
                    None => sqlserver::connect(config).await?,
                    Some(opened) => SqlServerClient::counted(
                        config.clone(),
                        sqlserver::reopen(config).await?,
                        opened.clone(),
                    ),
                };
                Ok(match config.db_type {
                    DatabaseType::Azure => DatabaseConnection::Azure(client),
                    _ => DatabaseConnection::SQLServer(client),
                })
            }
            // No dedicated session: there is no manual transaction mode.
            DatabaseType::MongoDB | DatabaseType::Redis if opened.is_some() => {
                Err(anyhow::anyhow!(
                    "Mode transaction manuel indisponible pour {}",
                    config.db_type
                ))
            }
            DatabaseType::MongoDB => Ok(DatabaseConnection::MongoDB(mongo::connect(config).await?)),
            DatabaseType::Redis => Ok(DatabaseConnection::Redis(redis::connect(config).await?)),
        }
    }

    /// The MongoDB client of this connection.
    #[allow(dead_code)] // used by the GUI and the TUI (plan 13, Tasks 4 and 5)
    pub fn mongo(&self) -> Result<&MongoClient> {
        match self {
            DatabaseConnection::MongoDB(c) => Ok(c),
            _ => anyhow::bail!("{} : connexion non MongoDB", self.db_type()),
        }
    }

    /// The Redis client of this connection.
    #[allow(dead_code)] // used by the GUI and the TUI (plan 13, Tasks 4 and 5)
    pub fn redis(&self) -> Result<&RedisClient> {
        match self {
            DatabaseConnection::Redis(c) => Ok(c),
            _ => anyhow::bail!("{} : connexion non Redis", self.db_type()),
        }
    }

    /// Execute a query and return results
    pub async fn execute_query(&self, query: &str) -> Result<QueryResult> {
        self.execute_query_limited(query, None).await
    }

    /// Execute a query, keeping at most `max_rows` rows (all when `None`).
    /// `QueryResult::truncated` is set when more rows were available.
    pub async fn execute_query_limited(
        &self,
        query: &str,
        max_rows: Option<usize>,
    ) -> Result<QueryResult> {
        let start = Instant::now();

        let result = match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::execute_query_limited(pool, query, max_rows).await?
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::execute_query_limited(pool, query, max_rows).await?
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::execute_query_limited(pool, query, max_rows).await?
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::execute_query_limited(client, query, max_rows).await?
            }
            DatabaseConnection::Azure(client) => {
                azure::execute_query_limited(client, query, max_rows).await?
            }
            DatabaseConnection::MongoDB(_) | DatabaseConnection::Redis(_) => {
                return Err(not_sql(&self.db_type()))
            }
        };

        Ok(QueryResult {
            execution_time_ms: start.elapsed().as_millis(),
            ..result
        })
    }

    /// Execute a transaction block (the statements between `BEGIN` and
    /// `COMMIT`/`ROLLBACK`, inclusive) atomically on a single dedicated
    /// connection. On any error the transaction is rolled back.
    pub async fn execute_transaction(&self, statements: &[String]) -> Result<QueryResult> {
        let start = Instant::now();

        let result = match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::execute_transaction(pool, statements).await?
            }
            DatabaseConnection::MySQL(pool) => mysql::execute_transaction(pool, statements).await?,
            DatabaseConnection::SQLite(pool) => {
                sqlite::execute_transaction(pool, statements).await?
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::execute_transaction(client, statements).await?
            }
            DatabaseConnection::Azure(client) => {
                sqlserver::execute_transaction(client, statements).await?
            }
            DatabaseConnection::MongoDB(_) | DatabaseConnection::Redis(_) => {
                return Err(not_sql(&self.db_type()))
            }
        };

        Ok(QueryResult {
            execution_time_ms: start.elapsed().as_millis(),
            ..result
        })
    }

    /// Whether scripts run on this connection as T-SQL batches (split on
    /// `GO`, see `ops::query::run_script`): SQL Server and Azure.
    pub fn runs_tsql_batches(&self) -> bool {
        self.db_type().runs_tsql_batches()
    }

    /// SQL Server / Azure: run one script batch in a single round trip
    /// (`sqlserver::execute_batch`); one result per result set.
    pub async fn execute_tsql_batch(
        &self,
        batch: &str,
        max_rows: Option<usize>,
        auto: bool,
    ) -> Result<Vec<QueryResult>> {
        match self {
            DatabaseConnection::SQLServer(client) | DatabaseConnection::Azure(client) => {
                sqlserver::execute_batch(client, batch, max_rows, auto).await
            }
            _ => anyhow::bail!("T-SQL batches need SQL Server"),
        }
    }

    /// SQL Server / Azure: roll back a transaction left open on the
    /// session; returns whether there was one (always false elsewhere).
    pub async fn rollback_open_transaction(&self) -> Result<bool> {
        match self {
            DatabaseConnection::SQLServer(client) | DatabaseConnection::Azure(client) => {
                sqlserver::rollback_open_transaction(client).await
            }
            _ => Ok(false),
        }
    }

    /// The database type of this connection.
    pub fn db_type(&self) -> DatabaseType {
        match self {
            DatabaseConnection::Postgres(_) => DatabaseType::Postgres,
            DatabaseConnection::MySQL(_) => DatabaseType::MySQL,
            DatabaseConnection::SQLite(_) => DatabaseType::SQLite,
            DatabaseConnection::SQLServer(_) => DatabaseType::SQLServer,
            DatabaseConnection::Azure(_) => DatabaseType::Azure,
            DatabaseConnection::MongoDB(_) => DatabaseType::MongoDB,
            DatabaseConnection::Redis(_) => DatabaseType::Redis,
        }
    }

    /// Run `statements` one by one in a transaction (the dialect's BEGIN and
    /// COMMIT) on a dedicated connection. Statement `i` must affect exactly
    /// `expected[i]` rows when that is `Some`: on a different count, or any
    /// error, everything is rolled back and an error returned. Returns the
    /// total number of affected rows.
    pub async fn execute_checked_batch(
        &self,
        statements: &[String],
        expected: &[Option<u64>],
    ) -> Result<u64> {
        if !self.db_type().is_sql() {
            return Err(not_sql(&self.db_type()));
        }
        let (begin, _) = utils::transaction_bounds(&self.db_type());
        match self {
            DatabaseConnection::Postgres(pool) => {
                utils::execute_checked(pool, begin, statements, expected).await
            }
            DatabaseConnection::MySQL(pool) => {
                utils::execute_checked(pool, begin, statements, expected).await
            }
            DatabaseConnection::SQLite(pool) => {
                utils::execute_checked(pool, begin, statements, expected).await
            }
            DatabaseConnection::SQLServer(client) | DatabaseConnection::Azure(client) => {
                sqlserver::execute_checked_batch(client, statements, expected).await
            }
            DatabaseConnection::MongoDB(_) | DatabaseConnection::Redis(_) => {
                Err(not_sql(&self.db_type()))
            }
        }
    }

    /// Test the connection
    pub async fn test(&self) -> Result<()> {
        match self {
            DatabaseConnection::Postgres(pool) => postgres::test(pool).await,
            DatabaseConnection::MySQL(pool) => mysql::test(pool).await,
            DatabaseConnection::SQLite(pool) => sqlite::test(pool).await,
            DatabaseConnection::SQLServer(client) => sqlserver::test(client).await,
            DatabaseConnection::Azure(client) => azure::test(client).await,
            DatabaseConnection::MongoDB(c) => mongo::test(c).await,
            DatabaseConnection::Redis(c) => redis::test(c).await,
        }
    }

    /// Get list of tables/schemas (legacy - returns flat list)
    #[allow(dead_code)]
    pub async fn get_tables(&self) -> Result<Vec<String>> {
        let schemas = self.get_tables_by_schema().await?;
        Ok(schemas
            .into_iter()
            .flat_map(|s| {
                s.tables.into_iter().map(move |t| {
                    if s.name.is_empty() {
                        t
                    } else {
                        format!("{}.{}", s.name, t)
                    }
                })
            })
            .collect())
    }

    /// Get list of tables grouped by schema
    pub async fn get_tables_by_schema(&self) -> Result<Vec<SchemaInfo>> {
        match self {
            DatabaseConnection::Postgres(pool) => postgres::get_tables_by_schema(pool).await,
            DatabaseConnection::MySQL(pool) => mysql::get_tables_by_schema(pool).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_tables_by_schema(pool).await,
            DatabaseConnection::SQLServer(client) => sqlserver::get_tables_by_schema(client).await,
            DatabaseConnection::Azure(client) => azure::get_tables_by_schema(client).await,
            DatabaseConnection::MongoDB(c) => mongo::get_tables_by_schema(c).await,
            DatabaseConnection::Redis(c) => redis::databases(c).await,
        }
    }

    /// Get column metadata (name, type, nullable) for a table
    /// Returns a map of column_name -> nullable
    pub async fn get_column_nullability(
        &self,
        table_name: &str,
    ) -> Result<std::collections::HashMap<String, bool>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_column_nullability(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::get_column_nullability(pool, table_name).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::get_column_nullability(pool, table_name).await
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_column_nullability(client, table_name).await
            }
            DatabaseConnection::MongoDB(c) => {
                Ok(
                    mongo::sample_fields(c, &collection(table_name)?, SAMPLE_SIZE)
                        .await?
                        .into_iter()
                        .map(|col| (col.name, col.nullable))
                        .collect(),
                )
            }
            DatabaseConnection::Redis(_) => Err(no_tables()),
            DatabaseConnection::Azure(client) => {
                sqlserver::get_column_nullability(client, table_name).await
            }
        }
    }

    /// Get primary key columns for a table
    pub async fn get_primary_keys(&self, table_name: &str) -> Result<Vec<String>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_primary_keys(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => mysql::get_primary_keys(pool, table_name).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_primary_keys(pool, table_name).await,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_primary_keys(client, table_name).await
            }
            DatabaseConnection::MongoDB(_) => {
                collection(table_name)?;
                Ok(vec!["_id".to_string()])
            }
            DatabaseConnection::Redis(_) => Err(no_tables()),
            DatabaseConnection::Azure(client) => {
                sqlserver::get_primary_keys(client, table_name).await
            }
        }
    }

    /// Get column names for a specific table (for autocompletion)
    #[allow(dead_code)]
    pub async fn get_table_columns(&self, table_name: &str) -> Result<Vec<String>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_table_columns(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => mysql::get_table_columns(pool, table_name).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_table_columns(pool, table_name).await,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_table_columns(client, table_name).await
            }
            DatabaseConnection::MongoDB(c) => {
                Ok(
                    mongo::sample_fields(c, &collection(table_name)?, SAMPLE_SIZE)
                        .await?
                        .into_iter()
                        .map(|col| col.name)
                        .collect(),
                )
            }
            DatabaseConnection::Redis(_) => Err(no_tables()),
            DatabaseConnection::Azure(client) => {
                sqlserver::get_table_columns(client, table_name).await
            }
        }
    }

    /// Get full column metadata for a table (for schema modification)
    pub async fn get_table_column_details(
        &self,
        table_name: &str,
    ) -> Result<Vec<crate::engine::models::Column>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_table_column_details(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::get_table_column_details(pool, table_name).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::get_table_column_details(pool, table_name).await
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_table_column_details(client, table_name).await
            }
            DatabaseConnection::MongoDB(c) => {
                mongo::sample_fields(c, &collection(table_name)?, SAMPLE_SIZE).await
            }
            DatabaseConnection::Redis(_) => Err(no_tables()),
            DatabaseConnection::Azure(client) => {
                sqlserver::get_table_column_details(client, table_name).await
            }
        }
    }

    /// Indexes of a table (including the one backing the primary key).
    pub async fn get_indexes(&self, table_name: &str) -> Result<Vec<IndexInfo>> {
        match self {
            DatabaseConnection::Postgres(pool) => postgres::get_indexes(pool, table_name).await,
            DatabaseConnection::MySQL(pool) => mysql::get_indexes(pool, table_name).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_indexes(pool, table_name).await,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_indexes(client, table_name).await
            }
            DatabaseConnection::Azure(client) => sqlserver::get_indexes(client, table_name).await,
            DatabaseConnection::MongoDB(c) => Ok(mongo::list_indexes(c, &collection(table_name)?)
                .await?
                .iter()
                .map(mongo::index_info)
                .collect()),
            DatabaseConnection::Redis(_) => Err(no_tables()),
        }
    }

    /// Foreign keys declared on a table.
    pub async fn get_foreign_keys(&self, table_name: &str) -> Result<Vec<ForeignKeyInfo>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_foreign_keys(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => mysql::get_foreign_keys(pool, table_name).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_foreign_keys(pool, table_name).await,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_foreign_keys(client, table_name).await
            }
            // No foreign keys between documents.
            DatabaseConnection::MongoDB(_) => Ok(Vec::new()),
            DatabaseConnection::Redis(_) => Err(no_tables()),
            DatabaseConnection::Azure(client) => {
                sqlserver::get_foreign_keys(client, table_name).await
            }
        }
    }

    /// Foreign keys of other tables pointing at a table (referencing tables
    /// qualified as `schema.table`).
    pub async fn get_incoming_foreign_keys(
        &self,
        table_name: &str,
    ) -> Result<Vec<IncomingForeignKey>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_incoming_foreign_keys(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::get_incoming_foreign_keys(pool, table_name).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::get_incoming_foreign_keys(pool, table_name).await
            }
            DatabaseConnection::SQLServer(client) | DatabaseConnection::Azure(client) => {
                sqlserver::get_incoming_foreign_keys(client, table_name).await
            }
            DatabaseConnection::MongoDB(_) => Ok(Vec::new()),
            DatabaseConnection::Redis(_) => Err(no_tables()),
        }
    }

    /// Close the connection's sessions now (a pool shared by other handles
    /// is closed for them too). SQL Server clients close when dropped.
    pub async fn close_sessions(&self) {
        match self {
            DatabaseConnection::Postgres(pool) => pool.close().await,
            DatabaseConnection::MySQL(pool) => pool.close().await,
            DatabaseConnection::SQLite(pool) => pool.close().await,
            DatabaseConnection::SQLServer(_)
            | DatabaseConnection::Azure(_)
            | DatabaseConnection::MongoDB(_)
            | DatabaseConnection::Redis(_) => {}
        }
    }

    /// Close the connection
    pub async fn close(self) {
        match &self {
            DatabaseConnection::Postgres(pool) => postgres::close(pool.clone()).await,
            DatabaseConnection::MySQL(pool) => mysql::close(pool.clone()).await,
            DatabaseConnection::SQLite(pool) => sqlite::close(pool.clone()).await,
            DatabaseConnection::SQLServer(_) => {
                // Tiberius client is dropped automatically
            }
            DatabaseConnection::Azure(_) => {
                // Azure client is dropped automatically
            }
            // The MongoDB pool and the Redis managers close when dropped.
            DatabaseConnection::MongoDB(_) | DatabaseConnection::Redis(_) => {}
        }
    }
}

impl Drop for DatabaseConnection {
    /// The pool is closed with its last handle: the Google Cloud IAM renewal
    /// task holds one more and stops when the pool is marked closed (done
    /// at once by `close()`, without awaiting the sessions' shutdown).
    fn drop(&mut self) {
        match self {
            DatabaseConnection::Postgres(pool) => std::mem::drop(pool.close()),
            DatabaseConnection::MySQL(pool) => std::mem::drop(pool.close()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_a_connection_closes_its_pool() {
        // Lazy: nothing listens on the port, no session is ever opened.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://u:p@127.0.0.1:9/db")
            .unwrap();
        let kept = pool.clone();
        drop(DatabaseConnection::Postgres(pool));
        assert!(kept.is_closed());

        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://u:p@127.0.0.1:9/db")
            .unwrap();
        let kept = pool.clone();
        drop(DatabaseConnection::MySQL(pool));
        assert!(kept.is_closed());
    }
}
