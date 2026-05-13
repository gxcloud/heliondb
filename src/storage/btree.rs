use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use serde::{Deserialize, Serialize};

use crate::error::{HelionError, Result};
use crate::storage::types::{Datum, Row};

/// Serializable metadata for an index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexMeta {
    pub name: String,
    pub columns: Vec<usize>,
    pub is_unique: bool,
}

impl IndexMeta {
    pub fn new(name: &str, columns: Vec<usize>, is_unique: bool) -> Self {
        IndexMeta {
            name: name.to_string(),
            columns,
            is_unique,
        }
    }
}

/// A B-tree backed index mapping key values to row indices.
///
/// Uses `std::collections::BTreeMap` internally for O(log n) point lookups,
/// range scans, insertions, and deletions. Non-unique indexes map each key
/// to a set of row indices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub meta: IndexMeta,
    entries: BTreeMap<Vec<Datum>, BTreeSet<usize>>,
}

impl Index {
    /// Create a new index with the given metadata.
    pub fn new(meta: IndexMeta) -> Self {
        Index {
            meta,
            entries: BTreeMap::new(),
        }
    }

    /// Create a new unique index.
    pub fn new_unique(name: &str, columns: Vec<usize>) -> Self {
        Index::new(IndexMeta::new(name, columns, true))
    }

    /// Create a new non-unique index.
    pub fn new_non_unique(name: &str, columns: Vec<usize>) -> Self {
        Index::new(IndexMeta::new(name, columns, false))
    }

    /// Return the number of distinct keys in the index.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return true if the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Extract the key values from a row for this index's columns.
    pub fn extract_key(&self, row: &Row) -> Vec<Datum> {
        self.meta
            .columns
            .iter()
            .filter_map(|&i| row.values.get(i).cloned())
            .collect()
    }

    /// Insert a row index into the index for the given key.
    ///
    /// For unique indexes, returns an error if the key already exists
    /// (even if it maps to the same row_idx — use `update` for that).
    pub fn insert(&mut self, key: &[Datum], row_idx: usize) -> Result<()> {
        if self.meta.is_unique {
            if let Some(existing) = self.entries.get(key) {
                if !existing.contains(&row_idx) {
                    let key_str: Vec<String> = key.iter().map(|d| d.display()).collect();
                    return Err(HelionError::DuplicateKey {
                        index: self.meta.name.clone(),
                        key: key_str.join(", "),
                    });
                }
            }
        }
        self.entries
            .entry(key.to_vec())
            .or_default()
            .insert(row_idx);
        Ok(())
    }

    /// Remove a row index from the index for the given key.
    pub fn remove(&mut self, key: &[Datum], row_idx: usize) {
        if let std::collections::btree_map::Entry::Occupied(mut entry) =
            self.entries.entry(key.to_vec())
        {
            entry.get_mut().remove(&row_idx);
            if entry.get().is_empty() {
                entry.remove();
            }
        }
    }

    /// Update a row's key in the index (remove old, insert new).
    pub fn update(&mut self, old_key: &[Datum], new_key: &[Datum], row_idx: usize) -> Result<()> {
        self.remove(old_key, row_idx);
        self.insert(new_key, row_idx)
    }

    /// Return all row indices for an exact key match.
    pub fn get(&self, key: &[Datum]) -> Option<&BTreeSet<usize>> {
        self.entries.get(key)
    }

    /// Check if the key exists in the index (for unique constraint checks).
    pub fn contains(&self, key: &[Datum]) -> bool {
        self.entries.contains_key(key)
    }

    /// Perform a range scan over keys that fall within the given bounds.
    ///
    /// Returns an iterator of (key, row_idx_set) pairs.
    pub fn range(
        &self,
        start: Bound<Vec<Datum>>,
        end: Bound<Vec<Datum>>,
    ) -> impl Iterator<Item = (&Vec<Datum>, &BTreeSet<usize>)> {
        self.entries.range((start, end))
    }

