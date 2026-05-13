//! HelionDB — A networked SQL database with pluggable storage engines, MVCC snapshot isolation,
//! async WAL persistence, and QUIC transport.
//!
//! # Architecture
//!
//! HelionDB is built as a layered architecture. The primary deployment is as a networked database
//! server, but it can also be embedded as a library:
//!
//! - **Network Layer**: QUIC-based server (via `quinn`) with a custom binary protocol, supporting
//!   hundreds of concurrent connections with true concurrent reads via MVCC snapshot isolation.
//! - **SQL Layer**: PostgreSQL-compatible SQL parser (via `sqlparser-rs`) and a logical query planner.
//! - **Executor Layer**: Expression evaluator and physical operators for executing queries against
//!   storage engines.
//! - **Storage Layer**: Pluggable storage engines (`disk` with append-only delta persistence,
//!   `memory` for ephemeral workloads) with MVCC snapshot isolation, version-chain row storage,
//!   and optimistic concurrency control. All mutations are persisted to a write-ahead log (WAL)
//!   with configurable durability and crash recovery.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use heliondb::storage::engine::DatabaseEngine;
//! use heliondb::sql::parser::parse;
//! use heliondb::sql::planner::plan;
//! use heliondb::executor::ops::execute;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut engine = DatabaseEngine::open("./mydb".as_ref()).await?;
//!
//! // Create a table
//! let sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)";
//! let stmts = parse(sql)?;
//! let create_plan = plan(&stmts[0], &engine.get_tables().await)?;
//! execute(&engine, &create_plan).await?;
//!
//! // Insert data
//! let sql = "INSERT INTO users VALUES (1, 'Alice', 30)";
//! let stmts = parse(sql)?;
//! let insert_plan = plan(&stmts[0], &engine.get_tables().await)?;
//! execute(&engine, &insert_plan).await?;
//!
//! // Query data
//! let sql = "SELECT * FROM users WHERE age > 25";
//! let stmts = parse(sql)?;
//! let select_plan = plan(&stmts[0], &engine.get_tables().await)?;
//! let result = execute(&engine, &select_plan).await?;
//! println!("{:?}", result);
//!
//! engine.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # SQL Support
//!
//! HelionDB supports a PostgreSQL-compatible subset of SQL:
//!
//! - **DDL**: `CREATE TABLE`, `DROP TABLE`, `CREATE INDEX`, `DROP INDEX`
//! - **DML**: `SELECT` (with `WHERE`, `ORDER BY`, `LIMIT`, `OFFSET`), `INSERT`, `UPDATE`, `DELETE`
//! - **Expressions**: Comparisons (`=`, `<>`, `<`, `>`, `<=`, `>=`), logical (`AND`, `OR`, `NOT`),
//!   arithmetic (`+`, `-`, `*`, `/`), `IS NULL`, `IS NOT NULL`, `IN`, `BETWEEN`, `LIKE`
//! - **Functions**: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `LOWER`, `UPPER`, `LENGTH`,
//!   `COALESCE`, `IFNULL`, `ABS`, `ROUND`, `UUIDV7`
//! - **Indexes**: B-tree indexes with O(log n) point lookups and range scans
//! - **Constraints**: `PRIMARY KEY` and `UNIQUE` enforced at commit time via indexes
//! - **Data Types**: `INTEGER`, `BIGINT`, `SMALLINT`, `REAL`, `DOUBLE`, `VARCHAR`, `CHAR`,
//!   `TEXT`, `BOOLEAN`, `DATE`, `TIME`, `TIMESTAMP`, `UUID`, `UUIDV7`, `U_SMALLINT`,
//!   `U_INTEGER`, `U_BIGINT`

pub mod client;
pub mod config;
pub mod error;
pub mod executor;
pub mod protocol;
pub mod server;
pub mod sql;
pub mod storage;

pub use error::HelionError;
pub use executor::eval::evaluate;
pub use executor::ops::{execute, execute_as, QueryResult};
pub use protocol::auth::SessionManager;
pub use sql::parser::{
    parse, BinaryOperator, Expression, HelionStatement, SelectColumn, UnaryOperator,
};
pub use sql::planner::{plan, LogicalPlan};
pub use storage::btree::{Index, IndexMeta};
pub use storage::engine::DatabaseEngine;
pub use storage::mvcc::{Transaction, TransactionStatus, WriteEntry, WriteOp};
pub use storage::permissions::{Permission, PermissionStore};
pub use storage::table::Table;
pub use storage::types::{ColumnMeta, DataType, Datum, Row};
pub use storage::users::{User, UserStore};
