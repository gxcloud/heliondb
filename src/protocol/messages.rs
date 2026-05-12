use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Query { sql: String },
    Prepare { sql: String },
    Execute { prepared_id: u64, params: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    QueryResult { columns: Vec<String>, rows: Vec<Vec<String>>, error: Option<String> },
    Prepared { id: u64 },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_query() {
        let msg = ClientMessage::Query { sql: "SELECT 1".to_string() };
        let bytes = bincode::serialize(&msg).unwrap();
        let deserialized: ClientMessage = bincode::deserialize(&bytes).unwrap();
        match deserialized {
            ClientMessage::Query { sql } => assert_eq!(sql, "SELECT 1"),
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
            ServerMessage::QueryResult { columns, rows, error } => {
                assert_eq!(columns, vec!["id"]);
                assert_eq!(rows, vec![vec!["1"]]);
                assert!(error.is_none());
            }
            _ => panic!("Wrong message type"),
        }
    }
}
