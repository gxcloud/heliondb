use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// SELECT on specific columns (empty = all columns)
    Select(Vec<String>),
    /// INSERT on specific columns (empty = all columns)
    Insert(Vec<String>),
    /// UPDATE on specific columns (empty = all columns)
    Update(Vec<String>),
    /// DELETE (table-level)
    Delete,
    /// ALL (all operations, all columns)
    All,
}

#[derive(Debug, Clone)]
pub struct PermissionStore {
    /// (username, tablename) -> Vec of permissions
    grants: HashMap<(String, String), Vec<Permission>>,
}

impl PermissionStore {
    pub fn new() -> Self {
        PermissionStore {
            grants: HashMap::new(),
        }
    }

    pub fn from_grants(grants: HashMap<(String, String), Vec<Permission>>) -> Self {
        PermissionStore { grants }
    }

    pub fn all_grants(&self) -> &HashMap<(String, String), Vec<Permission>> {
        &self.grants
    }

    pub fn grant(&mut self, username: &str, table: &str, permission: Permission) {
        let key = (username.to_lowercase(), table.to_lowercase());
        let perms = self.grants.entry(key).or_default();
        // Replace existing permission of the same type (e.g., if re-granting with different columns)
        perms.retain(|p| std::mem::discriminant(p) != std::mem::discriminant(&permission));
        perms.push(permission);
    }

    pub fn revoke(&mut self, username: &str, table: &str, permission: &Permission) {
        let key = (username.to_lowercase(), table.to_lowercase());
        if let Some(perms) = self.grants.get_mut(&key) {
            perms.retain(|p| {
                if std::mem::discriminant(p) == std::mem::discriminant(permission) {
                    match (p, permission) {
                        // If both have column lists, only remove if they match
                        (Permission::Select(a), Permission::Select(b))
                        | (Permission::Insert(a), Permission::Insert(b))
                        | (Permission::Update(a), Permission::Update(b)) => a != b,
                        // For Delete/All, just remove the first match
                        _ => false,
                    }
                } else {
                    true
                }
            });
            if perms.is_empty() {
                self.grants.remove(&key);
            }
        }
    }

    pub fn revoke_all(&mut self, username: &str, table: &str) {
        let key = (username.to_lowercase(), table.to_lowercase());
        self.grants.remove(&key);
    }

