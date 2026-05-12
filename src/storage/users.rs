use serde::{Deserialize, Serialize};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand::rngs::OsRng;

use crate::error::{HelionError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub username: String,
    pub password_hash: String,
}

impl User {
    pub fn new(username: &str, password: &str) -> Result<Self> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| HelionError::Internal(format!("Password hashing error: {}", e)))?
            .to_string();
        Ok(User {
            username: username.to_string(),
            password_hash: hash,
        })
    }

    pub fn verify_password(&self, password: &str) -> bool {
        let parsed_hash = match PasswordHash::new(&self.password_hash) {
            Ok(h) => h,
            Err(_) => return false,
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
    }

    pub fn set_password(&mut self, new_password: &str) -> Result<()> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(new_password.as_bytes(), &salt)
            .map_err(|e| HelionError::Internal(format!("Password hashing error: {}", e)))?
            .to_string();
        self.password_hash = hash;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct UserStore {
    users: Vec<User>,
}

impl UserStore {
    pub fn new() -> Self {
        UserStore { users: Vec::new() }
    }

    pub fn from_users(users: Vec<User>) -> Self {
        UserStore { users }
    }

    pub fn all_users(&self) -> &[User] {
        &self.users
    }

    pub fn get_user(&self, username: &str) -> Option<&User> {
        self.users.iter().find(|u| u.username.eq_ignore_ascii_case(username))
    }

    pub fn get_user_mut(&mut self, username: &str) -> Option<&mut User> {
        self.users.iter_mut().find(|u| u.username.eq_ignore_ascii_case(username))
    }

    pub fn user_exists(&self, username: &str) -> bool {
        self.users.iter().any(|u| u.username.eq_ignore_ascii_case(username))
    }

    pub fn create_user(&mut self, username: &str, password: &str) -> Result<()> {
        if self.user_exists(username) {
            return Err(HelionError::Internal(format!("User '{}' already exists", username)));
        }
        let user = User::new(username, password)?;
        self.users.push(user);
        Ok(())
    }

    pub fn drop_user(&mut self, username: &str) -> Result<()> {
        let initial_len = self.users.len();
        self.users.retain(|u| !u.username.eq_ignore_ascii_case(username));
        if self.users.len() == initial_len {
            return Err(HelionError::Internal(format!("User '{}' not found", username)));
        }
        Ok(())
    }

    pub fn update_password(&mut self, username: &str, new_password: &str) -> Result<()> {
        let user = self.get_user_mut(username)
            .ok_or_else(|| HelionError::Internal(format!("User '{}' not found", username)))?;
        user.set_password(new_password)
    }

    pub fn verify_login(&self, username: &str, password: &str) -> bool {
        self.get_user(username)
            .map(|u| u.verify_password(password))
            .unwrap_or(false)
    }

    pub fn user_count(&self) -> usize {
        self.users.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_user() {
        let mut store = UserStore::new();
        store.create_user("alice", "password123").unwrap();
        assert_eq!(store.user_count(), 1);
        assert!(store.user_exists("alice"));
    }

    #[test]
    fn test_duplicate_user() {
        let mut store = UserStore::new();
        store.create_user("alice", "pw1").unwrap();
        let result = store.create_user("alice", "pw2");
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_password() {
        let mut store = UserStore::new();
        store.create_user("alice", "correct_password").unwrap();
        assert!(store.verify_login("alice", "correct_password"));
        assert!(!store.verify_login("alice", "wrong_password"));
        assert!(!store.verify_login("nonexistent", "anything"));
    }

    #[test]
    fn test_drop_user() {
        let mut store = UserStore::new();
        store.create_user("alice", "pw").unwrap();
        store.drop_user("alice").unwrap();
        assert_eq!(store.user_count(), 0);
    }

    #[test]
    fn test_drop_nonexistent() {
        let mut store = UserStore::new();
        let result = store.drop_user("nobody");
        assert!(result.is_err());
    }

    #[test]
    fn test_update_password() {
        let mut store = UserStore::new();
        store.create_user("alice", "old_password").unwrap();
        store.update_password("alice", "new_password").unwrap();
        assert!(!store.verify_login("alice", "old_password"));
        assert!(store.verify_login("alice", "new_password"));
    }

    #[test]
    fn test_user_case_insensitive() {
        let mut store = UserStore::new();
        store.create_user("Alice", "pw").unwrap();
        assert!(store.user_exists("alice"));
        assert!(store.user_exists("ALICE"));
        assert!(store.verify_login("ALICE", "pw"));
    }

    #[test]
    fn test_from_users() {
        let users = vec![User::new("alice", "pw").unwrap()];
        let store = UserStore::from_users(users);
        assert_eq!(store.user_count(), 1);
        assert!(store.verify_login("alice", "pw"));
    }

    #[test]
    fn test_password_hash_differs() {
        let user1 = User::new("alice", "password").unwrap();
        let user2 = User::new("alice", "password").unwrap();
        // Each hash should be unique due to random salt
        assert_ne!(user1.password_hash, user2.password_hash);
    }
}