    /// Return all row indices whose keys are >= the given prefix.
    ///
    /// This is useful for composite index prefix matching
    /// and for range conditions like `col >= value`.
    pub fn scan_from(&self, from: Vec<Datum>) -> Vec<usize> {
        let mut results = BTreeSet::new();
        for (_, row_idxs) in self.entries.range::<Vec<Datum>, _>(from..) {
            results.extend(row_idxs);
        }
        results.into_iter().collect()
    }

    /// Return all row indices whose keys are <= the given value.
    pub fn scan_to(&self, to: Vec<Datum>) -> Vec<usize> {
        let mut results = BTreeSet::new();
        for (_, row_idxs) in self.entries.range::<Vec<Datum>, _>(..=to) {
            results.extend(row_idxs);
        }
        results.into_iter().collect()
    }

    /// Return all row indices in the index (full scan).
    pub fn all_row_idxs(&self) -> Vec<usize> {
        let mut results = BTreeSet::new();
        for row_idxs in self.entries.values() {
            results.extend(row_idxs);
        }
        results.into_iter().collect()
    }

    /// Return all entries (for building index from scratch).
    pub fn entries(&self) -> &BTreeMap<Vec<Datum>, BTreeSet<usize>> {
        &self.entries
    }

    /// Insert raw entries (used during index rebuild).
    pub fn insert_entry(&mut self, key: Vec<Datum>, row_idx: usize) {
        self.entries.entry(key).or_default().insert(row_idx);
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::Datum;

    fn make_index() -> Index {
        Index::new_unique("test_idx", vec![0])
    }

    #[test]
    fn test_new_index() {
        let idx = make_index();
        assert_eq!(idx.meta.name, "test_idx");
        assert_eq!(idx.meta.columns, vec![0]);
        assert!(idx.meta.is_unique);
        assert!(idx.is_empty());
    }

    #[test]
    fn test_insert_and_get() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        assert_eq!(idx.len(), 1);
        assert!(idx.contains(&[Datum::Integer(1)]));
        let row_idxs = idx.get(&[Datum::Integer(1)]).unwrap();
        assert!(row_idxs.contains(&0));
    }

