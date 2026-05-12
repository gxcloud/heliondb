use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Execute a raw SQL query
    Query { sql: String, token: u64 },
    /// Prepare a statement
    Prepare { sql: String, token: u64 },
    /// Execute a prepared statement
    Execute {
        prepared_id: u64,
        params: Vec<String>,
        token: u64,
    },
    /// Authenticate with username and password
    Auth { username: String, password: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Query result
    QueryResult {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        error: Option<String>,
    },
    /// Prepared statement ID
    Prepared { id: u64 },
    /// Error message
    Error { message: String },
    /// Authentication result
    AuthResult {
        success: bool,
        token: u64,
        error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_query() {
        let msg = ClientMessage::Query {
            sql: "SELECT 1".to_string(),
            token: 42,
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let deserialized: ClientMessage = bincode::deserialize(&bytes).unwrap();
        match deserialized {
            ClientMessage::Query { sql, token } => {
                assert_eq!(sql, "SELECT 1");
                assert_eq!(token, 42);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_serialize_auth() {
        let msg = ClientMessage::Auth {
            username: "alice".into(),
            password: "secret".into(),
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let deserialized: ClientMessage = bincode::deserialize(&bytes).unwrap();
        match deserialized {
            ClientMessage::Auth { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "secret");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_serialize_auth_result() {
        let msg = ServerMessage::AuthResult {
            success: true,
            token: 123,
            error: None,
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let deserialized: ServerMessage = bincode::deserialize(&bytes).unwrap();
        match deserialized {
            ServerMessage::AuthResult {
                success,
                token,
                error,
            } => {
                assert!(success);
                assert_eq!(token, 123);
                assert!(error.is_none());
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_serialize_result() {
        let msg = ServerMessage::QueryResult {
            columns: vec!["id".to_string()],
            rows: vec![vec!["1".to_string()]],
            error: None,
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let deserialized: ServerMessage = bincode::deserialize(&bytes).unwrap();
        match deserialized {
            ServerMessage::QueryResult {
                columns,
                rows,
                error,
            } => {
                assert_eq!(columns, vec!["id"]);
                assert_eq!(rows, vec![vec!["1"]]);
                assert!(error.is_none());
            }
            _ => panic!("Wrong message type"),
        }
    }
}
