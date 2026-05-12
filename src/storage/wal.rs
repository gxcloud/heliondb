use std::path::{Path, PathBuf};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{HelionError, Result};
use crate::storage::permissions::Permission;
use crate::storage::types::{ColumnMeta, Row};
use crate::storage::table::Table;

const WAL_FILE: &str = "helion.wal";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalRecord {
    CreateTable { name: String, columns: Vec<ColumnMeta> },
    DropTable { name: String },
    Insert { table: String, row: Row, txid: u64 },
    Update { table: String, row_idx: usize, new_row: Row, txid: u64 },
    Delete { table: String, row_idx: usize, txid: u64 },
    Commit { txid: u64 },
    Checkpoint { table_count: u32, tables: Vec<(String, Vec<ColumnMeta>, Vec<Vec<RowVersion>>)> },
    CreateUser { username: String, password_hash: String },
    DropUser { username: String },
    Grant { username: String, table: String, permission: Permission },
    Revoke { username: String, table: String, permission: Permission },
}

pub use crate::storage::table::RowVersion;

pub struct WalWriter {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl WalWriter {
    /// Open or create the WAL file.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(WAL_FILE);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| HelionError::Io(format!("Failed to open WAL '{}': {}", path.display(), e)))?;
        Ok(WalWriter { file, path })
    }

    /// Write a record to the WAL immediately (async).
    pub async fn append(&mut self, record: WalRecord) -> Result<()> {
        let bytes = bincode::serialize(&record)
            .map_err(|e| HelionError::Serialization(e.to_string()))?;
        let len = bytes.len() as u64;
        let checksum = crc32fast::hash(&bytes);

        // Write: length prefix (8 bytes) + data + checksum (4 bytes)
        self.file.write_all(&len.to_le_bytes()).await?;
        self.file.write_all(&bytes).await?;
        self.file.write_all(&checksum.to_le_bytes()).await?;
        self.file.flush().await?;
        Ok(())
    }

    /// Flush and sync the WAL file.
    pub async fn flush(&mut self) -> Result<()> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        Ok(())
    }
}

