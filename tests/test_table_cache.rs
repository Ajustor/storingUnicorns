use std::time::Duration;

use storing_unicorns::models::Column;
use storing_unicorns::services::table_cache::{CachedTableInfo, TableCache};

// ========== CachedTableInfo Tests ==========

#[test]
fn cached_table_info_new() {
    let cols = vec![Column {
        name: "id".into(),
        type_name: "integer".into(),
        nullable: false,
        is_primary_key: true,
    }];
    let info = CachedTableInfo::new(cols.clone());
    assert_eq!(info.column_details.len(), 1);
    assert_eq!(info.column_details[0].name, "id");
}

#[test]
fn cached_table_info_is_valid_fresh() {
    let info = CachedTableInfo::new(vec![]);
    // Freshly created, should be valid for 5 minutes
    assert!(info.is_valid(Duration::from_secs(300)));
}

#[test]
fn cached_table_info_is_valid_expired() {
    let info = CachedTableInfo::new(vec![]);
    // 0 duration => immediately expired
    assert!(!info.is_valid(Duration::from_secs(0)));
}

// ========== TableCache Async Tests ==========

#[tokio::test]
async fn table_cache_set_and_get() {
    let cache = TableCache::new(Duration::from_secs(300));
    let cols = vec![
        Column {
            name: "id".into(),
            type_name: "int".into(),
            nullable: false,
            is_primary_key: true,
        },
        Column {
            name: "name".into(),
            type_name: "varchar".into(),
            nullable: true,
            is_primary_key: false,
        },
    ];

    cache.set("users".into(), cols.clone()).await;
    let result = cache.get_column_details("users").await;
    assert!(result.is_some());
    let fetched = result.unwrap();
    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0].name, "id");
    assert_eq!(fetched[1].name, "name");
}

#[tokio::test]
async fn table_cache_get_nonexistent() {
    let cache = TableCache::new(Duration::from_secs(300));
    let result = cache.get_column_details("nonexistent").await;
    assert!(result.is_none());
}

#[tokio::test]
async fn table_cache_invalidate() {
    let cache = TableCache::new(Duration::from_secs(300));
    let cols = vec![Column {
        name: "x".into(),
        type_name: "int".into(),
        nullable: false,
        is_primary_key: false,
    }];
    cache.set("tbl".into(), cols).await;
    assert!(cache.get_column_details("tbl").await.is_some());

    cache.invalidate("tbl").await;
    assert!(cache.get_column_details("tbl").await.is_none());
}

#[tokio::test]
async fn table_cache_clear() {
    let cache = TableCache::new(Duration::from_secs(300));
    cache.set("a".into(), vec![]).await;
    cache.set("b".into(), vec![]).await;

    cache.clear().await;
    assert!(cache.get_column_details("a").await.is_none());
    assert!(cache.get_column_details("b").await.is_none());
}

#[tokio::test]
async fn table_cache_expired_entry_returns_none() {
    // Create cache with 0 TTL => entries expire immediately
    let cache = TableCache::new(Duration::from_secs(0));
    let cols = vec![Column {
        name: "id".into(),
        type_name: "int".into(),
        nullable: false,
        is_primary_key: true,
    }];

    cache.set("users".into(), cols).await;
    // Entry should be expired immediately
    let result = cache.get_column_details("users").await;
    assert!(result.is_none());
}

#[tokio::test]
async fn table_cache_overwrite() {
    let cache = TableCache::new(Duration::from_secs(300));
    let cols1 = vec![Column {
        name: "old".into(),
        type_name: "int".into(),
        nullable: false,
        is_primary_key: false,
    }];
    let cols2 = vec![
        Column {
            name: "new1".into(),
            type_name: "text".into(),
            nullable: true,
            is_primary_key: false,
        },
        Column {
            name: "new2".into(),
            type_name: "bool".into(),
            nullable: true,
            is_primary_key: false,
        },
    ];

    cache.set("tbl".into(), cols1).await;
    cache.set("tbl".into(), cols2).await;

    let result = cache.get_column_details("tbl").await.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].name, "new1");
}

#[tokio::test]
async fn table_cache_default_ttl() {
    let cache = TableCache::default();
    let cols = vec![Column {
        name: "col".into(),
        type_name: "text".into(),
        nullable: true,
        is_primary_key: false,
    }];
    cache.set("test".into(), cols).await;
    // Default TTL is 5 min, so fresh entry should be valid
    assert!(cache.get_column_details("test").await.is_some());
}

// ========== FetchQueue Tests ==========

use storing_unicorns::services::table_cache::FetchQueue;

#[tokio::test]
async fn fetch_queue_start_fetch() {
    let queue = FetchQueue::new(Duration::from_secs(10));
    let started = queue.start_fetch("users".into()).await;
    assert!(started);

    // Second call should return false (already fetching)
    let started2 = queue.start_fetch("users".into()).await;
    assert!(!started2);
}

#[tokio::test]
async fn fetch_queue_is_fetching() {
    let queue = FetchQueue::new(Duration::from_secs(10));
    assert!(!queue.is_fetching("users").await);

    queue.start_fetch("users".into()).await;
    assert!(queue.is_fetching("users").await);
}

#[tokio::test]
async fn fetch_queue_complete_fetch() {
    let queue = FetchQueue::new(Duration::from_secs(10));
    queue.start_fetch("users".into()).await;
    assert!(queue.is_fetching("users").await);

    queue.complete_fetch("users").await;
    assert!(!queue.is_fetching("users").await);
}

#[tokio::test]
async fn fetch_queue_cleanup_timed_out() {
    let queue = FetchQueue::new(Duration::from_secs(0)); // 0s timeout
    queue.start_fetch("users".into()).await;

    // With 0 timeout, is_fetching should return false (timed out)
    assert!(!queue.is_fetching("users").await);

    queue.cleanup().await;
    // After cleanup, start_fetch should succeed again
    let started = queue.start_fetch("users".into()).await;
    assert!(started);
}

#[tokio::test]
async fn fetch_queue_default() {
    let queue = FetchQueue::default();
    // Default timeout is 10s
    let started = queue.start_fetch("test".into()).await;
    assert!(started);
    assert!(queue.is_fetching("test").await);
}
