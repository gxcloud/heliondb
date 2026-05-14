use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::fs;

use crate::error::{HelionError, Result};
use crate::storage::engine_trait::{StorageEngine, TableMeta};
use crate::storage::table::{RowVersion, Table};
use crate::storage::types::{ColumnMeta, Row};

const META_FILE: &str = "meta.bin";
const BASE_PREFIX: &str = "base_";
const DELTA_PREFIX: &str = "delta_";
const OLD_TABLE_FILE: &str = "table.bin";

/// Disk-backed storage engine with append-only delta persistence.
///
/// Each table is stored under its own directory:
/// ```text
/// engines/disk/{table_name}/
///   meta.bin            — schema (name + columns), written once
///   base_{txid}.bin      — compacted full table snapshot (from compaction)
///   delta_{txid}.bin     — incremental write batches since last base
/// ```
///
/// Writes are O(#changed_rows) instead of O(total_rows). Reads are served
/// from an in-memory HashMap. Periodic compaction merges deltas into a new
/// base file for fast startup.
pub struct DiskEngine {
    base_dir: PathBuf,
    tables: Arc<RwLock<HashMap<String, Table>>>,
    next_delta_id: AtomicU64,
}

impl DiskEngine {
    pub async fn open(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        fs::create_dir_all(&base_dir).await?;

        let engine = DiskEngine {
            base_dir,
            tables: Arc::new(RwLock::new(HashMap::new())),
            next_delta_id: AtomicU64::new(1),
        };
        engine.load_existing_tables().await?;
        Ok(engine)
    }

    fn table_dir(&self, name: &str) -> PathBuf {
        self.base_dir.join(name)
    }

    // ── Startup Load ─────────────────────────────────────────────

    async fn load_existing_tables(&self) -> Result<()> {
        let mut entries = fs::read_dir(&self.base_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let dir_path = entry.path();
            let table_name = dir_path.file_name().unwrap().to_string_lossy().to_string();

            // Try loading with new delta format, fall back to old table.bin
            let table = if self.delta_format_exists(&dir_path).await {
                self.load_table_from_delta(&table_name, &dir_path).await?
            } else if self.old_format_exists(&dir_path).await {
                self.migrate_from_old_format(&table_name, &dir_path).await?
            } else {
                continue;
            };

            let mut tables = self.tables.write();
            tables.insert(table_name, table);
        }
        Ok(())
    }

    /// Check if this table uses the new delta format (has meta.bin).
    async fn delta_format_exists(&self, dir: &Path) -> bool {
        dir.join(META_FILE).exists()
    }

    /// Check if this table uses the old format (has table.bin).
    async fn old_format_exists(&self, dir: &Path) -> bool {
        dir.join(OLD_TABLE_FILE).exists()
    }

    /// Load a table from meta.bin + latest base + delta files.
    async fn load_table_from_delta(&self, _table_name: &str, dir: &Path) -> Result<Table> {
        // 1. Load meta — create empty Table with schema
        let meta_bytes = fs::read(dir.join(META_FILE)).await?;
        let (_name, columns): (String, Vec<ColumnMeta>) = bincode::deserialize(&meta_bytes)
            .map_err(|e| HelionError::Serialization(e.to_string()))?;

        let mut table = Table::new(&_name, columns.clone());

        // 2. Find latest base file
        let mut base_path = None;
        let mut base_txid = 0u64;
        let mut dir_entries = fs::read_dir(dir).await?;
        while let Some(e) = dir_entries.next_entry().await? {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with(BASE_PREFIX) && fname.ends_with(".bin") {
                let txid_str = &fname[BASE_PREFIX.len()..fname.len() - 4];
                if let Ok(txid) = txid_str.parse::<u64>() {
                    if txid > base_txid {
                        base_txid = txid;
                        base_path = Some(e.path());
                    }
                }
            }
        }

        // 3. Load base if found
        if let Some(path) = base_path {
            let base_bytes = fs::read(&path).await?;
            let base_table: Table = bincode::deserialize(&base_bytes)
                .map_err(|e| HelionError::Serialization(e.to_string()))?;
            table = base_table;
        }

        // 4. Find and apply delta files newer than base
        let mut deltas: Vec<(u64, PathBuf)> = Vec::new();
        let mut dir_entries = fs::read_dir(dir).await?;
        while let Some(e) = dir_entries.next_entry().await? {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with(DELTA_PREFIX) && fname.ends_with(".bin") {
                let txid_str = &fname[DELTA_PREFIX.len()..fname.len() - 4];
                if let Ok(txid) = txid_str.parse::<u64>() {
                    if txid > base_txid {
                        deltas.push((txid, e.path()));
                    }
                }
            }
        }
        deltas.sort_by_key(|(txid, _)| *txid);

        for (delta_txid, path) in &deltas {
            let bytes = fs::read(path).await?;
            let changes: Vec<(usize, RowVersion)> = bincode::deserialize(&bytes)
                .map_err(|e| HelionError::Serialization(e.to_string()))?;
            Self::apply_changes(&mut table, changes, *delta_txid);
        }

        Ok(table)
    }

