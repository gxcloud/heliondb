use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::error::Result;
use crate::storage::engine_trait::{StorageEngine, TableMeta};
use crate::storage::table::{RowVersion, Table};
use crate::storage::types::{ColumnMeta, Row};

/// In-memory storage engine. All data lives in RAM.
/// Data is lost on restart unless preserved via WAL replay.
pub struct MemoryEngine {
    tables: Arc<RwLock<HashMap<String, Table>>>,
}

impl MemoryEngine {
    pub fn new() -> Self {
        MemoryEngine {
            tables: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl StorageEngine for MemoryEngine {
    fn name(&self) -> &str {
        "Memory"
    }

    async fn create_table(&self, _meta: &TableMeta, columns: Vec<ColumnMeta>) -> Result<()> {
        let mut tables = self.tables.write();
        let table = Table::new(&_meta.name, columns);
        tables.insert(_meta.name.clone(), table);
        Ok(())
    }

    async fn drop_table(&self, name: &str) -> Result<()> {
        let mut tables = self.tables.write();
        tables.remove(name);
        Ok(())
    }

    async fn table_exists(&self, name: &str) -> Result<bool> {
        let tables = self.tables.read();
        Ok(tables.contains_key(name))
    }

    async fn get_table(&self, name: &str) -> Result<Table> {
        let tables = self.tables.read();
        tables.get(name).cloned().ok_or_else(|| {
            crate::error::HelionError::TableNotFound(name.to_string())
        })
    }

    async fn scan_visible(
        &self,
        table: &str,
        snapshot_txid: u64,
        active_txns: &BTreeSet<u64>,
    ) -> Result<Vec<(usize, Row)>> {
        let tables = self.tables.read();
        let t = tables.get(table).ok_or_else(|| {
            crate::error::HelionError::TableNotFound(table.to_string())
        })?;
        Ok(t.scan_visible(snapshot_txid, active_txns)
            .into_iter()
            .map(|(idx, row)| (idx, row.clone()))
            .collect())
    }

    async fn row_count(&self, table: &str) -> Result<usize> {
        let tables = self.tables.read();
        tables.get(table).map(|t| t.row_count())
            .ok_or_else(|| crate::error::HelionError::TableNotFound(table.to_string()))
    }

    async fn apply_write_set(
        &self,
        table: &str,
        changes: Vec<(usize, RowVersion)>,
    ) -> Result<()> {
        let mut tables = self.tables.write();
        let t = tables.get_mut(table).ok_or_else(|| {
            crate::error::HelionError::TableNotFound(table.to_string())
        })?;

        for (row_idx, version) in changes {
            if row_idx < t.version_chains.len() {
                if let Some(old) = t.version_chains[row_idx].last_mut() {
                    old.txid_max = version.txid_min;
                }
                t.version_chains[row_idx].push(version);
            } else {
                t.version_chains.push(vec![version]);
            }
        }
        Ok(())
    }

    async fn snapshot_table(&self, table: &str) -> Result<Table> {
        let tables = self.tables.read();
        tables.get(table).cloned()
            .ok_or_else(|| crate::error::HelionError::TableNotFound(table.to_string()))
    }

    async fn restore_table(&self, table: Table) -> Result<()> {
        let mut tables = self.tables.write();
        tables.insert(table.name.clone(), table);
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // Memory engine doesn't need to flush
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::*;

    fn setup_engine() -> MemoryEngine {
        MemoryEngine::new()
    }

    fn table_meta(name: &str, engine: &str) -> TableMeta {
        TableMeta::new(name, engine)
    }

    #[tokio::test]
    async fn test_create_table() {
        let engine = setup_engine();
        let columns = vec![ColumnMeta::new("id", DataType::Integer).primary_key()];
        engine.create_table(&table_meta("users", "Memory"), columns).await.unwrap();
        assert!(engine.table_exists("users").await.unwrap());
    }

    #[tokio::test]
    async fn test_drop_table() {
        let engine = setup_engine();
        let columns = vec![ColumnMeta::new("id", DataType::Integer)];
        engine.create_table(&table_meta("t", "Memory"), columns).await.unwrap();
        assert!(engine.table_exists("t").await.unwrap());
        engine.drop_table("t").await.unwrap();
        assert!(!engine.table_exists("t").await.unwrap());
    }

    #[tokio::test]
    async fn test_insert_and_scan() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("t", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer)],
        ).await.unwrap();

        let changes = vec![(0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(42)])))];
        engine.apply_write_set("t", changes).await.unwrap();

        let active = BTreeSet::new();
        let rows = engine.scan_visible("t", 100, &active).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.get(0), Some(&Datum::Integer(42)));
    }

    #[tokio::test]
    async fn test_scan_visible_filters_uncommitted() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("t", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer)],
        ).await.unwrap();

        // Insert from tx 5 which is still active
        let changes = vec![(0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(42)])))];
        engine.apply_write_set("t", changes).await.unwrap();

        let mut active = BTreeSet::new();
        active.insert(5);  // tx 5 is still active

        // Should not be visible because tx 5 hasn't committed
        let rows = engine.scan_visible("t", 100, &active).await.unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[tokio::test]
    async fn test_update_and_scan() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("t", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer), ColumnMeta::new("val", DataType::Integer)],
        ).await.unwrap();

        // Insert
        engine.apply_write_set("t", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Integer(100)])))
        ]).await.unwrap();

        // Update
        engine.apply_write_set("t", vec![
            (0usize, RowVersion::new_update(10, Row::new(vec![Datum::Integer(1), Datum::Integer(999)])))
        ]).await.unwrap();

        let active = BTreeSet::new();
        let rows = engine.scan_visible("t", 100, &active).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.get(1), Some(&Datum::Integer(999)));
    }

    #[tokio::test]
    async fn test_delete_tombstone() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("t", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer)],
        ).await.unwrap();

        engine.apply_write_set("t", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(42)])))
        ]).await.unwrap();

        engine.apply_write_set("t", vec![
            (0usize, RowVersion::new_delete(10, Row::new(vec![Datum::Integer(42)])))
        ]).await.unwrap();

        let active = BTreeSet::new();
        let rows = engine.scan_visible("t", 100, &active).await.unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[tokio::test]
    async fn test_snapshot_restore_roundtrip() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("t", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer)],
        ).await.unwrap();

        engine.apply_write_set("t", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(42)])))
        ]).await.unwrap();

        let snapshot = engine.snapshot_table("t").await.unwrap();
        assert_eq!(snapshot.row_count(), 1);
        assert_eq!(snapshot.columns[0].name, "id");

        engine.drop_table("t").await.unwrap();
        assert!(!engine.table_exists("t").await.unwrap());

        engine.restore_table(snapshot).await.unwrap();
        assert!(engine.table_exists("t").await.unwrap());
        assert_eq!(engine.row_count("t").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_row_count() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("t", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer)],
        ).await.unwrap();

        assert_eq!(engine.row_count("t").await.unwrap(), 0);
        engine.apply_write_set("t", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1)])))
        ]).await.unwrap();
        assert_eq!(engine.row_count("t").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_table_not_found() {
        let engine = setup_engine();
        let result = engine.get_table("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_multiple_tables() {
        let engine = setup_engine();
        engine.create_table(
            &table_meta("a", "Memory"),
            vec![ColumnMeta::new("id", DataType::Integer)],
        ).await.unwrap();
        engine.create_table(
            &table_meta("b", "Memory"),
            vec![ColumnMeta::new("name", DataType::Text)],
        ).await.unwrap();

        assert!(engine.table_exists("a").await.unwrap());
        assert!(engine.table_exists("b").await.unwrap());

        engine.apply_write_set("a", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1)])))
        ]).await.unwrap();
        engine.apply_write_set("b", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Text("hello".into())])))
        ]).await.unwrap();

        assert_eq!(engine.row_count("a").await.unwrap(), 1);
        assert_eq!(engine.row_count("b").await.unwrap(), 1);
    }
}
