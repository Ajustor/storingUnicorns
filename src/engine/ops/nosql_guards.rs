//! MongoDB / Redis connections never run SQL: every SQL operation refuses
//! them with `not_sql` (plan 13, « Garde-fous »). The connections used here
//! never reach a server.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{Column, ConnectionConfig, DatabaseType};
use crate::engine::ops::rows::RowChanges;
use crate::engine::ops::test_support::{mongo_offline, redis_offline};
use crate::engine::ops::{query, rows, schema, transfer, tx};
use crate::engine::services::export_import::ExportFormat;
use crate::engine::services::table_cache::TableCache;
use crate::engine::services::SchemaModification;

const NOT_SQL: &str = "Opération SQL indisponible pour";

async fn both() -> [DatabaseConnection; 2] {
    [mongo_offline().await, redis_offline()]
}

fn assert_not_sql(e: impl std::fmt::Display, conn: &DatabaseConnection) {
    let text = e.to_string();
    assert!(
        text.contains(NOT_SQL) && text.contains(&conn.db_type().to_string()),
        "{}: {text}",
        conn.db_type()
    );
}

fn columns() -> Vec<Column> {
    vec![Column {
        name: "id".into(),
        type_name: "int".into(),
        nullable: false,
        is_primary_key: true,
    }]
}

#[tokio::test]
async fn connector_refuses_sql_execution() {
    for conn in both().await {
        assert_not_sql(conn.execute_query("SELECT 1").await.unwrap_err(), &conn);
        assert_not_sql(
            conn.execute_query_limited("SELECT 1", Some(1))
                .await
                .unwrap_err(),
            &conn,
        );
        assert_not_sql(
            conn.execute_transaction(&["SELECT 1".into()])
                .await
                .unwrap_err(),
            &conn,
        );
        assert_not_sql(
            conn.execute_checked_batch(&["DELETE FROM t".into()], &[Some(1)])
                .await
                .unwrap_err(),
            &conn,
        );
        assert!(conn
            .execute_tsql_batch("SELECT 1", None, false)
            .await
            .is_err());
        assert!(!conn.rollback_open_transaction().await.unwrap());
        assert!(!conn.runs_tsql_batches());
    }
}

#[tokio::test]
async fn connector_clients_by_engine() {
    let mongo = mongo_offline().await;
    let redis = redis_offline();
    assert_eq!(mongo.db_type(), DatabaseType::MongoDB);
    assert_eq!(redis.db_type(), DatabaseType::Redis);
    assert!(mongo.mongo().is_ok() && mongo.redis().is_err());
    assert!(redis.redis().is_ok() && redis.mongo().is_err());
    let sqlite = crate::engine::ops::test_support::sqlite_mem(&[]).await;
    assert!(sqlite.mongo().is_err() && sqlite.redis().is_err());
}

#[tokio::test]
async fn connector_metadata_of_nosql() {
    let mongo = mongo_offline().await;
    assert_eq!(
        mongo.get_primary_keys(r#""shop"."users""#).await.unwrap(),
        ["_id"]
    );
    assert!(mongo.get_primary_keys("users").await.is_err()); // no database
    assert!(mongo
        .get_foreign_keys("shop.users")
        .await
        .unwrap()
        .is_empty());
    assert!(mongo
        .get_incoming_foreign_keys("shop.users")
        .await
        .unwrap()
        .is_empty());
    let redis = redis_offline();
    assert!(redis.get_table_column_details("db0.x").await.is_err());
    assert!(redis.get_indexes("db0.x").await.is_err());
    assert!(redis.get_primary_keys("db0.x").await.is_err());
}

#[tokio::test]
async fn no_dedicated_session_for_nosql() {
    for db_type in [DatabaseType::MongoDB, DatabaseType::Redis] {
        let config = ConnectionConfig {
            db_type: db_type.clone(),
            ..Default::default()
        };
        let e = DatabaseConnection::connect_dedicated(&config, Arc::new(AtomicU64::new(0)))
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("transaction manuel"), "{e}");
    }
}

