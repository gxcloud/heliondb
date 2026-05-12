use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::table::{RowVersion, Table};
use crate::storage::types::{ColumnMeta, Row};

/// Metadata for a table, stored in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMeta {
    pub name: String,
    pub engine: String,
    pub created_at: u64,
}

impl TableMeta {
    pub fn new(name: &str, engine: &str) -> Self {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        TableMeta {
            name: name.to_string(),
            engine: normalize_engine_name(engine),
            created_at,
        }
    }
}

pub fn normalize_engine_name(engine: &str) -> String {
    engine.trim().to_ascii_lowercase()
}

#[async_trait]
pub trait StorageEngine: Send + Sync {
    fn name(&self) -> &str;

    /// Create a new table managed by this engine.
    async fn create_table(&self, meta: &TableMeta, columns: Vec<ColumnMeta>) -> Result<()>;

    /// Drop a table and all its data.
    async fn drop_table(&self, name: &str) -> Result<()>;

    /// Check if a table exists in this engine.
    async fn table_exists(&self, name: &str) -> Result<bool>;

    /// Get table metadata + schema.
    async fn get_table(&self, name: &str) -> Result<Table>;

    /// Scan visible rows for a snapshot.
    async fn scan_visible(
        &self,
        table: &str,
        snapshot_txid: u64,
        active_txns: &BTreeSet<u64>,
    ) -> Result<Vec<(usize, Row)>>;

    /// Get the number of version chains (logical rows) in a table.
    async fn row_count(&self, table: &str) -> Result<usize>;

    /// Atomically apply a batch of version chain changes (called by MVCC commit).
    async fn apply_write_set(
        &self,
        table: &str,
        changes: Vec<(usize, RowVersion)>,
    ) -> Result<()>;

    /// Take a full snapshot of a table (all data, all version chains).
    async fn snapshot_table(&self, table: &str) -> Result<Table>;

    /// Restore a table from a snapshot (overwrites existing data).
    async fn restore_table(&self, table: Table) -> Result<()>;

    /// Flush any pending writes to durable storage.
    async fn flush(&self) -> Result<()>;

    /// Close the engine, releasing resources.
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
