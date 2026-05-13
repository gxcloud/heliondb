use crate::error::{HelionError, Result};
use crate::storage::btree::{Index, IndexMeta};
use crate::storage::types::{ColumnMeta, DataType, Row};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowVersion {
    pub txid_min: u64,
    pub txid_max: u64,
    pub row: Row,
    pub is_deleted: bool,
}

impl RowVersion {
    pub fn new_insert(txid: u64, row: Row) -> Self {
        RowVersion {
            txid_min: txid,
            txid_max: u64::MAX,
            row,
            is_deleted: false,
        }
    }

    pub fn new_update(txid: u64, row: Row) -> Self {
        RowVersion {
            txid_min: txid,
            txid_max: u64::MAX,
            row,
            is_deleted: false,
        }
    }

    pub fn new_delete(txid: u64, row: Row) -> Self {
        RowVersion {
            txid_min: txid,
            txid_max: u64::MAX,
            row,
            is_deleted: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub columns: Vec<ColumnMeta>,
    pub(crate) version_chains: Vec<Vec<RowVersion>>,
    pub primary_key_idx: Option<usize>,
    pub(crate) indexes: Vec<Index>,
}

impl Table {
    pub fn new(name: &str, columns: Vec<ColumnMeta>) -> Self {
        let pk_idx = columns.iter().position(|c| c.is_primary_key);
        let mut table = Table {
            name: name.to_string(),
            columns,
            version_chains: Vec::new(),
            primary_key_idx: pk_idx,
            indexes: Vec::new(),
        };
        table.build_pk_index();
        table.build_unique_indexes();
        table
    }

    pub fn row_count(&self) -> usize {
        self.version_chains.len()
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Return all visible row versions for the given snapshot.
    /// Returns a vector of (chain_index, &Row) pairs.
    pub fn scan_visible(
        &self,
        snapshot_txid: u64,
        active_txns: &std::collections::BTreeSet<u64>,
    ) -> Vec<(usize, &Row)> {
        self.version_chains
            .iter()
            .enumerate()
            .filter_map(|(idx, chain)| {
                self.latest_visible(chain, snapshot_txid, active_txns)
                    .map(|rv| (idx, &rv.row))
            })
            .collect()
    }

    /// Find the latest visible version in a chain for a given snapshot.
    fn latest_visible<'a>(
        &self,
        chain: &'a [RowVersion],
        snapshot_txid: u64,
        active_txns: &std::collections::BTreeSet<u64>,
    ) -> Option<&'a RowVersion> {
        let mut best: Option<&RowVersion> = None;
        for version in chain.iter().rev() {
            if is_version_visible(version, snapshot_txid, active_txns) {
                best = Some(version);
                break;
            }
        }
        best.filter(|v| !v.is_deleted)
    }

    /// Get the latest visible version for a specific chain index.
    pub fn get_visible_version(
        &self,
        chain_idx: usize,
        snapshot_txid: u64,
        active_txns: &std::collections::BTreeSet<u64>,
    ) -> Option<&RowVersion> {
        self.version_chains
            .get(chain_idx)
            .and_then(|chain| self.latest_visible(chain, snapshot_txid, active_txns))
    }

    // ── Index Management ──────────────────────────────────────────

    /// Build a unique index on the primary key column(s), if any.
    pub fn build_pk_index(&mut self) {
        if let Some(pk_col) = self.primary_key_idx {
            let pk_name = format!("pk_{}", self.name);
            if !self.indexes.iter().any(|i| i.meta.name == pk_name) {
                self.indexes
                    .push(Index::new_unique(&pk_name, vec![pk_col]));
            }
        }
    }

    /// Build unique indexes for columns marked as `is_unique`.
    pub fn build_unique_indexes(&mut self) {
        for (i, col) in self.columns.iter().enumerate() {
            if col.is_unique && !col.is_primary_key {
                let uq_name = format!("uq_{}_{}", self.name, col.name);
                if !self.indexes.iter().any(|idx| idx.meta.name == uq_name) {
                    self.indexes.push(Index::new_unique(&uq_name, vec![i]));
                }
            }
        }
    }

    /// Rebuild all indexes by scanning visible rows.
    /// Used after WAL replay or checkpoint restore.
    pub fn rebuild_indexes(&mut self) {
        for index in &mut self.indexes {
            index.clear();
        }
        // Scan committed rows (snapshot_txid = u64::MAX, no active txns)
        let active = std::collections::BTreeSet::new();
        for (row_idx, row) in self.scan_visible(u64::MAX, &active) {
            for index in &mut self.indexes {
                let key = index.extract_key(row);
                index.insert_entry(key, row_idx);
            }
        }
    }

    /// Add a user-defined index.
    pub fn add_index(
        &mut self,
        name: &str,
        columns: Vec<usize>,
        is_unique: bool,
    ) -> Result<()> {
        if self.indexes.iter().any(|i| i.meta.name == name) {
            return Err(HelionError::IndexAlreadyExists(name.to_string()));
        }
        let mut index = if is_unique {
            Index::new_unique(name, columns)
        } else {
            Index::new_non_unique(name, columns)
        };
        // Populate from existing data
        let active = std::collections::BTreeSet::new();
        for (row_idx, row) in self.scan_visible(u64::MAX, &active) {
            let key = index.extract_key(row);
            if is_unique {
                index.insert(&key, row_idx).map_err(|_| {
                    HelionError::DuplicateKey {
                        index: name.to_string(),
                        key: key.iter().map(|d| d.display()).collect::<Vec<_>>().join(", "),
                    }
                })?;
            } else {
                index.insert_entry(key, row_idx);
            }
        }
        self.indexes.push(index);
        Ok(())
    }

    /// Drop a named index.
    pub fn drop_index(&mut self, name: &str) -> Result<()> {
        let pos = self
            .indexes
            .iter()
            .position(|i| i.meta.name == name)
            .ok_or_else(|| HelionError::IndexNotFound(name.to_string()))?;
        self.indexes.remove(pos);
        Ok(())
    }

    /// Find an index by name.
    pub fn find_index(&self, name: &str) -> Option<&Index> {
        self.indexes.iter().find(|i| i.meta.name == name)
    }

    /// Check unique constraints for a write entry against the table's unique indexes.
    /// Returns Ok if no violations, Err otherwise.
    pub fn check_unique_constraints(
        &self,
        is_insert: bool,
        row_idx: usize,
        row: &Row,
    ) -> Result<()> {
        for index in &self.indexes {
            if !index.meta.is_unique {
                continue;
            }
            let key = index.extract_key(row);
            if key.iter().all(|d| d.is_null()) {
                continue;
            }
            if let Some(existing) = index.get(&key) {
                if existing.len() == 1 && existing.contains(&row_idx) {
                    continue;
                }
                if is_insert || !existing.contains(&row_idx) {
                    let key_str: Vec<String> = key.iter().map(|d| d.display()).collect();
                    return Err(HelionError::DuplicateKey {
                        index: index.meta.name.clone(),
                        key: key_str.join(", "),
                    });
                }
            }
        }
        Ok(())
    }

    /// Apply a write entry's changes to all indexes after commit.
    pub fn apply_index_changes(
        &mut self,
        operation: &crate::storage::mvcc::WriteOp,
        old_row: Option<&Row>,
        row_idx: usize,
    ) -> Result<()> {
        match operation {
            crate::storage::mvcc::WriteOp::Insert(row) => {
                for index in &mut self.indexes {
                    let key = index.extract_key(row);
                    index.insert(&key, row_idx).map_err(|e| {
                        HelionError::Internal(format!(
                            "Index insert failed after commit: {}",
                            e
                        ))
                    })?;
                }
            }
            crate::storage::mvcc::WriteOp::Update(new_row) => {
                for index in &mut self.indexes {
                    let old_key = index.extract_key(
                        old_row
                            .ok_or_else(|| {
                                HelionError::Internal("Missing old row for index update".into())
                            })?,
                    );
                    let new_key = index.extract_key(new_row);
                    index.update(&old_key, &new_key, row_idx).map_err(|e| {
                        HelionError::Internal(format!(
                            "Index update failed after commit: {}",
                            e
                        ))
                    })?;
                }
            }
            crate::storage::mvcc::WriteOp::Delete => {
                if let Some(old_row) = old_row {
                    for index in &mut self.indexes {
                        let key = index.extract_key(old_row);
                        index.remove(&key, row_idx);
                    }
                }
            }
        }
        Ok(())
    }

    /// Validate that a row matches the table schema.
    pub fn validate_row(&self, row: &Row) -> Result<()> {
        if row.values.len() != self.columns.len() {
            return Err(HelionError::Internal(format!(
                "Row has {} columns but table '{}' has {} columns",
                row.values.len(),
                self.name,
                self.columns.len()
            )));
        }
        for (col, datum) in self.columns.iter().zip(row.values.iter()) {
            if datum.is_null() {
                if !col.nullable {
                    return Err(HelionError::ConstraintViolation(format!(
                        "Column '{}' cannot be null",
                        col.name
                    )));
                }
                continue;
            }
            let actual = datum.data_type();
            if !types_compatible(&col.data_type, &actual) {
                return Err(HelionError::TypeMismatch {
                    expected: col.data_type.to_string(),
                    actual: actual.to_string(),
                });
            }
        }
        Ok(())
    }
}

/// Check if a version is visible to a transaction with the given snapshot.
/// Standard MVCC: version is visible if txid_min <= snapshot_txid < txid_max
/// and txid_min is not in the active set (i.e., committed before snapshot).
/// u64::MAX is treated as "infinity" (version is current / never deleted).
pub fn is_version_visible(
    version: &RowVersion,
    snapshot_txid: u64,
    active_txns: &std::collections::BTreeSet<u64>,
) -> bool {
    if version.txid_min > snapshot_txid {
        return false;
    }
    if version.txid_max != u64::MAX && version.txid_max <= snapshot_txid {
        return false;
    }
    if active_txns.contains(&version.txid_min) && version.txid_min != snapshot_txid {
        return false;
    }
    true
}

/// Check if two types are compatible (allow implicit casts).
fn types_compatible(expected: &DataType, actual: &DataType) -> bool {
    if expected == actual {
        return true;
    }
    // Numeric types are compatible
    match (expected, actual) {
        (DataType::Integer, DataType::BigInt)
        | (DataType::BigInt, DataType::Integer)
        | (DataType::Integer, DataType::SmallInt)
        | (DataType::SmallInt, DataType::Integer)
        | (DataType::BigInt, DataType::SmallInt)
        | (DataType::SmallInt, DataType::BigInt)
        | (DataType::Double, DataType::Integer)
        | (DataType::Double, DataType::BigInt)
        | (DataType::Integer, DataType::Double)
        | (DataType::BigInt, DataType::Double)
        | (DataType::Real, DataType::Double)
        | (DataType::Double, DataType::Real) => true,
        // String types are compatible
        (DataType::VarChar(_), DataType::Text)
        | (DataType::Text, DataType::VarChar(_))
        | (DataType::Char(_), DataType::Text)
        | (DataType::Text, DataType::Char(_))
        | (DataType::VarChar(_), DataType::Char(_))
        | (DataType::Char(_), DataType::VarChar(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::*;
    use std::collections::BTreeSet;

    fn test_table() -> Table {
        let columns = vec![
            ColumnMeta::new("id", DataType::Integer).primary_key(),
            ColumnMeta::new("name", DataType::VarChar(None)),
        ];
        Table::new("test", columns)
    }

    #[test]
    fn test_table_creation() {
        let t = test_table();
        assert_eq!(t.name, "test");
        assert_eq!(t.columns.len(), 2);
        assert_eq!(t.primary_key_idx, Some(0));
    }

    #[test]
    fn test_column_index_case_insensitive() {
        let t = test_table();
        assert_eq!(t.column_index("ID"), Some(0));
        assert_eq!(t.column_index("Name"), Some(1));
        assert_eq!(t.column_index("nonexistent"), None);
    }

    #[test]
    fn test_visibility_own_transaction() {
        let v = RowVersion::new_insert(
            5,
            Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
        );
        let active = BTreeSet::new();
        assert!(is_version_visible(&v, 5, &active));
    }

    #[test]
    fn test_visibility_committed() {
        let v = RowVersion::new_insert(
            5,
            Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
        );
        let active = BTreeSet::new();
        assert!(is_version_visible(&v, 10, &active));
    }

    #[test]
    fn test_visibility_uncommitted() {
        let v = RowVersion::new_insert(
            5,
            Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
        );
        let mut active = BTreeSet::new();
        active.insert(5);
        assert!(!is_version_visible(&v, 10, &active));
    }

    #[test]
    fn test_visibility_deleted() {
        let mut v = RowVersion::new_insert(
            5,
            Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
        );
        v.txid_max = 10;
        // tx 5 wrote it, tx 10 deleted/overwrote it
        let active = BTreeSet::new();
        assert!(is_version_visible(&v, 7, &active));
        assert!(!is_version_visible(&v, 10, &active));
        assert!(!is_version_visible(&v, 15, &active));
    }

    #[test]
    fn test_validate_row_ok() {
        let t = test_table();
        let row = Row::new(vec![Datum::Integer(1), Datum::VarChar("hello".into())]);
        assert!(t.validate_row(&row).is_ok());
    }

    #[test]
    fn test_validate_row_wrong_type() {
        let t = test_table();
        let row = Row::new(vec![
            Datum::Text("not_int".into()),
            Datum::Text("hello".into()),
        ]);
        assert!(t.validate_row(&row).is_err());
    }

    #[test]
    fn test_validate_row_null_not_nullable() {
        let mut t = test_table();
        t.columns[1].nullable = false;
        let row = Row::new(vec![Datum::Integer(1), Datum::Null]);
        assert!(t.validate_row(&row).is_err());
    }

    #[test]
    fn test_validate_row_wrong_length() {
        let t = test_table();
        let row = Row::new(vec![Datum::Integer(1)]);
        assert!(t.validate_row(&row).is_err());
    }

    #[test]
    fn test_scan_visible() {
        let mut t = test_table();
        t.version_chains.push(vec![RowVersion::new_insert(
            5,
            Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
        )]);
        t.version_chains.push(vec![RowVersion::new_insert(
            10,
            Row::new(vec![Datum::Integer(2), Datum::Text("b".into())]),
        )]);
        let active = BTreeSet::new();

        // Tx 7 should only see row 0 (tx 5 committed)
        let visible = t.scan_visible(7, &active);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1.get(0), Some(&Datum::Integer(1)));

        // Tx 15 should see both rows
        let visible = t.scan_visible(15, &active);
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn test_latest_visible_skips_deleted() {
        let chain = vec![
            RowVersion::new_insert(
                5,
                Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
            ),
            RowVersion::new_delete(
                10,
                Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
            ),
        ];
        let active = BTreeSet::new();
        let t = test_table();
        assert!(t.latest_visible(&chain, 15, &active).is_none());
        assert!(t.latest_visible(&chain, 7, &active).is_some());
    }

    #[test]
    fn test_scan_visible_empty_table() {
        let t = test_table();
        let active = BTreeSet::new();
        let visible = t.scan_visible(100, &active);
        assert_eq!(visible.len(), 0);
    }

    #[test]
    fn test_scan_visible_all_uncommitted() {
        let mut t = test_table();
        t.version_chains.push(vec![RowVersion::new_insert(
            5,
            Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]),
        )]);
        let mut active = BTreeSet::new();
        active.insert(5);
        // tx 5 is still active, so no rows should be visible
        let visible = t.scan_visible(10, &active);
        assert_eq!(visible.len(), 0);
    }

    #[test]
    fn test_multiple_updates_same_row() {
        let mut t = test_table();
        let row = Row::new(vec![Datum::Integer(1), Datum::Text("v1".into())]);
        t.version_chains.push(vec![RowVersion::new_insert(5, row)]);

        // Update twice
        let v2 = Row::new(vec![Datum::Integer(1), Datum::Text("v2".into())]);
        t.version_chains[0].push(RowVersion::new_update(10, v2));
        let v3 = Row::new(vec![Datum::Integer(1), Datum::Text("v3".into())]);
        t.version_chains[0].push(RowVersion::new_update(15, v3));

        let active = BTreeSet::new();

        // Should see latest version at tx 20
        let visible = t.scan_visible(20, &active);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1.get(1), Some(&Datum::Text("v3".into())));

        // Should see v2 at tx 12
        let visible = t.scan_visible(12, &active);
        assert_eq!(visible[0].1.get(1), Some(&Datum::Text("v2".into())));

        // Should see v1 at tx 7
        let visible = t.scan_visible(7, &active);
        assert_eq!(visible[0].1.get(1), Some(&Datum::Text("v1".into())));
    }

    #[test]
    fn test_get_visible_version_out_of_range() {
        let t = test_table();
        let active = BTreeSet::new();
        assert!(t.get_visible_version(0, 100, &active).is_none());
        assert!(t.get_visible_version(999, 100, &active).is_none());
    }

    #[test]
    fn test_types_compatible_self() {
        assert!(types_compatible(&DataType::Integer, &DataType::Integer));
        assert!(types_compatible(&DataType::Text, &DataType::Text));
    }

    #[test]
    fn test_types_compatible_numeric() {
        assert!(types_compatible(&DataType::Integer, &DataType::BigInt));
        assert!(types_compatible(&DataType::BigInt, &DataType::Integer));
        assert!(types_compatible(&DataType::Double, &DataType::Integer));
    }

    #[test]
    fn test_types_compatible_string() {
        assert!(types_compatible(&DataType::VarChar(None), &DataType::Text));
        assert!(types_compatible(
            &DataType::Text,
            &DataType::VarChar(Some(100))
        ));
        assert!(types_compatible(&DataType::Char(None), &DataType::Text));
    }

    #[test]
    fn test_types_compatible_incompatible() {
        assert!(!types_compatible(&DataType::Integer, &DataType::Text));
        assert!(!types_compatible(&DataType::Boolean, &DataType::Integer));
        assert!(!types_compatible(&DataType::Date, &DataType::Text));
    }

    // ── Index Tests ────────────────────────────────────────────────

    #[test]
    fn test_table_auto_creates_pk_index() {
        let t = test_table();
        assert!(t.primary_key_idx == Some(0));
        assert!(t.indexes.iter().any(|i| i.meta.name == "pk_test"));
        let pk_idx = t.indexes.iter().find(|i| i.meta.name == "pk_test").unwrap();
        assert!(pk_idx.meta.is_unique);
        assert_eq!(pk_idx.meta.columns, vec![0]);
    }

    #[test]
    fn test_table_auto_creates_unique_indexes() {
        let columns = vec![
            ColumnMeta::new("id", DataType::Integer).primary_key(),
            ColumnMeta::new("email", DataType::Text).not_null(),
        ];
        let mut t = Table::new("users", columns);
        // No unique columns besides PK
        assert_eq!(t.indexes.len(), 1);
        assert_eq!(t.indexes[0].meta.name, "pk_users");

        // Add a unique column
        t.columns.push(
            ColumnMeta::new("username", DataType::VarChar(None))
                .not_null()
                .primary_key(),
        );
        // Actually, let's test with a non-PK unique column
        let columns = vec![
            ColumnMeta::new("id", DataType::Integer).primary_key(),
            ColumnMeta::new("email", DataType::Text).not_null(),
        ];
        let mut t2 = Table::new("users2", columns);
        // Mark email as unique after creation
        t2.columns[1].is_unique = true;
        t2.build_unique_indexes();
        assert!(t2.indexes.iter().any(|i| i.meta.name == "uq_users2_email"));
    }

    #[test]
    fn test_rebuild_indexes() {
        let mut t = test_table();
        // Add some data
        t.version_chains
            .push(vec![RowVersion::new_insert(1, Row::new(vec![Datum::Integer(10), Datum::Text("a".into())]))]);
        t.version_chains
            .push(vec![RowVersion::new_insert(1, Row::new(vec![Datum::Integer(20), Datum::Text("b".into())]))]);

        // Clear indexes and rebuild
        for idx in &mut t.indexes {
            idx.clear();
        }
        assert!(t.indexes[0].is_empty());
        t.rebuild_indexes();
        assert_eq!(t.indexes[0].len(), 2);
    }

    #[test]
    fn test_add_index_and_populate() {
        let mut t = test_table();
        t.version_chains
            .push(vec![RowVersion::new_insert(5, Row::new(vec![Datum::Integer(10), Datum::Text("alice".into())]))]);
        t.version_chains
            .push(vec![RowVersion::new_insert(5, Row::new(vec![Datum::Integer(20), Datum::Text("bob".into())]))]);

        t.add_index("idx_name", vec![1], false).unwrap();
        let idx = t.find_index("idx_name").unwrap();
        assert!(!idx.meta.is_unique);
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn test_add_index_unique_violation() {
        let mut t = test_table();
        t.version_chains
            .push(vec![RowVersion::new_insert(5, Row::new(vec![Datum::Integer(10), Datum::Text("dup".into())]))]);
        t.version_chains
            .push(vec![RowVersion::new_insert(5, Row::new(vec![Datum::Integer(20), Datum::Text("dup".into())]))]);

        let result = t.add_index("uq_name", vec![1], true);
        assert!(result.is_err());
        match result.unwrap_err() {
            HelionError::DuplicateKey { index, key } => {
                assert_eq!(index, "uq_name");
            }
            e => panic!("Expected DuplicateKey, got: {:?}", e),
        }
    }

    #[test]
    fn test_drop_index() {
        let mut t = test_table();
        assert_eq!(t.indexes.len(), 1);
        t.drop_index("pk_test").unwrap();
        assert_eq!(t.indexes.len(), 0);
    }

    #[test]
    fn test_drop_index_not_found() {
        let mut t = test_table();
        let result = t.drop_index("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_add_index_duplicate_name() {
        let mut t = test_table();
        let result = t.add_index("pk_test", vec![1], false);
        assert!(result.is_err());
        match result.unwrap_err() {
            HelionError::IndexAlreadyExists(_) => {}
            e => panic!("Expected IndexAlreadyExists, got: {:?}", e),
        }
    }

    #[test]
    fn test_check_unique_constraints_insert_ok() {
        let t = test_table();
        let row = Row::new(vec![Datum::Integer(1), Datum::Text("new".into())]);
        assert!(t.check_unique_constraints(true, 0, &row).is_ok());
    }

    #[test]
    fn test_check_unique_constraints_insert_conflict() {
        let mut t = test_table();
        // Pre-populate an entry in the PK index
        t.version_chains
            .push(vec![RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("existing".into())]))]);
        t.rebuild_indexes();

        // Try to insert a row with same PK
        let row = Row::new(vec![Datum::Integer(1), Datum::Text("duplicate".into())]);
        let result = t.check_unique_constraints(true, 1, &row);
        assert!(result.is_err());
        match result.unwrap_err() {
            HelionError::DuplicateKey { index, .. } => {
                assert_eq!(index, "pk_test");
            }
            e => panic!("Expected DuplicateKey, got: {:?}", e),
        }
    }

