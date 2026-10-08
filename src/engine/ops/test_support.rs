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