#[tokio::test]
async fn query_ops_refuse_nosql() {
    for conn in both().await {
        assert_not_sql(
            query::run_query(&conn, "SELECT 1").await.unwrap_err(),
            &conn,
        );
        assert_not_sql(
            query::run_table_page(&conn, &conn, "SELECT 1", "t")
                .await
                .unwrap_err(),
            &conn,
        );
        assert_not_sql(
            query::run_at_cursor(&conn, "SELECT 1", 0)
                .await
                .unwrap_err(),
            &conn,
        );
        let outcomes = query::run_at_cursor_outcomes(&conn, "SELECT 1", 0, None).await;
        assert_eq!(outcomes.len(), 1);
        assert_not_sql(outcomes[0].result.as_ref().unwrap_err(), &conn);
        let outcomes = query::run_script(&conn, "SELECT 1; SELECT 2", None).await;
        assert_eq!(outcomes.len(), 1);
        assert_not_sql(outcomes[0].result.as_ref().unwrap_err(), &conn);
        assert_not_sql(query::run_all(&conn, "SELECT 1").await.unwrap_err(), &conn);
    }
}

#[tokio::test]
async fn row_ops_refuse_nosql() {
    let cols = columns();
    for conn in both().await {
        let db = conn.db_type();
        // Unchanged row: still refused, before anything else.
        assert_not_sql(
            rows::update_row(&conn, "t", &cols, &["1".into()], &["1".into()])
                .await
                .unwrap_err(),
            &conn,
        );
        assert_not_sql(
            rows::insert_row(&conn, "t", &cols, &[Some("1".into())])
                .await
                .unwrap_err(),
            &conn,
        );
        assert_not_sql(
            rows::delete_row(&conn, "t", &cols, &["1".into()])
                .await
                .unwrap_err(),
            &conn,
        );
        let changes = RowChanges {
            deletes: vec![vec!["1".into()]],
            ..Default::default()
        };
        assert_not_sql(
            rows::submit_changes(&conn, &db, "t", &cols, &changes)
                .await
                .unwrap_err(),
            &conn,
        );
        let report = rows::truncate_tables(&conn, &["t".into()], |_, _, _| {}).await;
        assert_eq!(report.succeeded, 0);
        assert_not_sql(&report.errors[0], &conn);
    }
}

#[tokio::test]
async fn schema_ops_refuse_nosql() {
    let cache = TableCache::new(Duration::from_secs(60));
    for conn in both().await {
        let db = conn.db_type();
        assert_not_sql(
            schema::table_ddl(&conn, &cache, &db, "shop.users")
                .await
                .unwrap_err(),
            &conn,
        );
        let m = SchemaModification::DropTable {
            table_name: "t".into(),
        };
        assert_not_sql(
            schema::apply_modification(&conn, &cache, &m, &db)
                .await
                .unwrap_err(),
            &conn,
        );
    }
}

#[tokio::test]
async fn transfer_ops_refuse_nosql() {
    let dir = tempfile::tempdir().unwrap();
    let tables = vec![("s".to_string(), "t".to_string())];
    for conn in both().await {
        assert_not_sql(
            transfer::import_csv(&conn, "t", "id\n1\n", ('"', '"'), |_, _| {})
                .await
                .unwrap_err(),
            &conn,
        );
        let report = transfer::export_tables(
            &conn,
            &tables,
            dir.path(),
            ExportFormat::Csv,
            ('"', '"'),
            |_, _, _| {},
        )
        .await;
        assert_not_sql(&report.errors[0], &conn);
        let report =
            transfer::import_tables(&conn, &tables, dir.path(), ('"', '"'), |_, _, _| {}).await;
        assert_not_sql(&report.errors[0], &conn);
    }
}

#[tokio::test]
async fn manual_transactions_refuse_nosql() {
    for conn in both().await {
        let config = ConnectionConfig {
            db_type: conn.db_type(),
            ..Default::default()
        };
        let e = tx::ManualTx::open(&config, Arc::new(conn))
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("transaction manuel"), "{e}");
    }
}
