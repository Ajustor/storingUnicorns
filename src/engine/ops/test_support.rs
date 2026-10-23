use sqlx::sqlite::SqlitePoolOptions;

use crate::engine::db::DatabaseConnection;

/// In-memory SQLite connection. `max_connections(1)` makes every query reuse
/// the same connection, hence the same in-memory database.
pub async fn sqlite_mem(setup: &[&str]) -> DatabaseConnection {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for stmt in setup {
        sqlx::query(stmt).execute(&pool).await.unwrap();
    }
    DatabaseConnection::SQLite(pool)
}

pub async fn count(conn: &DatabaseConnection, table: &str) -> String {
    let r = conn
        .execute_query(&format!("SELECT COUNT(*) AS n FROM {table}"))
        .await
        .unwrap();
    r.rows[0][0].clone()
}

/// A MongoDB connection that never connects (nothing listens on port 9;
/// the driver only connects on the first operation).
pub async fn mongo_offline() -> DatabaseConnection {
    let client =
        mongodb::Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=200")
            .await
            .unwrap();
    DatabaseConnection::MongoDB(crate::engine::nosql::mongo::MongoClient {
        client,
        default_db: None,
    })
}

/// A Redis connection that never connects (`Client::open` does no I/O).
pub fn redis_offline() -> DatabaseConnection {
    DatabaseConnection::Redis(crate::engine::nosql::redis::RedisClient {
        client: Box::new(::redis::Client::open("redis://127.0.0.1:9/").unwrap()),
        managers: Default::default(),
        default_db: 0,
    })
}