    #[test]
    fn test_apply_index_changes_insert() {
        let mut t = test_table();
        let row = Row::new(vec![Datum::Integer(1), Datum::Text("hello".into())]);
        t.apply_index_changes(&crate::storage::mvcc::WriteOp::Insert(row.clone()), None, 0)
            .unwrap();
        assert_eq!(t.indexes[0].len(), 1);
        assert!(t.indexes[0].contains(&[Datum::Integer(1)]));
    }

    #[test]
    fn test_apply_index_changes_delete() {
        let mut t = test_table();
        let row = Row::new(vec![Datum::Integer(1), Datum::Text("hello".into())]);
        t.apply_index_changes(&crate::storage::mvcc::WriteOp::Insert(row.clone()), None, 0)
            .unwrap();
        t.apply_index_changes(&crate::storage::mvcc::WriteOp::Delete, Some(&row), 0)
            .unwrap();
        assert!(!t.indexes[0].contains(&[Datum::Integer(1)]));
    }

    #[test]
    fn test_serialize_table_with_indexes() {
        let t = test_table();
        let bytes = bincode::serialize(&t).unwrap();
        let deserialized: Table = bincode::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.indexes.len(), 1);
        assert_eq!(deserialized.indexes[0].meta.name, "pk_test");
    }
}
