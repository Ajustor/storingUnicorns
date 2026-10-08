use anyhow::Result;

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{Column, DatabaseType, TableDetails};
use crate::engine::services::{
    table_cache::TableCache, ColumnDefinition, SchemaModification, SchemaService,
};

/// Columns of `table`, served from `cache` when fresh (filled otherwise).
async fn cached_columns(
    conn: &DatabaseConnection,
    cache: &TableCache,
    table: &str,
) -> Result<Vec<Column>> {
    match cache.get_column_details(table).await {
        Some(cols) => Ok(cols),
        None => {
            let cols = conn.get_table_column_details(table).await?;
            cache.set(table.to_string(), cols.clone()).await;
            Ok(cols)
        }
    }
}

/// Column definitions of `table`, served from `cache` when fresh.
pub async fn fetch_columns(
    conn: &DatabaseConnection,
    cache: &TableCache,
    table: &str,
) -> Result<Vec<ColumnDefinition>> {
    let columns = cached_columns(conn, cache, table).await?;
    Ok(columns
        .into_iter()
        .map(|c| ColumnDefinition {
            name: c.name,
            data_type: c.type_name,
            nullable: c.nullable,
            is_primary_key: c.is_primary_key,
            default_value: None,
        })
        .collect())
}

/// Columns (through `cache`), indexes and foreign keys of `table`, fetched
/// concurrently.
#[allow(dead_code)] // used by the GUI (plan 3b)
pub async fn table_details(
    conn: &DatabaseConnection,
    cache: &TableCache,
    table: &str,
) -> Result<TableDetails> {
    let (columns, indexes, foreign_keys) = tokio::try_join!(
        cached_columns(conn, cache, table),
        conn.get_indexes(table),
        conn.get_foreign_keys(table),
    )?;
    Ok(TableDetails {
        columns,
        indexes,
        foreign_keys,
    })
}

/// The table a modification applies to.
pub fn table_of(m: &SchemaModification) -> Option<&str> {
    match m {
        SchemaModification::AddColumn { table_name, .. }
        | SchemaModification::DropColumn { table_name, .. }
        | SchemaModification::RenameColumn { table_name, .. }
        | SchemaModification::ModifyColumn { table_name, .. }
        | SchemaModification::CreateTable { table_name, .. }
        | SchemaModification::DropTable { table_name }
        | SchemaModification::AddIndex { table_name, .. }
        | SchemaModification::DropIndex { table_name, .. } => Some(table_name),
        SchemaModification::RenameTable { old_name, .. } => Some(old_name),
    }
}

/// Generate and execute the SQL for `m`; returns the SQL that ran.
pub async fn apply_modification(
    conn: &DatabaseConnection,
    cache: &TableCache,
    m: &SchemaModification,
    db_type: &DatabaseType,
) -> Result<String> {
    let sql = SchemaService::generate_sql(m, db_type);
    conn.execute_query(&sql).await?;
    if let Some(table) = table_of(m) {
        cache.invalidate(table).await;
    }
    Ok(sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::DatabaseType;
    use crate::engine::ops::test_support::sqlite_mem;
    use crate::engine::services::{table_cache::TableCache, ColumnDefinition, SchemaModification};

    #[tokio::test]
    async fn fetch_columns_uses_and_fills_cache() {
        let conn = sqlite_mem(&["CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)"]).await;
        let cache = TableCache::default();
        let cols = fetch_columns(&conn, &cache, "t").await.unwrap();
        assert_eq!(
            cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["id", "name"]
        );
        assert!(cols[0].is_primary_key);
        assert!(cache.get_column_details("t").await.is_some());
    }

    #[tokio::test]
    async fn add_column_executes_and_invalidates_cache() {
        let conn = sqlite_mem(&["CREATE TABLE t (id INTEGER PRIMARY KEY)"]).await;
        let cache = TableCache::default();
        fetch_columns(&conn, &cache, "t").await.unwrap();
        let m = SchemaModification::AddColumn {
            table_name: "t".into(),
            column: ColumnDefinition {
                name: "age".into(),
                data_type: "INTEGER".into(),
                ..Default::default()
            },
        };
        let sql = apply_modification(&conn, &cache, &m, &DatabaseType::SQLite)
            .await
            .unwrap();
        assert!(sql.to_uppercase().contains("ALTER TABLE"));
        assert!(cache.get_column_details("t").await.is_none());
        let cols = fetch_columns(&conn, &cache, "t").await.unwrap();
        assert!(cols.iter().any(|c| c.name == "age"));
    }

    #[tokio::test]
    async fn table_details_lists_indexes_and_foreign_keys() {
        let conn = sqlite_mem(&[
            "CREATE TABLE a (id INTEGER PRIMARY KEY, code TEXT UNIQUE)",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), label TEXT)",
            "CREATE INDEX b_label ON b(label)",
        ])
        .await;
        let cache = TableCache::default();
        let d = table_details(&conn, &cache, "b").await.unwrap();
        assert_eq!(d.columns.len(), 3);
        assert!(d
            .indexes
            .iter()
            .any(|i| i.name == "b_label" && i.columns == ["label"] && !i.unique));
        assert_eq!(d.foreign_keys.len(), 1);
        assert_eq!(d.foreign_keys[0].ref_table, "a");
        assert_eq!(d.foreign_keys[0].columns, ["a_id"]);
        let da = table_details(&conn, &cache, "a").await.unwrap();
        assert!(da.indexes.iter().any(|i| i.unique && i.columns == ["code"]));
    }

    #[test]
    fn modification_table_name() {
        let m = SchemaModification::DropColumn {
            table_name: "t".into(),
            column_name: "c".into(),
        };
        assert_eq!(table_of(&m), Some("t"));
    }
}