    /// Check if a user has permission to SELECT specific columns on a table.
    pub fn can_select(&self, username: &str, table: &str, columns: &[&str]) -> bool {
        let key = (username.to_lowercase(), table.to_lowercase());
        let perms = match self.grants.get(&key) {
            Some(p) => p,
            None => return false,
        };

        // ALL or Delete-only grants don't help with SELECT
        for perm in perms {
            match perm {
                Permission::All => return true,
                Permission::Select(granted_cols) => {
                    if granted_cols.is_empty() {
                        return true; // all columns
                    }
                    if columns.iter().all(|c| granted_cols.iter().any(|gc| gc.eq_ignore_ascii_case(c))) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Check if a user has permission to INSERT into specific columns on a table.
    pub fn can_insert(&self, username: &str, table: &str, columns: &[&str]) -> bool {
        let key = (username.to_lowercase(), table.to_lowercase());
        let perms = match self.grants.get(&key) {
            Some(p) => p,
            None => return false,
        };

        for perm in perms {
            match perm {
                Permission::All => return true,
                Permission::Insert(granted_cols) => {
                    if granted_cols.is_empty() {
                        return true;
                    }
                    if columns.iter().all(|c| granted_cols.iter().any(|gc| gc.eq_ignore_ascii_case(c))) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Check if a user has permission to UPDATE specific columns on a table.
    pub fn can_update(&self, username: &str, table: &str, columns: &[&str]) -> bool {
        let key = (username.to_lowercase(), table.to_lowercase());
        let perms = match self.grants.get(&key) {
            Some(p) => p,
            None => return false,
        };

        for perm in perms {
            match perm {
                Permission::All => return true,
                Permission::Update(granted_cols) => {
                    if granted_cols.is_empty() {
                        return true;
                    }
                    if columns.iter().all(|c| granted_cols.iter().any(|gc| gc.eq_ignore_ascii_case(c))) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Check if a user has DELETE permission on a table.
    pub fn can_delete(&self, username: &str, table: &str) -> bool {
        let key = (username.to_lowercase(), table.to_lowercase());
        let perms = match self.grants.get(&key) {
            Some(p) => p,
            None => return false,
        };

        for perm in perms {
            match perm {
                Permission::All | Permission::Delete => return true,
                _ => {}
            }
        }
        false
    }

    /// Check if a user has ANY permission on a table (for table existence checks).
    pub fn has_any(&self, username: &str, table: &str) -> bool {
        let key = (username.to_lowercase(), table.to_lowercase());
        self.grants.contains_key(&key)
    }

    /// Remove all permissions for a user (called when user is dropped).
    pub fn remove_user(&mut self, username: &str) {
        let username_lower = username.to_lowercase();
        self.grants.retain(|k, _| k.0 != username_lower);
    }

    /// Remove all permissions for a table (called when table is dropped).
    pub fn remove_table(&mut self, table: &str) {
        let table_lower = table.to_lowercase();
        self.grants.retain(|k, _| k.1 != table_lower);
    }
}

impl Default for PermissionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grant_select_all_columns() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Select(vec![]));
        assert!(store.can_select("alice", "users", &["id", "name"]));
    }

    #[test]
    fn test_grant_select_specific_columns() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Select(vec!["id".into(), "name".into()]));
        assert!(store.can_select("alice", "users", &["id"]));
        assert!(store.can_select("alice", "users", &["name"]));
        assert!(!store.can_select("alice", "users", &["email"]));
    }

    #[test]
    fn test_grant_insert() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Insert(vec!["name".into()]));
        assert!(store.can_insert("alice", "users", &["name"]));
        assert!(!store.can_insert("alice", "users", &["email"]));
    }

    #[test]
    fn test_grant_update() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Update(vec!["email".into()]));
        assert!(store.can_update("alice", "users", &["email"]));
        assert!(!store.can_update("alice", "users", &["name"]));
    }

    #[test]
    fn test_grant_delete() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Delete);
        assert!(store.can_delete("alice", "users"));
        assert!(!store.can_select("alice", "users", &["id"]));
    }

    #[test]
    fn test_grant_all() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::All);
        assert!(store.can_select("alice", "users", &["id", "name", "email"]));
        assert!(store.can_insert("alice", "users", &["id"]));
        assert!(store.can_update("alice", "users", &["name"]));
        assert!(store.can_delete("alice", "users"));
    }

    #[test]
    fn test_revoke() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Select(vec!["id".into()]));
        assert!(store.can_select("alice", "users", &["id"]));
        store.revoke("alice", "users", &Permission::Select(vec!["id".into()]));
        assert!(!store.can_select("alice", "users", &["id"]));
    }

    #[test]
    fn test_revoke_all() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Select(vec![]));
        store.grant("alice", "users", Permission::Delete);
        assert!(store.can_select("alice", "users", &["id"]));
        store.revoke_all("alice", "users");
        assert!(!store.can_select("alice", "users", &["id"]));
        assert!(!store.can_delete("alice", "users"));
    }

    #[test]
    fn test_remove_user() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::All);
        store.remove_user("alice");
        assert!(!store.has_any("alice", "users"));
    }

    #[test]
    fn test_remove_table() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::All);
        store.remove_table("users");
        assert!(!store.has_any("alice", "users"));
    }

    #[test]
    fn test_no_permission() {
        let store = PermissionStore::new();
        assert!(!store.can_select("alice", "users", &["id"]));
        assert!(!store.can_delete("alice", "users"));
    }

    #[test]
    fn test_case_insensitive() {
        let mut store = PermissionStore::new();
        store.grant("Alice", "Users", Permission::Select(vec!["ID".into()]));
        assert!(store.can_select("alice", "users", &["id"]));
        assert!(store.can_select("ALICE", "USERS", &["ID"]));
    }

    #[test]
    fn test_update_upgrade_from_specific_to_all() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Update(vec!["email".into()]));
        // Re-grant with all columns replaces the old one
        store.grant("alice", "users", Permission::Update(vec![]));
        assert!(store.can_update("alice", "users", &["email", "name"]));
    }

    #[test]
    fn test_select_requires_all_requested_columns() {
        let mut store = PermissionStore::new();
        store.grant("alice", "users", Permission::Select(vec!["id".into(), "name".into()]));
        // Need ALL columns to be granted
        assert!(!store.can_select("alice", "users", &["id", "email"]));
    }

    #[test]
    fn test_from_grants() {
        let mut grants = HashMap::new();
        grants.insert(
            ("alice".into(), "users".into()),
            vec![Permission::All],
        );
        let store = PermissionStore::from_grants(grants);
        assert!(store.can_select("alice", "users", &["id"]));
    }
}
