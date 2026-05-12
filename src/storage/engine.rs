use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::{HelionError, Result};
use crate::storage::catalog::Catalog;
use crate::storage::checkpoint::{checkpoint_loop, write_checkpoint};
use crate::storage::engine_trait::normalize_engine_name;
use crate::storage::mvcc::{MvccStore, Transaction, TransactionStatus, WriteEntry, WriteOp};
use crate::storage::permissions::{Permission, PermissionStore};
use crate::storage::table::Table;
use crate::storage::types::ColumnMeta;
use crate::storage::users::{User, UserStore};
use crate::storage::wal::{replay_wal, WalRecord, WalWriter};

fn compute_max_txid(tables: &[Table]) -> Option<u64> {
    let mut max_txid: Option<u64> = None;
    for table in tables {
        for chain in &table.version_chains {
            for version in chain {
                let mut current = max_txid.unwrap_or(0);
                current = current.max(version.txid_min);
                if version.txid_max != u64::MAX {
                    current = current.max(version.txid_max);
                }
                max_txid = Some(current);
            }
        }
    }
    max_txid
}

async fn replay_users_from_wal(data_dir: &Path) -> Result<UserStore> {
    let path = data_dir.join("helion.wal");
    if !path.exists() {
        return Ok(UserStore::new());
    }

    let bytes = tokio::fs::read(&path).await?;
    let mut store = UserStore::new();
    let mut offset = 0;

    while offset + 8 <= bytes.len() {
        let len_bytes: [u8; 8] = match bytes[offset..offset + 8].try_into() {
            Ok(b) => b,
            Err(_) => break,
        };
        let record_len = u64::from_le_bytes(len_bytes) as usize;
        offset += 8;

        if offset + record_len + 4 > bytes.len() {
            break;
        }

        let data = &bytes[offset..offset + record_len];
        offset += record_len + 4;

        if let Ok(record) = bincode::deserialize::<WalRecord>(data) {
            match record {
                WalRecord::CreateUser { username, password_hash } => {
                    let _ = store.insert_user(User { username, password_hash });
                }
                WalRecord::DropUser { username } => {
                    let _ = store.drop_user(&username);
                }
                _ => {}
            }
        }
    }

    Ok(store)
}

async fn replay_permissions_from_wal(data_dir: &Path) -> Result<PermissionStore> {
    let path = data_dir.join("helion.wal");
    if !path.exists() {
        return Ok(PermissionStore::new());
    }

    let bytes = tokio::fs::read(&path).await?;
    let mut store = PermissionStore::new();
    let mut offset = 0;

    while offset + 8 <= bytes.len() {
        let len_bytes: [u8; 8] = match bytes[offset..offset + 8].try_into() {
            Ok(b) => b,
            Err(_) => break,
        };
        let record_len = u64::from_le_bytes(len_bytes) as usize;
        offset += 8;

        if offset + record_len + 4 > bytes.len() {
            break;
        }

        let data = &bytes[offset..offset + record_len];
        offset += record_len + 4;

        if let Ok(record) = bincode::deserialize::<WalRecord>(data) {
            match record {
                WalRecord::Grant { username, table, permission } => {
                    store.grant(&username, &table, permission);
                }
                WalRecord::Revoke { username, table, permission } => {
                    store.revoke(&username, &table, &permission);
                }
                _ => {}
            }
        }
    }

    Ok(store)
}

pub struct DatabaseEngine {
    pub(crate) tables: Arc<RwLock<Vec<Table>>>,
    catalog: Catalog,
    pub(crate) mvcc: MvccStore,
    wal_writer: Arc<Mutex<WalWriter>>,
    data_dir: std::path::PathBuf,
    cancel: CancellationToken,
    pub(crate) users: RwLock<UserStore>,
    pub(crate) permissions: RwLock<PermissionStore>,
}