    /// Apply a set of row changes to a table (used for delta replay).
    fn apply_changes(table: &mut Table, changes: Vec<(usize, RowVersion)>, _txid: u64) {
        for (row_idx, version) in changes {
            if row_idx < table.version_chains.len() {
                if let Some(old) = table.version_chains[row_idx].last_mut() {
                    old.txid_max = version.txid_min;
                }
                table.version_chains[row_idx].push(version);
            } else {
                table.version_chains.push(vec![version]);
            }
        }
    }

    /// Migrate from old table.bin format to new delta format.
    async fn migrate_from_old_format(&self, table_name: &str, dir: &Path) -> Result<Table> {
        let old_path = dir.join(OLD_TABLE_FILE);
        let bytes = fs::read(&old_path).await?;
        let table: Table =
            bincode::deserialize(&bytes).map_err(|e| HelionError::Serialization(e.to_string()))?;

        // Write meta.bin
        let meta = (table_name.to_string(), table.columns.clone());
        let meta_bytes =
            bincode::serialize(&meta).map_err(|e| HelionError::Serialization(e.to_string()))?;
        fs::write(dir.join(META_FILE), &meta_bytes).await?;

        // Rename old table.bin → table.bin.migrated (backup)
        let backup = dir.join(format!("{}.migrated", OLD_TABLE_FILE));
        fs::rename(&old_path, &backup).await?;

        Ok(table)
    }

    // ── Delta Persistence ───────────────────────────────────────

