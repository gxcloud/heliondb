use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::fs;

use crate::error::{HelionError, Result};
use crate::storage::engine_trait::{StorageEngine, TableMeta};
use crate::storage::table::{RowVersion, Table};
use crate::storage::types::{ColumnMeta, Row};

const SNAPSHOT_FILE: &str = "table.bin";

/// Disk-backed storage engine. Each table lives in its own directory.
pub struct DiskEngine {
    base_dir: PathBuf,
    tables: Arc<RwLock<HashMap<String, Table>>>,
}

impl DiskEngine {
    pub async fn open(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        fs::create_dir_all(&base_dir).await?;

        let engine = DiskEngine {
            base_dir,
            tables: Arc::new(RwLock::new(HashMap::new())),
        };
        engine.load_existing_tables().await?;
        Ok(engine)
    }

    fn table_dir(&self, name: &str) -> PathBuf {
        self.base_dir.join(name)
    }

    async fn load_existing_tables(&self) -> Result<()> {
        let mut entries = fs::read_dir(&self.base_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if !file_type.is_dir() {
                continue;
            }

            let table_path = entry.path().join(SNAPSHOT_FILE);
            if !table_path.exists() {
                continue;
            }

            let bytes = fs::read(&table_path).await?;
            let table: Table = bincode::deserialize(&bytes)
                .map_err(|e| HelionError::Serialization(e.to_string()))?;
            self.tables.write().insert(table.name.clone(), table);
        }
        Ok(())
    }

    async fn persist_table(&self, table: &Table) -> Result<()> {
        let dir = self.table_dir(&table.name);
        fs::create_dir_all(&dir).await?;
        let tmp_path = dir.join(format!("{}.tmp", SNAPSHOT_FILE));
        let final_path = dir.join(SNAPSHOT_FILE);
        let bytes =
            bincode::serialize(table).map_err(|e| HelionError::Serialization(e.to_string()))?;
        fs::write(&tmp_path, &bytes).await?;
        fs::rename(&tmp_path, &final_path).await?;
        Ok(())
    }

    async fn remove_table_dir(&self, name: &str) -> Result<()> {
        let dir = self.table_dir(name);
        if dir.exists() {
            fs::remove_dir_all(dir).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl StorageEngine for DiskEngine {
    fn name(&self) -> &str {
        "disk"
    }

    async fn create_table(&self, _meta: &TableMeta, columns: Vec<ColumnMeta>) -> Result<()> {
        {
            let tables = self.tables.read();
            if tables.contains_key(&_meta.name) {
                return Err(crate::error::HelionError::TableAlreadyExists(
                    _meta.name.clone(),
                ));
            }
        }

        let table = Table::new(&_meta.name, columns);
        self.persist_table(&table).await?;

        let mut tables = self.tables.write();
        if tables.contains_key(&_meta.name) {
            return Err(crate::error::HelionError::TableAlreadyExists(
                _meta.name.clone(),
            ));
        }
        tables.insert(_meta.name.clone(), table);
        Ok(())
    }

    async fn drop_table(&self, name: &str) -> Result<()> {
        {
            let mut tables = self.tables.write();
            if tables.remove(name).is_none() {
                return Err(crate::error::HelionError::TableNotFound(name.to_string()));
            }
        }
        self.remove_table_dir(name).await?;
        Ok(())
    }

    async fn table_exists(&self, name: &str) -> Result<bool> {
        let tables = self.tables.read();
        Ok(tables.contains_key(name))
    }

    async fn get_table(&self, name: &str) -> Result<Table> {
        let tables = self.tables.read();
        tables
            .get(name)
            .cloned()
            .ok_or_else(|| crate::error::HelionError::TableNotFound(name.to_string()))
    }

    async fn scan_visible(
        &self,
        table: &str,
        snapshot_txid: u64,
        active_txns: &BTreeSet<u64>,
    ) -> Result<Vec<(usize, Row)>> {
        let tables = self.tables.read();
        let t = tables
            .get(table)
            .ok_or_else(|| crate::error::HelionError::TableNotFound(table.to_string()))?;
        Ok(t.scan_visible(snapshot_txid, active_txns)
            .into_iter()
            .map(|(idx, row)| (idx, row.clone()))
            .collect())
    }

    async fn row_count(&self, table: &str) -> Result<usize> {
        let tables = self.tables.read();
        tables
            .get(table)
            .map(|t| t.row_count())
            .ok_or_else(|| crate::error::HelionError::TableNotFound(table.to_string()))
    }

    async fn apply_write_set(&self, table: &str, changes: Vec<(usize, RowVersion)>) -> Result<()> {
        let snapshot = {
            let mut tables = self.tables.write();
            let t = tables
                .get_mut(table)
                .ok_or_else(|| crate::error::HelionError::TableNotFound(table.to_string()))?;

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
            t.clone()
        };

        self.persist_table(&snapshot).await
    }

    async fn snapshot_table(&self, table: &str) -> Result<Table> {
        let tables = self.tables.read();
        tables
            .get(table)
            .cloned()
            .ok_or_else(|| crate::error::HelionError::TableNotFound(table.to_string()))
    }

    async fn restore_table(&self, table: Table) -> Result<()> {
        self.persist_table(&table).await?;
        let mut tables = self.tables.write();
        tables.insert(table.name.clone(), table);
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        let snapshot: Vec<Table> = {
            let tables = self.tables.read();
            tables.values().cloned().collect()
        };
        for table in snapshot {
            self.persist_table(&table).await?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.flush().await
    }
}
