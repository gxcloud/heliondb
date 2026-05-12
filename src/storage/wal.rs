use std::path::{Path, PathBuf};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

use crate::error::{HelionError, Result};
use crate::storage::types::{ColumnMeta, Row};
use crate::storage::table::Table;

const WAL_FILE: &str = "helion.wal";
const WAL_BUFFER_SIZE: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalRecord {
    CreateTable {
        name: String,
        columns: Vec<ColumnMeta>,
    },
    DropTable {
        name: String,
    },
    Insert {
        table: String,
        row: Row,
        txid: u64,
    },
    Update {
        table: String,
        row_idx: usize,
        new_row: Row,
        txid: u64,
    },
    Delete {
        table: String,
        row_idx: usize,
        txid: u64,
    },
    Commit {
        txid: u64,
    },
    Checkpoint {
        table_count: u32,
        tables: Vec<(String, Vec<ColumnMeta>, Vec<Vec<RowVersion>>)>,
    },
}

/// Re-export RowVersion for serialization in checkpoint.
pub use crate::storage::table::RowVersion;

#[derive(Debug)]
pub struct WalWriter {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
    sender: Option<mpsc::UnboundedSender<WalRecord>>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

impl WalWriter {
    /// Open or create the WAL file and start the background writer task.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(WAL_FILE);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| HelionError::Io(format!("Failed to open WAL file '{}': {}", path.display(), e)))?;

        let (sender, mut receiver) = mpsc::unbounded_channel::<WalRecord>();

        let join_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            let mut file = file;
            let mut buffer = Vec::with_capacity(WAL_BUFFER_SIZE);
            let mut flush_interval = time::interval(Duration::from_millis(5));

            loop {
                tokio::select! {
                    maybe_record = receiver.recv() => {
                        match maybe_record {
                            Some(record) => {
                                buffer.push(record);
                            }
                            None => {
                                // Channel closed, flush and exit
                                if !buffer.is_empty() {
                                    if let Err(e) = flush_wal(&mut file, &buffer).await {
                                        error!("WAL flush error on shutdown: {}", e);
                                    }
                                }
                                if let Err(e) = file.flush().await {
                                    error!("WAL final fsync error: {}", e);
                                }
                                break;
                            }
                        }
                    }
                    _ = flush_interval.tick() => {
                        if !buffer.is_empty() {
                            if let Err(e) = flush_wal(&mut file, &buffer).await {
                                error!("WAL flush error: {}", e);
                            }
                            buffer.clear();
                        }
                    }
                }
            }
        });

        Ok(WalWriter {
            file: fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?,
            path,
            sender: Some(sender),
            join_handle: Some(join_handle),
        })
    }

    /// Append a record to the WAL (non-blocking, via channel).
    pub fn append(&self, record: WalRecord) -> Result<()> {
        self.sender
            .as_ref()
            .ok_or_else(|| HelionError::Io("WAL writer not initialized".to_string()))?
            .send(record)
            .map_err(|e| HelionError::Io(format!("WAL channel send error: {}", e)))
    }

    /// Flush any pending records and close the WAL.
    pub async fn close(&mut self) -> Result<()> {
        // Drop the sender to close the channel, signal writer task to exit
        self.sender.take();
        if let Some(handle) = self.join_handle.take() {
            handle.await.map_err(|e| HelionError::Internal(format!("WAL writer task error: {}", e)))?;
        }
        self.file.flush().await?;
        self.file.sync_all().await?;
        Ok(())
    }
}

/// Flush a batch of records to the WAL file.
async fn flush_wal(file: &mut File, records: &[WalRecord]) -> Result<()> {
    for record in records {
        let bytes = bincode::serialize(record)
            .map_err(|e| HelionError::Serialization(e.to_string()))?;
        let len = bytes.len() as u64;
        let len_bytes = len.to_le_bytes();

        // Write: length prefix (8 bytes) + data + checksum (4 bytes)
        let checksum = crc32fast::hash(&bytes);
        let checksum_bytes = checksum.to_le_bytes();

        file.write_all(&len_bytes).await?;
        file.write_all(&bytes).await?;
        file.write_all(&checksum_bytes).await?;
    }
    file.flush().await?;
    Ok(())
}

