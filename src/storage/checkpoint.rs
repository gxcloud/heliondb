use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{self, Duration};
use tracing::{error, info, warn};

use crate::error::Result;
use crate::storage::table::Table;
use crate::storage::wal::{RowVersion, WalRecord, WalWriter};

const CHECKPOINT_FILE: &str = "helion.checkpoint";
#[allow(dead_code)]
const CHECKPOINT_INTERVAL_SECS: u64 = 60;

/// Background task that periodically writes checkpoints.
pub async fn checkpoint_loop(
    data_dir: PathBuf,
    tables: Arc<RwLock<Vec<Table>>>,
    wal_writer: Arc<Mutex<WalWriter>>,
    interval_secs: u64,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = write_checkpoint(&data_dir, &tables, &wal_writer).await {
                    error!("Checkpoint error: {}", e);
                }
            }
            _ = cancel.cancelled() => {
                info!("Checkpoint loop cancelled");
                break;
            }
        }
    }
}

/// Write a full checkpoint snapshot to disk.
pub async fn write_checkpoint(
    _data_dir: &Path,
    tables: &Arc<RwLock<Vec<Table>>>,
    wal_writer: &Arc<Mutex<WalWriter>>,
) -> Result<()> {
    let tables_guard = tables.read().await;
    let table_count = tables_guard.len() as u32;

    let checkpoint_tables: Vec<(String, Vec<_>, Vec<Vec<RowVersion>>)> = tables_guard
        .iter()
        .map(|t| {
            let columns = t.columns.clone();
            let chains = t.version_chains.clone();
            // Debug: print row counts per table
            let visible = t.scan_visible(u64::MAX, &std::collections::BTreeSet::new());
            tracing::debug!(
                "Checkpoint: table '{}' has {} version chains, {} visible",
                t.name,
                chains.len(),
                visible.len()
            );
            (t.name.clone(), columns, chains)
        })
        .collect();
    drop(tables_guard);

    // Write checkpoint record to WAL (will be replayed on next startup)
    let record = WalRecord::Checkpoint {
        table_count,
        tables: checkpoint_tables,
    };
    wal_writer.lock().await.append(record).await?;

    info!("Checkpoint written ({} tables)", table_count);
    Ok(())
}

/// Load checkpoint from disk (if exists).
pub async fn load_checkpoint(data_dir: &Path) -> Result<Option<Vec<Table>>> {
    let path = data_dir.join(CHECKPOINT_FILE);
    if !path.exists() {
        return Ok(None);
    }

    let bytes = fs::read(&path).await?;
    match bincode::deserialize::<Vec<(String, Vec<_>, Vec<Vec<RowVersion>>)>>(&bytes) {
        Ok(checkpoint_tables) => {
            let tables: Vec<Table> = checkpoint_tables
                .into_iter()
                .map(|(name, columns, chains)| {
                    let mut t = Table::new(&name, columns);
                    t.version_chains = chains;
                    t
                })
                .collect();
            info!("Loaded checkpoint with {} tables", tables.len());
            Ok(Some(tables))
        }
        Err(e) => {
            warn!("Failed to deserialize checkpoint: {}", e);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn test_load_checkpoint_nonexistent() {
        let dir = setup_test_dir();
        let result = load_checkpoint(dir.path()).await.unwrap();
        assert!(result.is_none());
    }
}
