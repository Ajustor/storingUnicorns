use anyhow::{bail, Result};

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{Column, DatabaseType, TableDetails};
use crate::engine::ops::transfer::qualified;
use crate::engine::services::{
    table_cache::TableCache, ColumnDefinition, SchemaModification, SchemaService,
};
use crate::engine::sql::statements::quote_chars;

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

/// `table` (optionally `schema.table`, possibly already quoted) quoted with
/// `quotes`, each part separately.
fn quote_table(table: &str, (q0, q1): (char, char)) -> String {
    let unquote = |s: &str| s.trim_matches(|c| c == q0 || c == q1).to_string();
    match table.split_once('.') {
        Some((schema, name)) => qualified(&unquote(schema), &unquote(name), (q0, q1)),
        None => format!("{q0}{}{q1}", unquote(table)),
    }
}

/// `CREATE TABLE` (+ `CREATE INDEX`) for `table`: native for SQLite
/// (`sqlite_master.sql`) and MySQL (`SHOW CREATE TABLE`), generated from
/// metadata for Postgres and SQL Server.
#[allow(dead_code)] // used by the GUI (plan 3b)
pub async fn table_ddl(
    conn: &DatabaseConnection,
    cache: &TableCache,
    db: &DatabaseType,
    table: &str,
) -> Result<String> {
    match db {
        DatabaseType::SQLite => {
            // Same unquoting as the SQLite connector's metadata queries.
            let bare = table
                .trim_matches('"')
                .replace("main.", "")
                .replace('\'', "''");
            let r = conn
                .execute_query(&format!(
                    "SELECT sql FROM sqlite_master \
                     WHERE tbl_name = '{bare}' AND type IN ('table', 'index') \
                     AND sql IS NOT NULL \
                     ORDER BY type = 'table' DESC, name"
                ))
                .await?;
            if r.rows.is_empty() {
                bail!("table {table} not found");
            }
            let statements: Vec<&str> = r.rows.iter().map(|row| row[0].as_str()).collect();
            Ok(format!("{};", statements.join(";\n\n")))
        }
        DatabaseType::MySQL => {
            let r = conn
                .execute_query(&format!("SHOW CREATE TABLE {table}"))
                .await?;
            match r.rows.first().and_then(|row| row.get(1)) {
                Some(ddl) => Ok(format!("{ddl};")),
                None => bail!("table {table} not found"),
            }
        }
        DatabaseType::Postgres | DatabaseType::SQLServer | DatabaseType::Azure => {
            let details = table_details(conn, cache, table).await?;
            if details.columns.is_empty() {
                bail!("table {table} not found");
            }
            let quotes = quote_chars(db);
            Ok(generate_ddl(&quote_table(table, quotes), &details, quotes))
        }
    }
}

/// Pure DDL generator used for Postgres / SQL Server. `table` is used as
/// given (already quoted/qualified). Statements are separated by `;\n\n`;
/// the primary-key index is part of the table body, not a `CREATE INDEX`.
#[allow(dead_code)] // used by the GUI (plan 3b)
pub fn generate_ddl(table: &str, details: &TableDetails, quotes: (char, char)) -> String {
    let (q0, q1) = quotes;
    let quote = |name: &str| format!("{q0}{name}{q1}");
    let quote_list = |names: &[String]| {
        names
            .iter()
            .map(|n| quote(n))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut body: Vec<String> = details
        .columns
        .iter()
        .map(|c| {
            let mut line = format!("  {} {}", quote(&c.name), c.type_name);
            if !c.nullable {
                line.push_str(" NOT NULL");
            }
            line
        })
        .collect();

    // Key order from the primary index when known, else column order.
    let pk: Vec<String> = match details.indexes.iter().find(|i| i.primary) {
        Some(index) => index.columns.clone(),
        None => details
            .columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.clone())
            .collect(),
    };
    if !pk.is_empty() {
        body.push(format!("  PRIMARY KEY ({})", quote_list(&pk)));
    }

    for fk in &details.foreign_keys {
        body.push(format!(
            "  CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
            quote(&fk.name),
            quote_list(&fk.columns),
            quote_table(&fk.ref_table, quotes),
            quote_list(&fk.ref_columns),
        ));
    }

    let mut statements = vec![format!("CREATE TABLE {table} (\n{}\n)", body.join(",\n"))];
    for index in details.indexes.iter().filter(|i| !i.primary) {
        statements.push(format!(
            "CREATE {}INDEX {} ON {table} ({})",
            if index.unique { "UNIQUE " } else { "" },
            quote(&index.name),
            quote_list(&index.columns),
        ));
    }
    format!("{};", statements.join(";\n\n"))
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
    fn generate_ddl_includes_columns_pk_fk_and_indexes() {
        use crate::engine::models::{Column, ForeignKeyInfo, IndexInfo};
        let col = |n: &str, t: &str, null: bool, pk: bool| Column {
            name: n.into(),
            type_name: t.into(),
            nullable: null,
            is_primary_key: pk,
        };
        let d = TableDetails {
            columns: vec![
                col("id", "integer", false, true),
                col("a_id", "integer", true, false),
            ],
            indexes: vec![IndexInfo {
                name: "ix_a".into(),
                columns: vec!["a_id".into()],
                unique: false,
                primary: false,
            }],
            foreign_keys: vec![ForeignKeyInfo {
                name: "fk_a".into(),
                columns: vec!["a_id".into()],
                ref_table: "a".into(),
                ref_columns: vec!["id".into()],
            }],
        };
        let ddl = generate_ddl("\"b\"", &d, ('"', '"'));
        assert!(ddl.starts_with("CREATE TABLE \"b\" (\n"));
        assert!(ddl.contains("  \"id\" integer NOT NULL"));
        assert!(ddl.contains("  \"a_id\" integer,") || ddl.contains("  \"a_id\" integer\n"));
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
        assert!(
            ddl.contains("CONSTRAINT \"fk_a\" FOREIGN KEY (\"a_id\") REFERENCES \"a\" (\"id\")")
        );
        assert!(ddl.contains("CREATE INDEX \"ix_a\" ON \"b\" (\"a_id\");"));
    }

    #[tokio::test]
    async fn sqlite_ddl_is_native() {
        let conn = sqlite_mem(&[
            "CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT)",
            "CREATE INDEX t_x ON t(x)",
        ])
        .await;
        let ddl = table_ddl(&conn, &TableCache::default(), &DatabaseType::SQLite, "t")
            .await
            .unwrap();
        assert!(ddl.contains("CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT)"));
        assert!(ddl.contains("CREATE INDEX t_x ON t(x)"));
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
