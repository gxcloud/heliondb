use thiserror::Error;

pub type Result<T> = std::result::Result<T, HelionError>;

#[derive(Error, Debug, Clone)]
pub enum HelionError {
    #[error("SQL parse error: {0}")]
    Parse(String),

    #[error("Table '{0}' not found")]
    TableNotFound(String),

    #[error("Column '{0}' not found")]
    ColumnNotFound(String),

    #[error("Table '{0}' already exists")]
    TableAlreadyExists(String),

    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: String, actual: String },

    #[error("Transaction error: {0}")]
    Transaction(String),

    #[error("Constraint violation: {0}")]
    ConstraintViolation(String),

    #[error("Optimistic lock conflict: transaction {0} aborted due to concurrent write")]
    Conflict(u64),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<std::io::Error> for HelionError {
    fn from(e: std::io::Error) -> Self {
        HelionError::Io(e.to_string())
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for HelionError {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        HelionError::Internal(e.to_string())
    }
}

impl From<sqlparser::parser::ParserError> for HelionError {
    fn from(e: sqlparser::parser::ParserError) -> Self {
        HelionError::Parse(e.to_string())
    }
}


