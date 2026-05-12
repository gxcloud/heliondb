use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Simple session token manager for authenticated connections.
pub struct SessionManager {
    next_token: AtomicU64,
    /// token -> username
    active_sessions: parking_lot::RwLock<HashMap<u64, String>>,
}

impl SessionManager {
    pub fn new() -> Self {
        SessionManager {
            next_token: AtomicU64::new(1),
            active_sessions: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    /// Create a new session for a user, returning a unique token.
    pub fn create_session(&self, username: &str) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::SeqCst);
        let mut sessions = self.active_sessions.write();
        sessions.insert(token, username.to_string());
        token
    }

    /// Verify a session token and return the associated username.
    pub fn verify_token(&self, token: u64) -> Option<String> {
        let sessions = self.active_sessions.read();
        sessions.get(&token).cloned()
    }

    /// Remove a session (on disconnect or logout).
    pub fn remove_session(&self, token: u64) {
        let mut sessions = self.active_sessions.write();
        sessions.remove(&token);
    }

    pub fn active_count(&self) -> usize {
        let sessions = self.active_sessions.read();
        sessions.len()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_verify_session() {
        let mgr = SessionManager::new();
        let token = mgr.create_session("alice");
        assert_eq!(mgr.verify_token(token), Some("alice".to_string()));
    }

    #[test]
    fn test_invalid_token() {
        let mgr = SessionManager::new();
        assert_eq!(mgr.verify_token(999), None);
    }

    #[test]
    fn test_remove_session() {
        let mgr = SessionManager::new();
        let token = mgr.create_session("alice");
        assert!(mgr.verify_token(token).is_some());
        mgr.remove_session(token);
        assert!(mgr.verify_token(token).is_none());
    }

    #[test]
    fn test_unique_tokens() {
        let mgr = SessionManager::new();
        let t1 = mgr.create_session("alice");
        let t2 = mgr.create_session("bob");
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_active_count() {
        let mgr = SessionManager::new();
        assert_eq!(mgr.active_count(), 0);
        mgr.create_session("alice");
        assert_eq!(mgr.active_count(), 1);
        mgr.create_session("bob");
        assert_eq!(mgr.active_count(), 2);
    }
}