/// Replay the WAL file to reconstruct table state.
pub async fn replay_wal(data_dir: &Path) -> Result<Vec<Table>> {
    let path = data_dir.join(WAL_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(&path).await?;
    let mut tables: Vec<Table> = Vec::new();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;

    let mut offset = 0;
    while offset + 8 <= buf.len() {
        // Read length prefix
        let len_bytes: [u8; 8] = buf[offset..offset + 8].try_into().unwrap();
        let record_len = u64::from_le_bytes(len_bytes) as usize;
        offset += 8;

        if offset + record_len + 4 > buf.len() {
            warn!("WAL: truncated record at offset {}, skipping", offset - 8);
            break;
        }

        let data = &buf[offset..offset + record_len];
        offset += record_len;

        // Read and verify checksum
        let stored_checksum: [u8; 4] = buf[offset..offset + 4].try_into().unwrap();
        let expected_checksum = crc32fast::hash(data);
        if u32::from_le_bytes(stored_checksum) != expected_checksum {
            warn!("WAL: checksum mismatch at offset {}, skipping", offset - 4);
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

/// Apply a single WAL record to the table list during replay.
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
                    warn!("WAL replay: skipping invalid row for table '{}': {}", table, e);
                    return;
                }
                t.version_chains.push(vec![RowVersion::new_insert(txid, row)]);
            }
        }
        WalRecord::Update { table, row_idx, new_row, txid } => {
            if let Some(t) = tables.iter_mut().find(|t| t.name == table) {
                if row_idx < t.version_chains.len() {
                    if let Some(old_latest) = t.version_chains[row_idx].last_mut() {
                        old_latest.txid_max = txid;
                    }
                    t.version_chains[row_idx].push(RowVersion::new_update(txid, new_row));
                }
            }
        }
        WalRecord::Delete { table, row_idx, txid } => {
            if let Some(t) = tables.iter_mut().find(|t| t.name == table) {
                if row_idx < t.version_chains.len() {
                    if let Some(latest) = t.version_chains[row_idx].last() {
                        let old_row = latest.row.clone();
                        if let Some(old_latest) = t.version_chains[row_idx].last_mut() {
                            old_latest.txid_max = txid;
                        }
                        t.version_chains[row_idx].push(RowVersion::new_delete(txid, old_row));
                    }
                }
            }
        }
        WalRecord::Commit { txid: _ } => {
            // Commit records are informational; actual state is captured by the data records above
        }
        WalRecord::Checkpoint { tables: checkpoint_tables, .. } => {
            // Replace all tables with checkpoint data
            *tables = checkpoint_tables
                .into_iter()
                .map(|(name, columns, chains)| {
                    let mut t = Table::new(&name, columns);
                    t.version_chains = chains;
                    t
                })
                .collect();
        }
    }
}

/// Get the WAL file path.
pub fn wal_path(data_dir: &Path) -> PathBuf {
    data_dir.join(WAL_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
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
            columns: vec![
                ColumnMeta::new("id", DataType::Integer),
                ColumnMeta::new("name", DataType::Text),
            ],
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
        assert_eq!(tables[0].row_count(), 1);
        apply_wal_record(&mut tables, WalRecord::Delete {
            table: "test".to_string(),
            row_idx: 0,
            txid: 10,
        });
        // Row still exists in version chain (as tombstone)
        assert_eq!(tables[0].version_chains.len(), 1);
        assert_eq!(tables[0].version_chains[0].len(), 2);
    }

    #[tokio::test]
    async fn test_wal_write_and_replay() {
        let dir = setup_test_dir();
        let mut wal = WalWriter::open(dir.path()).await.unwrap();

        wal.append(WalRecord::CreateTable {
            name: "users".to_string(),
            columns: vec![
                ColumnMeta::new("id", DataType::Integer).primary_key(),
                ColumnMeta::new("name", DataType::Text),
            ],
        }).unwrap();

        wal.append(WalRecord::Insert {
            table: "users".to_string(),
            row: Row::new(vec![Datum::Integer(1), Datum::Text("Alice".into())]),
            txid: 5,
        }).unwrap();

        // Close the WAL (flushes all records)
        wal.close().await.unwrap();

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
        }).unwrap();

        wal.append(WalRecord::Insert {
            table: "users".to_string(),
            row: Row::new(vec![Datum::Integer(1)]),
            txid: 5,
        }).unwrap();

        // Checkpoint with different data
        let chains = vec![vec![RowVersion::new_insert(10, Row::new(vec![Datum::Integer(42)]))]];
        wal.append(WalRecord::Checkpoint {
            table_count: 1,
            tables: vec![(
                "users".to_string(),
                vec![ColumnMeta::new("id", DataType::Integer)],
                chains,
            )],
        }).unwrap();

        wal.close().await.unwrap();

        let tables = replay_wal(dir.path()).await.unwrap();
        assert_eq!(tables.len(), 1);
        // After checkpoint, we should see the checkpoint data (id=42) not the original (id=1)
        let visible = tables[0].scan_visible(u64::MAX, &BTreeSet::new());
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1.get(0), Some(&Datum::Integer(42)));
    }
}