impl DatabaseEngine {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        Self::open_with_default_engine(data_dir, "memory").await
    }

    pub async fn open_with_default_engine(data_dir: &Path, default_engine: &str) -> Result<Self> {
        if !data_dir.exists() {
            std::fs::create_dir_all(data_dir)?;
        }

        let tables = replay_wal(data_dir).await?;
        let max_txid = compute_max_txid(&tables);
        info!("Replayed WAL: {} tables restored (max txid: {:?})", tables.len(), max_txid);

        let catalog = Catalog::open(data_dir, default_engine).await?;
        let users = replay_users_from_wal(data_dir).await?;
        let permissions = replay_permissions_from_wal(data_dir).await?;
        let wal_writer = Arc::new(Mutex::new(WalWriter::open(data_dir).await?));

        let engine = DatabaseEngine {
            tables: Arc::new(RwLock::new(tables)),
            catalog,
            mvcc: MvccStore::with_start_id(max_txid.map(|id| id + 1).unwrap_or(1)),
            wal_writer,
            data_dir: data_dir.to_path_buf(),
            cancel: CancellationToken::new(),
            users: RwLock::new(users),
            permissions: RwLock::new(permissions),
        };

        engine.restore_tables_to_engines().await?;
        engine.bootstrap_default_user().await?;
        engine.spawn_checkpoint_loop();

        Ok(engine)
    }

    async fn restore_tables_to_engines(&self) -> Result<()> {
        let tables = self.tables.read().await.clone();
        for table in tables {
            let engine_name = self
                .catalog
                .table_engine_name(&table.name)
                .unwrap_or_else(|| self.catalog.default_engine());
            let engine = self.catalog.get_engine(&engine_name)?;
            if let Err(e) = engine.restore_table(table).await {
                warn!("failed to restore table into engine {}: {}", engine_name, e);
            }
        }
        Ok(())
    }

    async fn bootstrap_default_user(&self) -> Result<()> {
        let users = self.users.read().await;
        if users.user_count() != 0 {
            return Ok(());
        }
        drop(users);

        if let Ok(password) = std::env::var("HELION_PASSWORD") {
            let mut users = self.users.write().await;
            users.create_user("helion", &password)?;
            let mut perms = self.permissions.write().await;
            for table in self.tables.read().await.iter() {
                perms.grant("helion", &table.name, Permission::All);
            }
            info!("Created default admin user 'helion' from HELION_PASSWORD");
        }
        Ok(())
    }

    fn spawn_checkpoint_loop(&self) {
        let checkpoint_tables = self.tables.clone();
        let checkpoint_wal = self.wal_writer.clone();
        let checkpoint_data_dir = self.data_dir.clone();
        let checkpoint_cancel = self.cancel.clone();
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
    }

    pub fn begin(&self) -> Transaction {
        let tx = self.mvcc.begin_transaction();
        debug!("BEGIN transaction {}", tx.tx_id);
        tx
    }

    pub async fn commit(&self, tx: &mut Transaction) -> Result<()> {
        if tx.status != TransactionStatus::InProgress {
            return Err(HelionError::Transaction(format!(
                "Transaction {} is not in progress",
                tx.tx_id
            )));
        }

        let tables_guard = self.tables.read().await;
        let tables_snapshot: HashMap<String, Table> = tables_guard
            .iter()
            .cloned()
            .map(|t| (t.name.clone(), t))
            .collect();
        drop(tables_guard);

        if let Err(conflicting_tx) = self.mvcc.check_conflicts(tx, &tables_snapshot) {
            self.mvcc.rollback_transaction(tx);
            tx.status = TransactionStatus::Aborted;
            debug!("Transaction {} aborted (conflict with {})", tx.tx_id, conflicting_tx);
            return Err(HelionError::Conflict(tx.tx_id));
        }

        let changes = MvccStore::apply_write_set(&tx.write_set, &tables_snapshot, tx.tx_id);

        let mut engine_changes: HashMap<String, Vec<(usize, crate::storage::table::RowVersion)>> = HashMap::new();
        for entry in &tx.write_set {
            let change_list = engine_changes.entry(entry.table_name.clone()).or_default();
            match &entry.operation {
                WriteOp::Insert(row) => {
                    change_list.push((entry.row_idx, crate::storage::table::RowVersion::new_insert(tx.tx_id, row.clone())));
                }
                WriteOp::Update(row) => {
                    change_list.push((entry.row_idx, crate::storage::table::RowVersion::new_update(tx.tx_id, row.clone())));
                }
                WriteOp::Delete => {
                    if let Some(table) = tables_snapshot.get(&entry.table_name) {
                        if entry.row_idx < table.version_chains.len() {
                            if let Some(latest) = table.version_chains[entry.row_idx].last() {
                                change_list.push((entry.row_idx, crate::storage::table::RowVersion::new_delete(tx.tx_id, latest.row.clone())));
                            }
                        }
                    }
                }
            }
        }

        for (table_name, changes) in engine_changes {
            let engine_name = self
                .catalog
                .table_engine_name(&table_name)
                .unwrap_or_else(|| self.catalog.default_engine());
            let engine = self.catalog.get_engine(&engine_name)?;
            engine.apply_write_set(&table_name, changes).await?;
        }

        {
            let mut tables = self.tables.write().await;
            for (table_name, new_chains) in changes {
                if let Some(table) = tables.iter_mut().find(|t| t.name == table_name) {
                    table.version_chains = new_chains;
                }
            }
        }

        {
            let mut wal = self.wal_writer.lock().await;
            for entry in &tx.write_set {
                match &entry.operation {
                    WriteOp::Insert(row) => {
                        wal.append(WalRecord::Insert {
                            table: entry.table_name.clone(),
                            row: row.clone(),
                            txid: tx.tx_id,
                        }).await?;
                    }
                    WriteOp::Update(new_row) => {
                        wal.append(WalRecord::Update {
                            table: entry.table_name.clone(),
                            row_idx: entry.row_idx,
                            new_row: new_row.clone(),
                            txid: tx.tx_id,
                        }).await?;
                    }
                    WriteOp::Delete => {
                        wal.append(WalRecord::Delete {
                            table: entry.table_name.clone(),
                            row_idx: entry.row_idx,
                            txid: tx.tx_id,
                        }).await?;
                    }
                }
            }
            wal.append(WalRecord::Commit { txid: tx.tx_id }).await?;
        }

        self.mvcc.commit_transaction(tx);
        tx.status = TransactionStatus::Committed;
        debug!("COMMIT transaction {}", tx.tx_id);
        Ok(())
    }

    pub fn rollback(&self, tx: &mut Transaction) {
        if tx.status != TransactionStatus::InProgress {
            return;
        }
        self.mvcc.rollback_transaction(tx);
        tx.status = TransactionStatus::Aborted;
        debug!("ROLLBACK transaction {}", tx.tx_id);
    }

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

    pub async fn get_tables(&self) -> Vec<Table> {
        self.tables.read().await.clone()
    }

    pub async fn create_table(&self, name: &str, columns: Vec<ColumnMeta>, engine_name: Option<&str>) -> Result<()> {
        {
            let tables = self.tables.read().await;
            if tables.iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
                return Err(HelionError::TableAlreadyExists(name.to_string()));
            }
        }

        let engine_name = engine_name.map(normalize_engine_name).unwrap_or_else(|| self.catalog.default_engine());
        self.catalog.create_table(name, columns.clone(), Some(&engine_name)).await?;

        {
            let mut tables = self.tables.write().await;
            tables.push(Table::new(name, columns.clone()));
        }

        if let Err(err) = self.wal_writer.lock().await.append(WalRecord::CreateTable {
            name: name.to_string(),
            columns,
        }).await {
            let _ = self.catalog.drop_table(name).await;
            self.tables.write().await.retain(|t| !t.name.eq_ignore_ascii_case(name));
            return Err(err);
        }

        debug!("Created table '{}'", name);
        Ok(())
    }

    pub async fn drop_table(&self, name: &str) -> Result<()> {
        let snapshot = {
            let tables = self.tables.read().await;
            tables.iter().find(|t| t.name.eq_ignore_ascii_case(name)).cloned()
        }.ok_or_else(|| HelionError::TableNotFound(name.to_string()))?;

        let engine_name = self
            .catalog
            .table_engine_name(name)
            .unwrap_or_else(|| self.catalog.default_engine());

        self.catalog.drop_table(name).await?;
        self.tables.write().await.retain(|t| !t.name.eq_ignore_ascii_case(name));

        if let Err(err) = self.wal_writer.lock().await.append(WalRecord::DropTable {
            name: name.to_string(),
        }).await {
            let _ = self.catalog.create_table(&snapshot.name, snapshot.columns.clone(), Some(&engine_name)).await;
            let _ = self.catalog.restore_table(snapshot.clone()).await;
            self.tables.write().await.push(snapshot);
            return Err(err);
        }

        self.permissions.write().await.remove_table(name);
        Ok(())
    }

    pub async fn alter_table_engine(&self, name: &str, target_engine: &str) -> Result<()> {
        self.catalog.migrate_table(name, target_engine).await
    }

    pub async fn create_user(&self, username: &str, password: &str) -> Result<()> {
        let mut users = self.users.write().await;
        users.create_user(username, password)?;
        let user = users.get_user(username).unwrap().clone();
        self.wal_writer.lock().await.append(WalRecord::CreateUser {
            username: user.username,
            password_hash: user.password_hash,
        }).await?;
        Ok(())
    }

    pub async fn drop_user(&self, username: &str) -> Result<()> {
        self.users.write().await.drop_user(username)?;
        self.wal_writer.lock().await.append(WalRecord::DropUser {
            username: username.to_string(),
        }).await?;
        self.permissions.write().await.remove_user(username);
        Ok(())
    }

    pub async fn alter_user_password(&self, username: &str, new_password: &str) -> Result<()> {
        let mut users = self.users.write().await;
        users.update_password(username, new_password)?;
        let user = users.get_user(username).unwrap().clone();
        self.wal_writer.lock().await.append(WalRecord::CreateUser {
            username: user.username,
            password_hash: user.password_hash,
        }).await?;
        Ok(())
    }

    pub async fn verify_user(&self, username: &str, password: &str) -> bool {
        self.users.read().await.verify_login(username, password)
    }

    pub async fn user_exists(&self, username: &str) -> bool {
        self.users.read().await.user_exists(username)
    }

    pub fn get_user_count(&self) -> usize {
        0
    }

    pub async fn has_permission(&self, username: &str, table: &str, columns: &[&str]) -> bool {
        self.permissions.read().await.can_select(username, table, columns)
    }

    pub async fn get_users(&self) -> Vec<User> {
        self.users.read().await.all_users().to_vec()
    }

    pub async fn grant_permission(&self, username: &str, table: &str, permission: Permission) -> Result<()> {
        if !self.user_exists(username).await {
            return Err(HelionError::Auth(format!("User '{}' does not exist", username)));
        }
        let tables = self.tables.read().await;
        if !tables.iter().any(|t| t.name.eq_ignore_ascii_case(table)) {
            return Err(HelionError::TableNotFound(table.to_string()));
        }
        drop(tables);

        self.permissions.write().await.grant(username, table, permission.clone());
        self.wal_writer.lock().await.append(WalRecord::Grant {
            username: username.to_string(),
            table: table.to_string(),
            permission,
        }).await?;
        Ok(())
    }

    pub async fn revoke_permission(&self, username: &str, table: &str, permission: &Permission) -> Result<()> {
        self.permissions.write().await.revoke(username, table, permission);
        self.wal_writer.lock().await.append(WalRecord::Revoke {
            username: username.to_string(),
            table: table.to_string(),
            permission: permission.clone(),
        }).await?;
        Ok(())
    }

    pub async fn check_select(&self, username: &str, table: &str, columns: &[&str]) -> Result<()> {
        if self.permissions.read().await.can_select(username, table, columns) {
            Ok(())
        } else {
            Err(HelionError::PermissionDenied(format!(
                "User '{}' does not have SELECT permission on '{}' for columns {:?}",
                username, table, columns
            )))
        }
    }

    pub async fn check_insert(&self, username: &str, table: &str, columns: &[&str]) -> Result<()> {
        if self.permissions.read().await.can_insert(username, table, columns) {
            Ok(())
        } else {
            Err(HelionError::PermissionDenied(format!(
                "User '{}' does not have INSERT permission on '{}' for columns {:?}",
                username, table, columns
            )))
        }
    }

    pub async fn check_update(&self, username: &str, table: &str, columns: &[&str]) -> Result<()> {
        if self.permissions.read().await.can_update(username, table, columns) {
            Ok(())
        } else {
            Err(HelionError::PermissionDenied(format!(
                "User '{}' does not have UPDATE permission on '{}' for columns {:?}",
                username, table, columns
            )))
        }
    }

    pub async fn check_delete(&self, username: &str, table: &str) -> Result<()> {
        if self.permissions.read().await.can_delete(username, table) {
            Ok(())
        } else {
            Err(HelionError::PermissionDenied(format!(
                "User '{}' does not have DELETE permission on '{}'",
                username, table
            )))
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        info!("Shutting down database engine...");
        self.cancel.cancel();
        write_checkpoint(&self.data_dir, &self.tables, &self.wal_writer).await?;
        self.catalog.flush().await?;
        self.wal_writer.lock().await.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::{DataType, Datum, Row};
    use tempfile::TempDir;

    async fn setup_engine() -> (DatabaseEngine, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        (engine, dir)
    }

    #[tokio::test]
    async fn test_create_table() {
        let (engine, _dir) = setup_engine().await;
        engine.create_table("users", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
        assert_eq!(engine.get_tables().await.len(), 1);
    }

    #[tokio::test]
    async fn test_drop_table() {
        let (engine, _dir) = setup_engine().await;
        engine.create_table("users", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
        engine.drop_table("users").await.unwrap();
        assert!(engine.get_tables().await.is_empty());
    }

    #[tokio::test]
    async fn test_restart_from_wal() {
        let dir = tempfile::tempdir().unwrap();
        {
            let engine = DatabaseEngine::open(dir.path()).await.unwrap();
            engine.create_table("users", vec![ColumnMeta::new("id", DataType::Integer)], None).await.unwrap();
            engine.with_write_txn(|_tx, tables| {
                let table = tables.iter().find(|t| t.name == "users").unwrap();
                Ok(vec![WriteEntry {
                    table_name: "users".to_string(),
                    row_idx: table.row_count(),
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Insert(Row::new(vec![Datum::Integer(1)])),
                }])
            }).await.unwrap();
        }

        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        assert_eq!(engine.get_tables().await[0].row_count(), 1);
    }

    #[tokio::test]
    async fn test_alter_table_engine() {
        let (engine, _dir) = setup_engine().await;
        engine.create_table("items", vec![ColumnMeta::new("id", DataType::Integer)], Some("memory")).await.unwrap();
        engine.alter_table_engine("items", "disk").await.unwrap();
        assert_eq!(engine.catalog.table_engine_name("items"), Some("disk".to_string()));
    }
}
