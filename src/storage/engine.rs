use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::error::{HelionError, Result};
use crate::storage::checkpoint::{checkpoint_loop, write_checkpoint};
use crate::storage::mvcc::{MvccStore, Transaction, WriteEntry, WriteOp, TransactionStatus};
use crate::storage::table::Table;
use crate::storage::types::ColumnMeta;
use crate::storage::wal::{replay_wal, WalRecord, WalWriter};

/// The central database engine that ties together MVCC, storage, and WAL persistence.
pub struct DatabaseEngine {
    /// All tables in the database (protected by RwLock for concurrent read access).
    pub(crate) tables: Arc<RwLock<Vec<Table>>>,
    /// MVCC transaction store.
    pub(crate) mvcc: MvccStore,
    /// WAL writer (shared across tasks).
    wal_writer: Arc<WalWriter>,
    /// Data directory for persistence.
    data_dir: PathBuf,
    /// Cancellation token for background tasks.
    cancel: CancellationToken,
}

impl DatabaseEngine {
    /// Create a new database engine, loading state from the WAL.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        if !data_dir.exists() {
            std::fs::create_dir_all(data_dir)?;
        }

        // Try to replay WAL
        let tables = replay_wal(data_dir).await?;
        info!("Replayed WAL: {} tables restored", tables.len());

        let wal_writer = WalWriter::open(data_dir).await?;

        let engine = DatabaseEngine {
            tables: Arc::new(RwLock::new(tables)),
            mvcc: MvccStore::new(),
            wal_writer: Arc::new(wal_writer),
            data_dir: data_dir.to_path_buf(),
            cancel: CancellationToken::new(),
        };

        // Start the checkpoint background loop
        let checkpoint_tables = engine.tables.clone();
        let checkpoint_wal = engine.wal_writer.clone();
        let checkpoint_data_dir = engine.data_dir.clone();
        let checkpoint_cancel = engine.cancel.clone();
        tokio::spawn(async move {
            checkpoint_loop(
                checkpoint_data_dir,
                checkpoint_tables,
                checkpoint_wal,
                60,
                checkpoint_cancel,
            )
            .await;
        });

        Ok(engine)
    }

    /// Begin a new transaction.
    pub fn begin(&self) -> Transaction {
        let tx = self.mvcc.begin_transaction();
        debug!("BEGIN transaction {}", tx.tx_id);
        tx
    }

    /// Commit a transaction: check conflicts, apply writes, append to WAL.
    pub async fn commit(&self, tx: &mut Transaction) -> Result<()> {
        if tx.status != TransactionStatus::InProgress {
            return Err(HelionError::Transaction(format!(
                "Transaction {} is not in progress",
                tx.tx_id
            )));
        }

        // Check for optimistic concurrency conflicts
        let tables_guard = self.tables.read().await;
        let tables_snapshot: std::collections::HashMap<String, Table> = tables_guard
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        drop(tables_guard);

        if let Err(conflicting_tx) = self.mvcc.check_conflicts(tx, &tables_snapshot) {
            self.mvcc.rollback_transaction(tx);
            tx.status = TransactionStatus::Aborted;
            debug!("Transaction {} aborted (conflict with {})", tx.tx_id, conflicting_tx);
            return Err(HelionError::Conflict(tx.tx_id));
        }

        // Apply writes to tables
        let changes = MvccStore::apply_write_set(&tx.write_set, &tables_snapshot, tx.tx_id);

        // Apply changes to the real tables
        {
            let mut tables_guard = self.tables.write().await;
            for (table_name, new_chains) in &changes {
                if let Some(table) = tables_guard.iter_mut().find(|t| t.name == *table_name) {
                    table.version_chains = new_chains.clone();
                }
            }
        }

        // Append all write operations to WAL
        for entry in &tx.write_set {
            match &entry.operation {
                WriteOp::Insert(row) => {
                    self.wal_writer.append(WalRecord::Insert {
                        table: entry.table_name.clone(),
                        row: row.clone(),
                        txid: tx.tx_id,
                    })?;
                }
                WriteOp::Update(new_row) => {
                    self.wal_writer.append(WalRecord::Update {
                        table: entry.table_name.clone(),
                        row_idx: entry.row_idx,
                        new_row: new_row.clone(),
                        txid: tx.tx_id,
                    })?;
                }
                WriteOp::Delete => {
                    self.wal_writer.append(WalRecord::Delete {
                        table: entry.table_name.clone(),
                        row_idx: entry.row_idx,
                        txid: tx.tx_id,
                    })?;
                }
            }
        }

        self.wal_writer.append(WalRecord::Commit { txid: tx.tx_id })?;

        self.mvcc.commit_transaction(tx);
        tx.status = TransactionStatus::Committed;
        debug!("COMMIT transaction {}", tx.tx_id);
        Ok(())
    }

    /// Rollback a transaction.
    pub fn rollback(&self, tx: &mut Transaction) {
        if tx.status != TransactionStatus::InProgress {
            return;
        }
        self.mvcc.rollback_transaction(tx);
        tx.status = TransactionStatus::Aborted;
        debug!("ROLLBACK transaction {}", tx.tx_id);
    }

    /// Execute a read-only function within a transaction context.
    pub async fn with_read_txn<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Transaction, &[Table]) -> Result<R>,
    {
        let tx = self.begin();
        let tables_guard = self.tables.read().await;
        let result = f(&tx, &tables_guard)?;
        self.mvcc.commit_transaction(&tx);
        Ok(result)
    }

    /// Execute a write function within a transaction context.
    pub async fn with_write_txn<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Transaction, &[Table]) -> Result<Vec<WriteEntry>>,
    {
        let mut tx = self.begin();
        let tables_guard = self.tables.read().await;
        let write_entries = f(&mut tx, &tables_guard)?;
        drop(tables_guard);

        for entry in write_entries {
            tx.add_write(entry);
        }

        self.commit(&mut tx).await
    }

    /// Get an immutable snapshot of all tables.
    pub async fn get_tables(&self) -> Vec<Table> {
        self.tables.read().await.clone()
    }

    /// Create a new table.
    pub async fn create_table(&self, name: &str, columns: Vec<ColumnMeta>) -> Result<()> {
        let mut tables_guard = self.tables.write().await;
        if tables_guard.iter().any(|t| t.name == name) {
            return Err(HelionError::TableAlreadyExists(name.to_string()));
        }
        let table = Table::new(name, columns.clone());
        tables_guard.push(table);
        self.wal_writer.append(WalRecord::CreateTable {
            name: name.to_string(),
            columns,
        })?;
        debug!("Created table '{}'", name);
        Ok(())
    }

    /// Drop a table.
    pub async fn drop_table(&self, name: &str) -> Result<()> {
        let mut tables_guard = self.tables.write().await;
        let initial_len = tables_guard.len();
        tables_guard.retain(|t| t.name != name);
        if tables_guard.len() == initial_len {
            return Err(HelionError::TableNotFound(name.to_string()));
        }
        self.wal_writer.append(WalRecord::DropTable {
            name: name.to_string(),
        })?;
        debug!("Dropped table '{}'", name);
        Ok(())
    }

    /// Shutdown the engine: write checkpoint, close WAL.
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down database engine...");
        self.cancel.cancel();

        // Write a final checkpoint
        write_checkpoint(&self.data_dir, &self.tables, &self.wal_writer).await?;

        info!("Database engine shut down");
        Ok(())
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::*;
    use tempfile::TempDir;

    async fn setup_engine() -> (DatabaseEngine, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        (engine, dir)
    }

    #[tokio::test]
    async fn test_create_table() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table(
                "users",
                vec![
                    ColumnMeta::new("id", DataType::Integer).primary_key(),
                    ColumnMeta::new("name", DataType::Text),
                ],
            )
            .await
            .unwrap();

        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "users");
    }

    #[tokio::test]
    async fn test_create_duplicate_table() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table("test", vec![ColumnMeta::new("id", DataType::Integer)])
            .await
            .unwrap();
        let result = engine
            .create_table("test", vec![ColumnMeta::new("id", DataType::Integer)])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_drop_table() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table("users", vec![ColumnMeta::new("id", DataType::Integer)])
            .await
            .unwrap();
        engine.drop_table("users").await.unwrap();
        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 0);
    }

    #[tokio::test]
    async fn test_drop_nonexistent_table() {
        let (engine, _dir) = setup_engine().await;
        let result = engine.drop_table("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_with_read_txn() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table(
                "users",
                vec![
                    ColumnMeta::new("id", DataType::Integer).primary_key(),
                    ColumnMeta::new("name", DataType::Text),
                ],
            )
            .await
            .unwrap();

        let result = engine
            .with_read_txn(|_tx, tables| {
                assert_eq!(tables.len(), 1);
                assert_eq!(tables[0].name, "users");
                Ok(42)
            })
            .await
            .unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_with_write_txn_insert() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table(
                "users",
                vec![
                    ColumnMeta::new("id", DataType::Integer).primary_key(),
                    ColumnMeta::new("name", DataType::Text),
                ],
            )
            .await
            .unwrap();

        engine
            .with_write_txn(|_tx, tables| {
                let table = tables.iter().find(|t| t.name == "users").unwrap();
                let row = Row::new(vec![Datum::Integer(1), Datum::Text("Alice".into())]);
                let entry = WriteEntry {
                    table_name: "users".to_string(),
                    row_idx: table.row_count(), // new row
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Insert(row),
                };
                Ok(vec![entry])
            })
            .await
            .unwrap();

        let tables = engine.get_tables().await;
        let table = tables.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(table.row_count(), 1);
    }

    #[tokio::test]
    async fn test_engine_replay_from_wal() {
        let dir = tempfile::tempdir().unwrap();

        // Create engine, create table and insert data
        {
            let engine = DatabaseEngine::open(dir.path()).await.unwrap();
            engine
                .create_table(
                    "users",
                    vec![
                        ColumnMeta::new("id", DataType::Integer).primary_key(),
                        ColumnMeta::new("name", DataType::Text),
                    ],
                )
                .await
                .unwrap();

            engine
            .with_write_txn(|_tx, tables| {
                    let table = tables.iter().find(|t| t.name == "users").unwrap();
                    let row = Row::new(vec![Datum::Integer(1), Datum::Text("Alice".into())]);
                    let entry = WriteEntry {
                        table_name: "users".to_string(),
                        row_idx: table.row_count(),
                        old_txid_max: u64::MAX,
                        operation: WriteOp::Insert(row),
                    };
                    Ok(vec![entry])
                })
                .await
                .unwrap();

            engine.shutdown().await.unwrap();
        }

        // Re-open engine and verify data is restored from WAL
        {
            let engine = DatabaseEngine::open(dir.path()).await.unwrap();
            let tables = engine.get_tables().await;
            assert_eq!(tables.len(), 1);
            assert_eq!(tables[0].name, "users");
            assert_eq!(tables[0].row_count(), 1);
            engine.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_commit_and_rollback() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table("test", vec![ColumnMeta::new("id", DataType::Integer)])
            .await
            .unwrap();

        // Rollback a transaction
        let mut tx = engine.begin();
        let _tables = engine.get_tables().await;
        let row = Row::new(vec![Datum::Integer(42)]);
        let entry = WriteEntry {
            table_name: "test".to_string(),
            row_idx: 0,
            old_txid_max: u64::MAX,
            operation: WriteOp::Insert(row),
        };
        tx.add_write(entry);
        engine.rollback(&mut tx);
        assert_eq!(tx.status, TransactionStatus::Aborted);

        // After rollback, no rows should be in table
        let _tables = engine.get_tables().await;
        assert_eq!(_tables[0].row_count(), 0);
    }

    #[tokio::test]
    async fn test_optimistic_conflict() {
        let (engine, _dir) = setup_engine().await;
        engine
            .create_table(
                "test",
                vec![
                    ColumnMeta::new("id", DataType::Integer).primary_key(),
                    ColumnMeta::new("val", DataType::Integer),
                ],
            )
            .await
            .unwrap();

        // Insert a row
        engine
            .with_write_txn(|_tx, tables| {
                let table = tables.iter().find(|t| t.name == "test").unwrap();
                let row = Row::new(vec![Datum::Integer(1), Datum::Integer(100)]);
                let entry = WriteEntry {
                    table_name: "test".to_string(),
                    row_idx: table.row_count(),
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Insert(row),
                };
                Ok(vec![entry])
            })
            .await
            .unwrap();

        // Now simulate a conflict by having two concurrent transactions
        // Both begin before either commits, so both see the same initial state
        let mut tx1 = engine.begin();
        let mut tx2 = engine.begin();

        // Both read tables at their respective snapshots
        let tables1 = engine.get_tables().await;
        let tables2 = engine.get_tables().await;

        // tx1 updates row 0
        let row1 = Row::new(vec![Datum::Integer(1), Datum::Integer(200)]);
        let entry1 = WriteEntry {
            table_name: "test".to_string(),
            row_idx: 0,
            old_txid_max: u64::MAX,
            operation: WriteOp::Update(row1),
        };
        tx1.add_write(entry1);
        drop(tables1);

        // First commit succeeds
        engine.commit(&mut tx1).await.unwrap();

        // tx2 also tries to update the same row — should detect conflict
        let row2 = Row::new(vec![Datum::Integer(1), Datum::Integer(300)]);
        let entry2 = WriteEntry {
            table_name: "test".to_string(),
            row_idx: 0,
            old_txid_max: u64::MAX,
            operation: WriteOp::Update(row2),
        };
        tx2.add_write(entry2);
        drop(tables2);

        let result = engine.commit(&mut tx2).await;
        assert!(result.is_err(), "Expected conflict error but got Ok");
        assert!(matches!(result, Err(HelionError::Conflict(_))));
    }
}