    async fn write_delta(
        &self,
        table: &str,
        changes: &[(usize, RowVersion)],
        txid: u64,
    ) -> Result<()> {
        let dir = self.table_dir(table);
        fs::create_dir_all(&dir).await?;
        let filename = format!("{}{:020}.bin", DELTA_PREFIX, txid);
        let final_path = dir.join(&filename);

        // Idempotency: if the delta file already exists, skip (this tx was already applied)
        if final_path.exists() {
            return Ok(());
        }

        let tmp = dir.join(format!("{}.tmp", filename));
        let bytes =
            bincode::serialize(changes).map_err(|e| HelionError::Serialization(e.to_string()))?;
        // Write, fsync, then rename: ensures the file content is durable before it becomes visible
        let mut file = fs::File::create(&tmp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &bytes).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&tmp, &final_path).await?;
        Ok(())
    }

    /// Write a full base snapshot for a table.
    async fn write_base(table: &Table, dir: &Path) -> Result<u64> {
        let txid = table
            .version_chains
            .iter()
            .filter_map(|chain| chain.last().map(|v| v.txid_min))
            .max()
            .unwrap_or(0);

        let filename = format!("{}{:020}.bin", BASE_PREFIX, txid);
        let tmp = dir.join(format!("{}.tmp", filename));
        let final_path = dir.join(&filename);

        let bytes =
            bincode::serialize(table).map_err(|e| HelionError::Serialization(e.to_string()))?;
        // Write, fsync, then rename: ensures the file content is durable before it becomes visible
        let mut file = fs::File::create(&tmp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &bytes).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&tmp, &final_path).await?;
        Ok(txid)
    }

    /// Remove all delta files for a table.
    async fn clean_deltas(dir: &Path) -> Result<()> {
        let mut entries = fs::read_dir(dir).await?;
        while let Some(e) = entries.next_entry().await? {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with(DELTA_PREFIX) && fname.ends_with(".bin") {
                let _ = fs::remove_file(e.path()).await;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl StorageEngine for DiskEngine {
    fn name(&self) -> &str {
        "disk"
    }

    async fn create_table(&self, meta: &TableMeta, columns: Vec<ColumnMeta>) -> Result<()> {
        let dir = self.table_dir(&meta.name);
        fs::create_dir_all(&dir).await?;

        // Write meta.bin
        let meta_data = (meta.name.clone(), columns.clone());
        let meta_bytes = bincode::serialize(&meta_data)
            .map_err(|e| HelionError::Serialization(e.to_string()))?;
        fs::write(dir.join(META_FILE), &meta_bytes).await?;

        let table = Table::new(&meta.name, columns);
        let mut tables = self.tables.write();
        tables.insert(meta.name.clone(), table);
        Ok(())
    }

    async fn drop_table(&self, name: &str) -> Result<()> {
        {
            let mut tables = self.tables.write();
            tables.remove(name);
        }
        let dir = self.table_dir(name);
        if dir.exists() {
            fs::remove_dir_all(dir).await?;
        }
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
            .ok_or_else(|| HelionError::TableNotFound(name.to_string()))
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
            .ok_or_else(|| HelionError::TableNotFound(table.to_string()))?;
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
            .ok_or_else(|| HelionError::TableNotFound(table.to_string()))
    }

    async fn apply_write_set(&self, table: &str, changes: Vec<(usize, RowVersion)>) -> Result<()> {
        // Compute the txid from the changes (use max txid_min)
        let txid = changes
            .iter()
            .map(|(_, v)| v.txid_min)
            .max()
            .unwrap_or_else(|| self.next_delta_id.fetch_add(1, Ordering::SeqCst));

        // 1. Write delta file FIRST (durability: data on disk before in-memory update)
        self.write_delta(table, &changes, txid).await?;

        // 2. Apply to in-memory HashMap
        {
            let mut tables = self.tables.write();
            let t = tables
                .get_mut(table)
                .ok_or_else(|| HelionError::TableNotFound(table.to_string()))?;
            for (row_idx, version) in &changes {
                let row_idx = *row_idx;
                if row_idx < t.version_chains.len() {
                    if let Some(old) = t.version_chains[row_idx].last_mut() {
                        old.txid_max = version.txid_min;
                    }
                    t.version_chains[row_idx].push(version.clone());
                } else {
                    t.version_chains.push(vec![version.clone()]);
                }
            }
        }

        Ok(())
    }

    async fn snapshot_table(&self, table: &str) -> Result<Table> {
        let tables = self.tables.read();
        tables
            .get(table)
            .cloned()
            .ok_or_else(|| HelionError::TableNotFound(table.to_string()))
    }

    async fn restore_table(&self, table: Table) -> Result<()> {
        let dir = self.table_dir(&table.name);
        fs::create_dir_all(&dir).await?;

        // Write meta.bin
        let meta = (table.name.clone(), table.columns.clone());
        let meta_bytes =
            bincode::serialize(&meta).map_err(|e| HelionError::Serialization(e.to_string()))?;
        fs::write(dir.join(META_FILE), &meta_bytes).await?;

        // Write initial base
        Self::write_base(&table, &dir).await?;

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
            let dir = self.table_dir(&table.name);
            // Write base snapshot
            Self::write_base(&table, &dir).await?;
            // Clean up delta files
            Self::clean_deltas(&dir).await?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.flush().await
    }
}