    #[test]
    fn test_unique_index_duplicate_key() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        let result = idx.insert(&[Datum::Integer(1)], 1);
        assert!(result.is_err());
        match result.unwrap_err() {
            HelionError::DuplicateKey { index, key } => {
                assert_eq!(index, "test_idx");
                assert_eq!(key, "1");
            }
            e => panic!("Expected DuplicateKey, got: {:?}", e),
        }
    }

    #[test]
    fn test_unique_index_same_row_same_key() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        // Inserting the same key + row_idx should be a no-op (no error)
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.get(&[Datum::Integer(1)]).unwrap().len(), 1);
    }

    #[test]
    fn test_non_unique_index() {
        let mut idx = Index::new_non_unique("non_unique", vec![0]);
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.insert(&[Datum::Integer(1)], 1).unwrap();
        assert_eq!(idx.len(), 1); // one key
        let row_idxs = idx.get(&[Datum::Integer(1)]).unwrap();
        assert_eq!(row_idxs.len(), 2);
        assert!(row_idxs.contains(&0));
        assert!(row_idxs.contains(&1));
    }

    #[test]
    fn test_remove() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.insert(&[Datum::Integer(2)], 1).unwrap();
        idx.remove(&[Datum::Integer(1)], 0);
        assert_eq!(idx.len(), 1);
        assert!(!idx.contains(&[Datum::Integer(1)]));
    }

    #[test]
    fn test_remove_empty_set_cleans_up() {
        let mut idx = Index::new_non_unique("multi", vec![0]);
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.insert(&[Datum::Integer(1)], 1).unwrap();
        idx.remove(&[Datum::Integer(1)], 0);
        assert!(idx.contains(&[Datum::Integer(1)])); // key still exists
        idx.remove(&[Datum::Integer(1)], 1);
        assert!(!idx.contains(&[Datum::Integer(1)])); // key removed
    }

    #[test]
    fn test_update() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.update(&[Datum::Integer(1)], &[Datum::Integer(2)], 0)
            .unwrap();
        assert!(!idx.contains(&[Datum::Integer(1)]));
        assert!(idx.contains(&[Datum::Integer(2)]));
    }

    #[test]
    fn test_update_unique_conflict() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.insert(&[Datum::Integer(2)], 1).unwrap();
        let result = idx.update(&[Datum::Integer(1)], &[Datum::Integer(2)], 0);
        // Updating to key=2 which is already taken by row 1
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_key() {
        let idx = Index::new_unique("test", vec![0, 2]);
        let row = Row::new(vec![
            Datum::Integer(10),
            Datum::Text("ignore".into()),
            Datum::Integer(20),
        ]);
        let key = idx.extract_key(&row);
        assert_eq!(key, vec![Datum::Integer(10), Datum::Integer(20)]);
    }

    #[test]
    fn test_range_scan() {
        let mut idx = make_index();
        for i in 0i32..10 {
            idx.insert(&[Datum::Integer(i)], i as usize).unwrap();
        }
        let range: Vec<_> = idx
            .range(
                std::ops::Bound::Included(vec![Datum::Integer(3)]),
                std::ops::Bound::Included(vec![Datum::Integer(6)]),
            )
            .collect();
        assert_eq!(range.len(), 4); // keys 3,4,5,6
    }

    #[test]
    fn test_scan_from() {
        let mut idx = make_index();
        for i in 0i32..10 {
            idx.insert(&[Datum::Integer(i)], i as usize).unwrap();
        }
        let result = idx.scan_from(vec![Datum::Integer(7)]);
        assert_eq!(result.len(), 3); // 7, 8, 9
    }

    #[test]
    fn test_scan_to() {
        let mut idx = make_index();
        for i in 0i32..10 {
            idx.insert(&[Datum::Integer(i)], i as usize).unwrap();
        }
        let result = idx.scan_to(vec![Datum::Integer(2)]);
        assert_eq!(result.len(), 3); // 0, 1, 2
    }

    #[test]
    fn test_all_row_idxs() {
        let mut idx = Index::new_non_unique("multi", vec![0]);
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.insert(&[Datum::Integer(1)], 2).unwrap();
        idx.insert(&[Datum::Integer(2)], 1).unwrap();
        let all = idx.all_row_idxs();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&0));
        assert!(all.contains(&1));
        assert!(all.contains(&2));
    }

    #[test]
    fn test_clear() {
        let mut idx = make_index();
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.clear();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn test_insert_entry() {
        let mut idx = Index::new_non_unique("test", vec![0]);
        idx.insert_entry(vec![Datum::Integer(1)], 0);
        idx.insert_entry(vec![Datum::Integer(1)], 1);
        assert_eq!(idx.get(&[Datum::Integer(1)]).unwrap().len(), 2);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let mut idx = Index::new_unique("serde_test", vec![0]);
        idx.insert(&[Datum::Integer(1)], 0).unwrap();
        idx.insert(&[Datum::Integer(2)], 1).unwrap();

        let bytes = bincode::serialize(&idx).unwrap();
        let deserialized: Index = bincode::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.meta.name, "serde_test");
        assert!(deserialized.contains(&[Datum::Integer(1)]));
        assert!(deserialized.contains(&[Datum::Integer(2)]));
        assert_eq!(deserialized.len(), 2);
    }

    #[test]
    fn test_composite_key() {
        let mut idx = Index::new_unique("composite", vec![0, 1]);
        idx.insert(&[Datum::Integer(1), Datum::Text("a".into())], 0)
            .unwrap();
        idx.insert(&[Datum::Integer(1), Datum::Text("b".into())], 1)
            .unwrap();
        idx.insert(&[Datum::Integer(2), Datum::Text("a".into())], 2)
            .unwrap();

        assert!(idx.contains(&[Datum::Integer(1), Datum::Text("a".into())]));
        assert!(!idx.contains(&[Datum::Integer(1), Datum::Text("c".into())]));

        // Duplicate composite key should fail
        let result = idx.insert(&[Datum::Integer(1), Datum::Text("a".into())], 3);
        assert!(result.is_err());
    }
}