/// Replay the WAL file to reconstruct table state.
pub async fn replay_wal(data_dir: &Path) -> Result<Vec<Table>> {
    let path = data_dir.join(WAL_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(&path).await?;
    let mut buf = Vec::new();
    use tokio::io::AsyncReadExt;
    file.read_to_end(&mut buf).await?;

    let mut tables: Vec<Table> = Vec::new();
    let mut offset = 0;
    while offset + 8 <= buf.len() {
        let len_bytes: [u8; 8] = buf[offset..offset + 8].try_into().unwrap();
        let record_len = u64::from_le_bytes(len_bytes) as usize;
        offset += 8;

        if offset + record_len + 4 > buf.len() {
            warn!("WAL: truncated record at offset {}", offset - 8);
            break;
        }

        let data = &buf[offset..offset + record_len];
        offset += record_len;

        let stored_checksum: [u8; 4] = buf[offset..offset + 4].try_into().unwrap();
        let expected = crc32fast::hash(data);
        if u32::from_le_bytes(stored_checksum) != expected {
            warn!("WAL: checksum mismatch at offset {}", offset - 4);
            offset += 4;
            continue;
        }
        offset += 4;

        let record: WalRecord = match bincode::deserialize(data) {
            Ok(r) => r,
            Err(e) => {
                warn!("WAL: deserialize error at offset {}: {}", offset - record_len - 4, e);
                continue;
            }
        };

        apply_wal_record(&mut tables, record);
    }

    Ok(tables)
}

fn apply_wal_record(tables: &mut Vec<Table>, record: WalRecord) {
    match record {
        WalRecord::CreateTable { name, columns } => {
            if !tables.iter().any(|t| t.name == name) {
                tables.push(Table::new(&name, columns));
            }
        }
        WalRecord::DropTable { name } => {
            tables.retain(|t| t.name != name);
        }
        WalRecord::Insert { table, row, txid } => {
            if let Some(t) = tables.iter_mut().find(|t| t.name == table) {
                if let Err(e) = t.validate_row(&row) {
                    warn!("WAL replay: skipping invalid row for '{}': {}", table, e);
                    return;
                }
                t.version_chains.push(vec![RowVersion::new_insert(txid, row)]);
            }
        }
        WalRecord::Update { table, row_idx, new_row, txid } => {
            if let Some(t) = tables.iter_mut().find(|t| t.name == table) {
                if row_idx < t.version_chains.len() {
                    if let Some(old) = t.version_chains[row_idx].last_mut() {
                        old.txid_max = txid;
                    }
                    t.version_chains[row_idx].push(RowVersion::new_update(txid, new_row));
                }
            }
        }
        WalRecord::Delete { table, row_idx, txid } => {
            if let Some(t) = tables.iter_mut().find(|t| t.name == table) {
                if row_idx < t.version_chains.len() {
                    let old_row = t.version_chains[row_idx].last().map(|v| v.row.clone()).unwrap();
                    if let Some(old) = t.version_chains[row_idx].last_mut() {
                        old.txid_max = txid;
                    }
                    t.version_chains[row_idx].push(RowVersion::new_delete(txid, old_row));
                }
            }
        }
        WalRecord::Commit { .. } => {}
        WalRecord::Checkpoint { tables: checkpoint_tables, .. } => {
            *tables = checkpoint_tables.into_iter()
                .map(|(name, columns, chains)| {
                    let mut t = Table::new(&name, columns);
                    t.version_chains = chains;
                    t
                })
                .collect();
        }
        WalRecord::CreateUser { .. }
        | WalRecord::DropUser { .. }
        | WalRecord::Grant { .. }
        | WalRecord::Revoke { .. } => {
            // These are handled by the engine on replay, not during WAL replay
            // (the engine stores users/permissions separately from tables)
        }
    }
}

pub fn wal_path(data_dir: &Path) -> PathBuf {
    data_dir.join(WAL_FILE)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use super::*;
    use crate::storage::types::*;
    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_apply_create_table() {
        let mut tables = Vec::new();
        apply_wal_record(&mut tables, WalRecord::CreateTable {
            name: "test".to_string(),
            columns: vec![ColumnMeta::new("id", DataType::Integer)],
        });
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "test");
    }

    #[test]
    fn test_apply_insert() {
        let mut tables = Vec::new();
        apply_wal_record(&mut tables, WalRecord::CreateTable {
            name: "test".to_string(),
            columns: vec![ColumnMeta::new("id", DataType::Integer), ColumnMeta::new("name", DataType::Text)],
        });
        apply_wal_record(&mut tables, WalRecord::Insert {
            table: "test".to_string(),
            row: Row::new(vec![Datum::Integer(1), Datum::Text("Alice".into())]),
            txid: 5,
        });
        assert_eq!(tables[0].row_count(), 1);
    }

    #[test]
    fn test_apply_delete() {
        let mut tables = Vec::new();
        apply_wal_record(&mut tables, WalRecord::CreateTable {
            name: "test".to_string(),
            columns: vec![ColumnMeta::new("id", DataType::Integer)],
        });
        apply_wal_record(&mut tables, WalRecord::Insert {
            table: "test".to_string(),
            row: Row::new(vec![Datum::Integer(1)]),
            txid: 5,
        });
        apply_wal_record(&mut tables, WalRecord::Delete { table: "test".to_string(), row_idx: 0, txid: 10 });
        assert_eq!(tables[0].version_chains.len(), 1);
        assert_eq!(tables[0].version_chains[0].len(), 2);
    }

    #[tokio::test]
    async fn test_wal_write_and_replay() {
        let dir = setup_test_dir();
        let mut wal = WalWriter::open(dir.path()).await.unwrap();

        wal.append(WalRecord::CreateTable {
            name: "users".to_string(),
            columns: vec![ColumnMeta::new("id", DataType::Integer).primary_key(), ColumnMeta::new("name", DataType::Text)],
        }).await.unwrap();

        wal.append(WalRecord::Insert {
            table: "users".to_string(),
            row: Row::new(vec![Datum::Integer(1), Datum::Text("Alice".into())]),
            txid: 5,
        }).await.unwrap();

        wal.flush().await.unwrap();

        let tables = replay_wal(dir.path()).await.unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "users");
        assert_eq!(tables[0].row_count(), 1);
    }

    #[tokio::test]
    async fn test_wal_replay_empty() {
        let dir = setup_test_dir();
        let tables = replay_wal(dir.path()).await.unwrap();
        assert_eq!(tables.len(), 0);
    }

    #[tokio::test]
    async fn test_wal_replay_checkpoint() {
        let dir = setup_test_dir();
        let mut wal = WalWriter::open(dir.path()).await.unwrap();

        wal.append(WalRecord::CreateTable {
            name: "users".to_string(),
            columns: vec![ColumnMeta::new("id", DataType::Integer)],
        }).await.unwrap();

        wal.append(WalRecord::Insert {
            table: "users".to_string(),
            row: Row::new(vec![Datum::Integer(1)]),
            txid: 5,
        }).await.unwrap();

        let chains = vec![vec![RowVersion::new_insert(10, Row::new(vec![Datum::Integer(42)]))]];
        wal.append(WalRecord::Checkpoint {
            table_count: 1,
            tables: vec![("users".to_string(), vec![ColumnMeta::new("id", DataType::Integer)], chains)],
        }).await.unwrap();

        wal.flush().await.unwrap();

        let tables = replay_wal(dir.path()).await.unwrap();
        assert_eq!(tables.len(), 1);
        let visible = tables[0].scan_visible(u64::MAX, &BTreeSet::new());
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1.get(0), Some(&Datum::Integer(42)));
    }
}
