use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;
use crate::storage::types::Row;
use crate::storage::table::{RowVersion, Table};

pub type TransactionId = u64;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub txid: TransactionId,
    pub active: Arc<BTreeSet<TransactionId>>,
}

impl Snapshot {
    pub fn new(txid: TransactionId, active: BTreeSet<TransactionId>) -> Self {
        Snapshot {
            txid,
            active: Arc::new(active),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionStatus {
    InProgress,
    Committed,
    Aborted,
}

#[derive(Debug, Clone)]
pub enum WriteOp {
    Insert(Row),
    Update(Row),
    Delete,
}

#[derive(Debug, Clone)]
pub struct WriteEntry {
    pub table_name: String,
    pub row_idx: usize,
    pub old_txid_max: u64,
    pub operation: WriteOp,
}

#[derive(Debug)]
pub struct Transaction {
    pub tx_id: TransactionId,
    pub status: TransactionStatus,
    pub snapshot: Snapshot,
    pub write_set: Vec<WriteEntry>,
}

impl Transaction {
    pub fn new(tx_id: TransactionId, snapshot: Snapshot) -> Self {
        Transaction {
            tx_id,
            status: TransactionStatus::InProgress,
            snapshot,
            write_set: Vec::new(),
        }
    }

    pub fn add_write(&mut self, entry: WriteEntry) {
        self.write_set.push(entry);
    }
}

pub struct MvccStore {
    next_txid: AtomicU64,
    active_txns: RwLock<BTreeSet<TransactionId>>,
}

impl MvccStore {
    pub fn new() -> Self {
        MvccStore::with_start_id(1)
    }

    /// Create an MVCC store starting transaction IDs from a given value.
    /// Used after WAL recovery to ensure new snapshots can see replayed changes.
    pub fn with_start_id(start_id: TransactionId) -> Self {
        let mut active = BTreeSet::new();
        active.insert(0);
        MvccStore {
            next_txid: AtomicU64::new(start_id.max(1)),
            active_txns: RwLock::new(active),
        }
    }

    /// Allocate a new transaction ID, register it as active, and create a snapshot.
    /// The snapshot captures all transactions that were active before this one began.
    pub fn begin_transaction(&self) -> Transaction {
        let tx_id = self.next_txid.fetch_add(1, Ordering::SeqCst);
        let active = self.active_txns.read().clone();
        self.add_active(tx_id);
        let snapshot = Snapshot::new(tx_id, active);
        Transaction::new(tx_id, snapshot)
    }

    /// Called AFTER writes have been successfully applied and WAL appended.
    pub fn commit_transaction(&self, tx: &Transaction) {
        let mut active = self.active_txns.write();
        active.remove(&tx.tx_id);
    }

    /// Called on rollback — just remove from active set.
    pub fn rollback_transaction(&self, tx: &Transaction) {
        let mut active = self.active_txns.write();
        active.remove(&tx.tx_id);
    }

    /// Track a new active transaction (called after allocating txid).
    pub fn add_active(&self, tx_id: TransactionId) {
        let mut active = self.active_txns.write();
        active.insert(tx_id);
    }

    pub fn next_txid(&self) -> TransactionId {
        self.next_txid.fetch_add(1, Ordering::SeqCst)
    }

    /// Check for optimistic concurrency conflicts.
    /// Returns Ok(()) if no conflicts, Err with the conflicting transaction ID if found.
    /// A conflict occurs when the latest committed version of any row we're modifying
    /// was created by a transaction that was active at our snapshot time —
    /// meaning it committed between our snapshot and our commit.
    pub fn check_conflicts(
        &self,
        tx: &Transaction,
        tables: &HashMap<String, Table>,
    ) -> std::result::Result<(), u64> {
        for entry in &tx.write_set {
            if !tables.contains_key(&entry.table_name) {
                continue;
            }
            if let Some(table) = tables.get(&entry.table_name) {
                if entry.row_idx < table.version_chains.len() {
                    let chain = &table.version_chains[entry.row_idx];
                    // Find the latest committed version (the one with txid_max == u64::MAX)
                    for version in chain.iter().rev() {
                        if version.txid_max == u64::MAX {
                            // This is the current version. If its creator was active
                            // at our snapshot time, it committed after us — conflict!
                            if tx.snapshot.active.contains(&version.txid_min) {
                                return Err(version.txid_min);
                            }
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply write set to the tables (assumes conflicts already checked).
    pub fn apply_write_set(
        write_set: &[WriteEntry],
        tables: &HashMap<String, Table>,
        tx_id: TransactionId,
    ) -> HashMap<String, Vec<Vec<RowVersion>>> {
        let mut changes: HashMap<String, Vec<(usize, RowVersion)>> = HashMap::new();

        for entry in write_set {
            let change_list = changes.entry(entry.table_name.clone()).or_default();
            match &entry.operation {
                WriteOp::Insert(row) => {
                    change_list.push((entry.row_idx, RowVersion::new_insert(tx_id, row.clone())));
                }
                WriteOp::Update(row) => {
                    change_list.push((entry.row_idx, RowVersion::new_update(tx_id, row.clone())));
                }
                WriteOp::Delete => {
                    // We need the old row data for the tombstone
                    if let Some(table) = tables.get(&entry.table_name) {
                        if entry.row_idx < table.version_chains.len() {
                            if let Some(latest) = table.version_chains[entry.row_idx].last() {
                                let old_row = latest.row.clone();
                                change_list.push((entry.row_idx, RowVersion::new_delete(tx_id, old_row)));
                            }
                        }
                    }
                }
            }
        }

        let mut result: HashMap<String, Vec<Vec<RowVersion>>> = HashMap::new();
        for (table_name, table_changes) in changes {
            if let Some(table) = tables.get(&table_name) {
                let mut new_chains = table.version_chains.clone();
                for (row_idx, version) in table_changes {
                    if row_idx < new_chains.len() {
                        // Mark the old latest version as done
                        if let Some(old_latest) = new_chains[row_idx].last_mut() {
                            old_latest.txid_max = tx_id;
                        }
                        // Add the new version
                        new_chains[row_idx].push(version);
                    } else {
                        // New row (INSERT) - add to chain
                        new_chains.push(vec![version]);
                    }
                }
                result.insert(table_name, new_chains);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::*;

    fn setup_tables() -> HashMap<String, Table> {
        let mut tables = HashMap::new();
        let columns = vec![
            ColumnMeta::new("id", DataType::Integer).primary_key(),
            ColumnMeta::new("name", DataType::Text),
        ];
        let mut table = Table::new("users", columns);
        // Add one row from tx 10
        table.version_chains.push(vec![
            RowVersion::new_insert(10, Row::new(vec![Datum::Integer(1), Datum::Text("Alice".into())]))
        ]);
        tables.insert("users".to_string(), table);
        tables
    }

    #[test]
    fn test_begin_transaction() {
        let store = MvccStore::new();
        let tx = store.begin_transaction();
        assert_eq!(tx.tx_id, 1);
        assert_eq!(tx.status, TransactionStatus::InProgress);
    }

    #[test]
    fn test_transaction_ids_increment() {
        let store = MvccStore::new();
        let tx1 = store.begin_transaction();
        let tx2 = store.begin_transaction();
        assert_eq!(tx1.tx_id, 1);
        assert_eq!(tx2.tx_id, 2);
    }

    #[test]
    fn test_snapshot_excludes_active() {
        let store = MvccStore::new();
        let tx1 = store.begin_transaction();
        let tx2 = store.begin_transaction();
        // tx2's snapshot should not include tx2, but should include tx1
        assert!(!tx2.snapshot.active.contains(&tx2.tx_id));
        assert!(tx2.snapshot.active.contains(&tx1.tx_id));
    }

    #[test]
    fn test_no_conflict_empty_write_set() {
        let store = MvccStore::new();
        let tx = store.begin_transaction();
        let tables = setup_tables();
        assert!(store.check_conflicts(&tx, &tables).is_ok());
    }

    #[test]
    fn test_conflict_detected() {
        let store = MvccStore::new();
        let mut tables = setup_tables();

        // Register tx 20 as an active concurrent transaction
        store.add_active(20);

        // Simulate: tx 20 (still active) modified row 0
        tables.get_mut("users").unwrap().version_chains[0].push(
            RowVersion::new_update(20, Row::new(vec![Datum::Integer(1), Datum::Text("Bob".into())]))
        );

        // Now tx 30 begins — its snapshot should include active tx 20
        let mut tx = store.begin_transaction();
        assert!(tx.snapshot.active.contains(&20),
            "tx 30's snapshot should include active tx 20");

        tx.add_write(WriteEntry {
            table_name: "users".to_string(),
            row_idx: 0,
            old_txid_max: u64::MAX,
            operation: WriteOp::Update(Row::new(vec![Datum::Integer(1), Datum::Text("Charlie".into())])),
        });

        // tx 20 is still active and modified the same row — conflict!
        let result = store.check_conflicts(&tx, &tables);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 20);
    }

    #[test]
    fn test_apply_write_set_insert() {
        let tables = setup_tables();
        let _store = MvccStore::new();
        let tx = Transaction::new(
            99,
            Snapshot::new(99, BTreeSet::new()),
        );

        let entries = vec![WriteEntry {
            table_name: "users".to_string(),
            row_idx: 1, // new row, idx >= current len
            old_txid_max: u64::MAX,
            operation: WriteOp::Insert(Row::new(vec![Datum::Integer(2), Datum::Text("Bob".into())])),
        }];

        let changes = MvccStore::apply_write_set(&entries, &tables, tx.tx_id);
        if let Some(new_chains) = changes.get("users") {
            assert_eq!(new_chains.len(), 2); // original 1 + new 1
            assert_eq!(new_chains[1].len(), 1);
            assert_eq!(new_chains[1][0].txid_min, 99);
        } else {
            panic!("Expected changes for 'users'");
        }
    }

    #[test]
    fn test_apply_write_set_update() {
        let tables = setup_tables();
        let _store = MvccStore::new();
        let tx = Transaction::new(
            99,
            Snapshot::new(99, BTreeSet::new()),
        );

        let entries = vec![WriteEntry {
            table_name: "users".to_string(),
            row_idx: 0,
            old_txid_max: u64::MAX,
            operation: WriteOp::Update(Row::new(vec![Datum::Integer(1), Datum::Text("Updated".into())])),
        }];

        let changes = MvccStore::apply_write_set(&entries, &tables, tx.tx_id);
        if let Some(new_chains) = changes.get("users") {
            assert_eq!(new_chains[0].len(), 2); // original + update
            // Original version should have txid_max = 99
            assert_eq!(new_chains[0][0].txid_max, 99);
            // New version should have txid_min = 99
            assert_eq!(new_chains[0][1].txid_min, 99);
        } else {
            panic!("Expected changes for 'users'");
        }
    }

    #[test]
    fn test_commit_removes_from_active() {
        let store = MvccStore::new();
        let tx = store.begin_transaction();
        assert_eq!(tx.tx_id, 1);

        // Tx should be in active set
        {
            let active = store.active_txns.read();
            assert!(active.contains(&1));
        }

        store.commit_transaction(&tx);

        // Tx should no longer be in active set
        {
            let active = store.active_txns.read();
            assert!(!active.contains(&1));
        }
    }

    #[test]
    fn test_rollback_removes_from_active() {
        let store = MvccStore::new();
        let tx = store.begin_transaction();
        store.rollback_transaction(&tx);
        let active = store.active_txns.read();
        assert!(!active.contains(&tx.tx_id));
    }

    #[test]
    fn test_apply_delete_in_chain() {
        let tables = setup_tables();
        let _store = MvccStore::new();
        let tx = Transaction::new(99, Snapshot::new(99, BTreeSet::new()));

        let entries = vec![WriteEntry {
            table_name: "users".to_string(),
            row_idx: 0,
            old_txid_max: u64::MAX,
            operation: WriteOp::Delete,
        }];

        let changes = MvccStore::apply_write_set(&entries, &tables, tx.tx_id);
        if let Some(chains) = changes.get("users") {
            assert_eq!(chains[0].len(), 2);
            assert!(chains[0][1].is_deleted);
        }
    }

    #[test]
    fn test_snapshot_does_not_include_self() {
        let store = MvccStore::new();
        let tx = store.begin_transaction();
        assert!(!tx.snapshot.active.contains(&tx.tx_id));
    }

    #[test]
    fn test_next_txid_increases() {
        let store = MvccStore::new();
        let id1 = store.next_txid();
        let id2 = store.next_txid();
        assert_eq!(id2, id1 + 1);
    }

    #[test]
    fn test_concurrent_begin_transactions() {
        let store = MvccStore::new();
        let tx1 = store.begin_transaction();
        let tx2 = store.begin_transaction();

        assert_ne!(tx1.tx_id, tx2.tx_id);
        // tx2's snapshot should include tx1 (which is still active)
        assert!(tx2.snapshot.active.contains(&tx1.tx_id));
        // tx1's snapshot should NOT include tx2 (tx2 started after tx1)
        assert!(!tx1.snapshot.active.contains(&tx2.tx_id));
    }
}
