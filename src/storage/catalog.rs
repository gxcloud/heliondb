use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{HelionError, Result};
use crate::storage::engine_trait::{StorageEngine, TableMeta};
use crate::storage::engines::memory::MemoryEngine;
use crate::storage::table::{RowVersion, Table};
use crate::storage::types::{ColumnMeta, Row};

const CATALOG_FILE: &str = "helion.catalog";

/// Serialized form of the catalog for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogData {
    default_engine: String,
    tables: Vec<TableMeta>,
}

/// The catalog manages table metadata and routes operations to the correct
/// storage engine for each table.
pub struct Catalog {
    /// Table metadata (name → metadata).
    metadata: RwLock<HashMap<String, TableMeta>>,
    /// Registered storage engines.
    engines: HashMap<String, Arc<dyn StorageEngine>>,
    /// The default engine for new tables.
    default_engine: RwLock<String>,
    /// Path to the catalog file.
    catalog_path: PathBuf,
}

impl Catalog {
    /// Create a new catalog and register default engines.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        let catalog_path = data_dir.join(CATALOG_FILE);
        let mut engines: HashMap<String, Arc<dyn StorageEngine>> = HashMap::new();

        // Register built-in engines
        engines.insert("Memory".to_string(), Arc::new(MemoryEngine::new()));

        let (metadata, default_engine) = if catalog_path.exists() {
            let bytes = tokio::fs::read(&catalog_path).await?;
            let data: CatalogData = bincode::deserialize(&bytes)
                .map_err(|e| HelionError::Serialization(e.to_string()))?;
            let meta: HashMap<String, TableMeta> = data.tables.into_iter()
                .map(|m| (m.name.clone(), m))
                .collect();
            (meta, data.default_engine)
        } else {
            (HashMap::new(), "Memory".to_string())
        };

