use serde::{Deserialize, Serialize};
use crate::error::{HelionError, Result};
use crate::storage::types::{ColumnMeta, DataType, Row};

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
}

impl Table {
    pub fn new(name: &str, columns: Vec<ColumnMeta>) -> Self {
        let pk_idx = columns.iter().position(|c| c.is_primary_key);
        Table {
            name: name.to_string(),
            columns,
            version_chains: Vec::new(),
            primary_key_idx: pk_idx,
        }
    }

    pub fn row_count(&self) -> usize {
        self.version_chains.len()
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Return all visible row versions for the given snapshot.
    /// Returns a vector of (chain_index, &Row) pairs.
    pub fn scan_visible(&self, snapshot_txid: u64, active_txns: &std::collections::BTreeSet<u64>) -> Vec<(usize, &Row)> {
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
        for (_i, (col, datum)) in self.columns.iter().zip(row.values.iter()).enumerate() {
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
        let v = RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]));
        let active = BTreeSet::new();
        assert!(is_version_visible(&v, 5, &active));
    }

    #[test]
    fn test_visibility_committed() {
        let v = RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]));
        let active = BTreeSet::new();
        assert!(is_version_visible(&v, 10, &active));
    }

    #[test]
    fn test_visibility_uncommitted() {
        let v = RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]));
        let mut active = BTreeSet::new();
        active.insert(5);
        assert!(!is_version_visible(&v, 10, &active));
    }

    #[test]
    fn test_visibility_deleted() {
        let mut v = RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]));
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
        let row = Row::new(vec![Datum::Text("not_int".into()), Datum::Text("hello".into())]);
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
        t.version_chains.push(vec![
            RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]))
        ]);
        t.version_chains.push(vec![
            RowVersion::new_insert(10, Row::new(vec![Datum::Integer(2), Datum::Text("b".into())]))
        ]);
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
        let mut chain = Vec::new();
        chain.push(RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())])));
        chain.push(RowVersion::new_delete(10, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())])));
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
        t.version_chains.push(vec![
            RowVersion::new_insert(5, Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]))
        ]);
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
        assert!(types_compatible(&DataType::Text, &DataType::VarChar(Some(100))));
        assert!(types_compatible(&DataType::Char(None), &DataType::Text));
    }

    #[test]
    fn test_types_compatible_incompatible() {
        assert!(!types_compatible(&DataType::Integer, &DataType::Text));
        assert!(!types_compatible(&DataType::Boolean, &DataType::Integer));
        assert!(!types_compatible(&DataType::Date, &DataType::Text));
    }
}
