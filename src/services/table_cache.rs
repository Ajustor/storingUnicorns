use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::models::Column;

/// Cached table information for autocompletion
#[derive(Debug, Clone)]
pub struct CachedTableInfo {
    /// Full column metadata
    pub column_details: Vec<Column>,
    /// When this cache entry was last updated
    pub last_updated: Instant,
}

impl CachedTableInfo {
    pub fn new(column_details: Vec<Column>) -> Self {
        Self {
            column_details,
            last_updated: Instant::now(),
        }
    }

    /// Check if this cache entry is still valid
    pub fn is_valid(&self, max_age: Duration) -> bool {
        self.last_updated.elapsed() < max_age
    }
}

/// Thread-safe cache for table column information
/// Used for SQL autocompletion and schema modification
#[derive(Debug, Clone)]
pub struct TableCache {
    /// Map of table_name -> cached info
    cache: Arc<RwLock<HashMap<String, CachedTableInfo>>>,
    /// Maximum age before cache entries are considered stale
    max_age: Duration,
}

impl Default for TableCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(300)) // 5 minutes default
    }
}

impl TableCache {
    pub fn new(max_age: Duration) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            max_age,
        }
    }

    /// Get cached column details for a table
    pub async fn get_column_details(&self, table_name: &str) -> Option<Vec<Column>> {
        let cache = self.cache.read().await;
        cache.get(table_name).and_then(|info| {
            if info.is_valid(self.max_age) {
                Some(info.column_details.clone())
            } else {
                None
            }
        })
    }

    /// Store column information in the cache
    pub async fn set(&self, table_name: String, column_details: Vec<Column>) {
        let mut cache = self.cache.write().await;
        cache.insert(table_name, CachedTableInfo::new(column_details));
    }

    /// Remove a table from the cache
    pub async fn invalidate(&self, table_name: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(table_name);
    }

    /// Clear the entire cache
    #[allow(dead_code)]
    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }
}

/// Represents a pending fetch request for table columns
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PendingFetch {
    pub table_name: String,
    pub requested_at: Instant,
}

/// Manages async fetching of table metadata
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FetchQueue {
    /// Tables currently being fetched
    in_progress: Arc<RwLock<HashMap<String, Instant>>>,
    /// Maximum time to wait for a fetch
    timeout: Duration,
}

impl Default for FetchQueue {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

impl FetchQueue {
    #[allow(dead_code)]
    pub fn new(timeout: Duration) -> Self {
        Self {
            in_progress: Arc::new(RwLock::new(HashMap::new())),
            timeout,
        }
    }

    /// Check if a table fetch is already in progress
    #[allow(dead_code)]
    pub async fn is_fetching(&self, table_name: &str) -> bool {
        let in_progress = self.in_progress.read().await;
        if let Some(started) = in_progress.get(table_name) {
            // Check if it hasn't timed out
            started.elapsed() < self.timeout
        } else {
            false
        }
    }

    /// Mark a table as being fetched
    #[allow(dead_code)]
    pub async fn start_fetch(&self, table_name: String) -> bool {
        let mut in_progress = self.in_progress.write().await;

        // Check if already fetching
        if let Some(started) = in_progress.get(&table_name) {
            if started.elapsed() < self.timeout {
                return false; // Already fetching
            }
        }

        in_progress.insert(table_name, Instant::now());
        true
    }

    /// Mark a fetch as complete
    #[allow(dead_code)]
    pub async fn complete_fetch(&self, table_name: &str) {
        let mut in_progress = self.in_progress.write().await;
        in_progress.remove(table_name);
    }

    /// Clean up timed-out fetches
    #[allow(dead_code)]
    pub async fn cleanup(&self) {
        let mut in_progress = self.in_progress.write().await;
        in_progress.retain(|_, started| started.elapsed() < self.timeout);
    }
}