        Ok(Catalog {
            metadata: RwLock::new(metadata),
            engines,
            default_engine: RwLock::new(default_engine),
            catalog_path,
        })
    }

    pub fn default_engine(&self) -> String {
        self.default_engine.read().clone()
    }

    pub fn set_default_engine(&self, engine: &str) -> Result<()> {
        if !self.engines.contains_key(engine) {
            return Err(HelionError::Internal(format!("Unknown engine: {}", engine)));
        }
        *self.default_engine.write() = engine.to_string();
        Ok(())
    }

    pub fn register_engine(&mut self, name: &str, engine: Arc<dyn StorageEngine>) {
        self.engines.insert(name.to_string(), engine);
    }

    pub fn get_engine(&self, name: &str) -> Result<&Arc<dyn StorageEngine>> {
        self.engines.get(name).ok_or_else(|| {
            HelionError::Internal(format!("Unknown engine: {}", name))
        })
    }

    pub fn get_table_engine(&self, table_name: &str) -> Result<&Arc<dyn StorageEngine>> {
        let meta = self.metadata.read();
        let table_meta = meta.get(table_name).ok_or_else(|| {
            HelionError::TableNotFound(table_name.to_string())
        })?;
        let engine_name = &table_meta.engine;
        self.engines.get(engine_name).ok_or_else(|| {
            HelionError::Internal(format!("Engine '{}' not found for table '{}'", engine_name, table_name))
        })
    }

    pub async fn table_exists(&self, name: &str) -> Result<bool> {
        if self.metadata.read().contains_key(name) {
            return Ok(true);
        }
        // Also check in engines directly
        for engine in self.engines.values() {
            if engine.table_exists(name).await.unwrap_or(false) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Create a table with the specified engine (or default if not specified).
    pub async fn create_table(
        &self,
        name: &str,
        columns: Vec<ColumnMeta>,
        engine_name: Option<&str>,
    ) -> Result<()> {
        let engine_name = match engine_name {
            Some(e) => e.to_string(),
            None => self.default_engine.read().clone(),
        };
        let engine = self.get_engine(&engine_name)?;
        let meta = TableMeta::new(name, &engine_name);

        engine.create_table(&meta, columns).await?;

        {
            let mut metadata = self.metadata.write();
            metadata.insert(name.to_string(), meta);
        }
        self.save().await?;
        Ok(())
    }

    /// Drop a table from its engine.
    pub async fn drop_table(&self, name: &str) -> Result<()> {
        let engine = self.get_table_engine(name)?;
        engine.drop_table(name).await?;

        {
            let mut metadata = self.metadata.write();
            metadata.remove(name);
        }
        self.save().await?;
        Ok(())
    }

    /// Get table schema from its engine.
    pub async fn get_table(&self, name: &str) -> Result<Table> {
        let engine = self.get_table_engine(name)?;
        engine.get_table(name).await
    }

    /// Scan visible rows from the correct engine.
    pub async fn scan_visible(
        &self,
        table: &str,
        snapshot_txid: u64,
        active_txns: &BTreeSet<u64>,
    ) -> Result<Vec<(usize, Row)>> {
        let engine = self.get_table_engine(table)?;
        engine.scan_visible(table, snapshot_txid, active_txns).await
    }

    /// Get row count from the correct engine.
    pub async fn row_count(&self, table: &str) -> Result<usize> {
        let engine = self.get_table_engine(table)?;
        engine.row_count(table).await
    }

    /// Apply a write set to the correct engine.
    pub async fn apply_write_set(
        &self,
        table: &str,
        changes: Vec<(usize, RowVersion)>,
    ) -> Result<()> {
        let engine = self.get_table_engine(table)?;
        engine.apply_write_set(table, changes).await
    }

    /// Take a snapshot of a table from its engine.
    pub async fn snapshot_table(&self, table: &str) -> Result<Table> {
        let engine = self.get_table_engine(table)?;
        engine.snapshot_table(table).await
    }

    /// Restore a table to its engine.
    pub async fn restore_table(&self, table: Table) -> Result<()> {
        let engine = self.get_table_engine(&table.name)?;
        engine.restore_table(table).await
    }

    /// Migrate a table from one engine to another.
    pub async fn migrate_table(&self, name: &str, target_engine: &str) -> Result<()> {
        // Snapshot from current engine
        let snapshot = self.snapshot_table(name).await?;

        // Create in target engine
        let target = self.get_engine(target_engine)?;
        let meta = TableMeta::new(name, target_engine);
        target.create_table(&meta, snapshot.columns.clone()).await?;
        target.restore_table(snapshot).await?;

        // Drop from old engine
        let old_engine_name = {
            let meta = self.metadata.read();
            meta.get(name).map(|m| m.engine.clone())
        };
        if let Some(old_name) = old_engine_name {
            if old_name != target_engine {
                let old_engine = self.get_engine(&old_name)?;
                old_engine.drop_table(name).await?;
            }
        }

        // Update metadata
        {
            let mut metadata = self.metadata.write();
            if let Some(m) = metadata.get_mut(name) {
                m.engine = target_engine.to_string();
            }
        }
        self.save().await?;
        Ok(())
    }

    /// Get engine name for a table.
    pub fn table_engine_name(&self, name: &str) -> Option<String> {
        self.metadata.read().get(name).map(|m| m.engine.clone())
    }

    /// Get all table metadata.
    pub fn all_table_metadata(&self) -> Vec<TableMeta> {
        self.metadata.read().values().cloned().collect()
    }

    /// Get all table names.
    pub fn table_names(&self) -> Vec<String> {
        self.metadata.read().keys().cloned().collect()
    }

    /// Flush all engines.
    pub async fn flush(&self) -> Result<()> {
        for engine in self.engines.values() {
            engine.flush().await?;
        }
        Ok(())
    }

    /// Close all engines.
    pub async fn close(&self) -> Result<()> {
        for engine in self.engines.values() {
            engine.close().await?;
        }
        self.save().await
    }

    /// Persist catalog metadata to disk.
    async fn save(&self) -> Result<()> {
        let default_engine = self.default_engine.read().clone();
        let tables: Vec<TableMeta> = {
            let metadata = self.metadata.read();
            metadata.values().cloned().collect()
        };
        let data = CatalogData {
            default_engine,
            tables,
        };
        let bytes = bincode::serialize(&data)
            .map_err(|e| HelionError::Serialization(e.to_string()))?;
        tokio::fs::write(&self.catalog_path, &bytes).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::*;
    use tempfile::TempDir;

    async fn setup_catalog() -> (Catalog, TempDir) {
        let dir = TempDir::new().unwrap();
        let catalog = Catalog::open(dir.path()).await.unwrap();
        (catalog, dir)
    }

    #[tokio::test]
    async fn test_create_table_default_engine() {
        let (catalog, _dir) = setup_catalog().await;
        catalog.create_table("users", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
        assert!(catalog.table_exists("users").await.unwrap());
        assert_eq!(catalog.table_engine_name("users"), Some("Memory".to_string()));
    }

    #[tokio::test]
    async fn test_create_table_memory_engine() {
        let (catalog, _dir) = setup_catalog().await;
        catalog.create_table("t", vec![ColumnMeta::new("id", DataType::Integer)], Some("Memory")).await.unwrap();
        assert!(catalog.table_exists("t").await.unwrap());
    }

    #[tokio::test]
    async fn test_drop_table() {
        let (catalog, _dir) = setup_catalog().await;
        catalog.create_table("t", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
        assert!(catalog.table_exists("t").await.unwrap());
        catalog.drop_table("t").await.unwrap();
        assert!(!catalog.table_exists("t").await.unwrap());
    }

    #[tokio::test]
    async fn test_unknown_engine_error() {
        let (catalog, _dir) = setup_catalog().await;
        let result = catalog.create_table("t", vec![], Some("Nonexistent")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_default_engine() {
        let (catalog, _dir) = setup_catalog().await;
        assert_eq!(catalog.default_engine(), "Memory");
    }

    #[tokio::test]
    async fn test_table_names() {
        let (catalog, _dir) = setup_catalog().await;
        catalog.create_table("a", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
        catalog.create_table("b", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
        let names = catalog.table_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[tokio::test]
    async fn test_catalog_persistence() {
        let dir = TempDir::new().unwrap();
        // Create catalog, add tables
        {
            let catalog = Catalog::open(dir.path()).await.unwrap();
            catalog.create_table("persist_me", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
            catalog.close().await.unwrap();
        }
        // Re-open catalog, check table still exists
        {
            let catalog = Catalog::open(dir.path()).await.unwrap();
            assert!(catalog.table_exists("persist_me").await.unwrap());
        }
    }

    #[tokio::test]
    async fn test_scan_and_write_through_catalog() {
        let (catalog, _dir) = setup_catalog().await;
        catalog.create_table("t", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();

        catalog.apply_write_set("t", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(42)])))
        ]).await.unwrap();

        let active = BTreeSet::new();
        let rows = catalog.scan_visible("t", 100, &active).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.get(0), Some(&Datum::Integer(42)));
    }

    #[tokio::test]
    async fn test_migrate_table() {
        let (catalog, _dir) = setup_catalog().await;
        catalog.create_table("t", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();

        catalog.apply_write_set("t", vec![
            (0usize, RowVersion::new_insert(5, Row::new(vec![Datum::Integer(42)])))
        ]).await.unwrap();

        // Migrate to same engine (Memory → Memory)
        catalog.migrate_table("t", "Memory").await.unwrap();
        assert!(catalog.table_exists("t").await.unwrap());

        let active = BTreeSet::new();
        let rows = catalog.scan_visible("t", 100, &active).await.unwrap();
        assert_eq!(rows.len(), 1);
    }
}
