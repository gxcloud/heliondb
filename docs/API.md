# HelionDB API Reference

## Using HelionDB as a Library

Add to your `Cargo.toml`:

```toml
[dependencies]
heliondb = { git = "https://github.com/gxcloud/heliondb" }
tokio = { version = "1", features = ["full"] }
```

## Core Types

### `DatabaseEngine`

The central database engine. Create via `DatabaseEngine::open()`.

```rust
use heliondb::storage::engine::DatabaseEngine;

let mut engine = DatabaseEngine::open("./mydb".as_ref()).await?;
```

**Methods:**

| Method | Description |
|--------|-------------|
| `open(data_dir)` | Open or create database, replay WAL |
| `begin()` | Start a new MVCC transaction |
| `commit(tx)` | Commit a transaction (conflict detection + WAL) |
| `rollback(tx)` | Rollback a transaction |
| `with_read_txn(f)` | Execute read-only closure in a transaction |
| `with_write_txn(f)` | Execute write closure in a transaction |
| `get_tables()` | Get snapshot of all tables |
| `create_table(name, columns)` | Create a new table |
| `drop_table(name)` | Drop a table |
| `create_user(username, password)` | Create a new database user |
| `drop_user(username)` | Drop a user |
| `alter_user_password(username, password)` | Change user password |
| `verify_user(username, password)` | Verify login credentials |
| `grant_permission(username, table, permission)` | Grant a permission |
| `revoke_permission(username, table, permission)` | Revoke a permission |
| `check_select(username, table, columns)` | Check SELECT permission (returns `Result`) |
| `check_insert(username, table, columns)` | Check INSERT permission |
| `check_update(username, table, columns)` | Check UPDATE permission |
| `check_delete(username, table)` | Check DELETE permission |
| `shutdown()` | Write checkpoint, flush WAL, shut down |
| `get_users()` | Get all users |

### `Transaction`

Represents an MVCC transaction:

```rust
pub struct Transaction {
    pub tx_id: TransactionId,
    pub status: TransactionStatus,
    pub snapshot: Snapshot,
    pub write_set: Vec<WriteEntry>,
}
```

### `WriteEntry`

A pending write within a transaction:

```rust
pub struct WriteEntry {
    pub table_name: String,
    pub row_idx: usize,
    pub old_txid_max: u64,
    pub operation: WriteOp,
}

pub enum WriteOp {
    Insert(Row),
    Update(Row),
    Delete,
}
```

### `DataType` / `Datum`

The type system:

```rust
pub enum DataType {
    Boolean, SmallInt, Integer, BigInt, Real, Double,
    VarChar(Option<usize>), Char(Option<usize>), Text,
    Binary, Date, Time, Timestamp, TimestampTz, Uuid, Null,
}

pub enum Datum {
    Boolean(bool), SmallInt(i16), Integer(i32), BigInt(i64),
    Real(f32), Double(f64), VarChar(String), Char(String),
    Text(String), Binary(Vec<u8>), Date(NaiveDate),
    Time(NaiveTime), Timestamp(NaiveDateTime),
    TimestampTz(i64), Uuid(uuid::Uuid), Null,
}
```

### `ColumnMeta`

Column metadata:

```rust
pub struct ColumnMeta {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_unique: bool,
    pub default: Option<Datum>,
}
```

### `Row`

A row of data:

```rust
pub struct Row {
    pub values: Vec<Datum>,
}
```

### `QueryResult`

Result of executing a query:

```rust
pub struct QueryResult {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub rows_affected: u64,
}
```

### `Permission`

Permission types:

```rust
pub enum Permission {
    Select(Vec<String>),   // column-level SELECT
    Insert(Vec<String>),   // column-level INSERT
    Update(Vec<String>),   // column-level UPDATE
    Delete,                // table-level DELETE
    All,                   // all operations, all columns
}
```

## SQL Parsing

```rust
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;

let stmts = parse("SELECT * FROM users WHERE age > 18")?;
let logical_plan = plan(&stmts[0], &engine.get_tables().await)?;
```

### Supported Statements

- `CREATE TABLE name (col TYPE [PRIMARY KEY] [NOT NULL] [UNIQUE], ...)`
- `DROP TABLE [IF EXISTS] name`
- `INSERT INTO name [(cols)] VALUES (vals), ...`
- `SELECT [cols|*] FROM name [WHERE expr] [ORDER BY col [ASC|DESC]] [LIMIT n] [OFFSET n]`
- `UPDATE name SET col = val [, ...] [WHERE expr]`
- `DELETE FROM name [WHERE expr]`
- `CREATE USER name [WITH] PASSWORD '...'`
- `DROP USER [IF EXISTS] name`
- `ALTER USER name [WITH] PASSWORD '...'`
- `GRANT {SELECT[(cols)]|INSERT[(cols)]|UPDATE[(cols)]|DELETE|ALL} ON table TO user`
- `REVOKE {SELECT[(cols)]|INSERT[(cols)]|UPDATE[(cols)]|DELETE|ALL} ON table FROM user`

### Supported Expressions

| Expression | Example |
|------------|---------|
| Column reference | `age`, `users.name` |
| Literal | `42`, `3.14`, `'hello'`, `TRUE`, `NULL` |
| Comparison | `=`, `<>`, `<`, `>`, `<=`, `>=` |
| Logical | `AND`, `OR`, `NOT` |
| Arithmetic | `+`, `-`, `*`, `/` |
| `IS NULL` / `IS NOT NULL` | `name IS NULL` |
| `IN` | `age IN (20, 30, 40)` |
| `BETWEEN` | `age BETWEEN 20 AND 40` |
| `LIKE` | `name LIKE 'A%'` |
| Functions | `COUNT(*)`, `SUM(age)`, `LOWER(name)` |

### Supported Functions

| Function | Description |
|----------|-------------|
| `COUNT(expr)` | Count non-null values |
| `SUM(expr)` | Sum of numeric values |
| `AVG(expr)` | Average of numeric values |
| `MIN(expr)` | Minimum value |
| `MAX(expr)` | Maximum value |
| `LOWER(str)` / `LCASE(str)` | Convert to lowercase |
| `UPPER(str)` / `UCASE(str)` | Convert to uppercase |
| `LENGTH(str)` / `LEN(str)` | String length |
| `COALESCE(val, ...)` | First non-null value |
| `IFNULL(val, default)` | Null coalescing |
| `ABS(num)` | Absolute value |
| `ROUND(num, decimals)` | Round to decimals |

### Supported Data Types

| SQL Type | Rust Type |
|----------|-----------|
| `BOOLEAN` | `bool` |
| `SMALLINT` | `i16` |
| `INTEGER` / `INT` | `i32` |
| `BIGINT` | `i64` |
| `REAL` | `f32` |
| `DOUBLE` / `FLOAT` | `f64` |
| `VARCHAR(n)` / `VARCHAR` | `String` |
| `CHAR(n)` / `CHAR` | `String` |
| `TEXT` | `String` |
| `DATE` | `NaiveDate` |
| `TIME` | `NaiveTime` |
| `TIMESTAMP` | `NaiveDateTime` |
| `UUID` | `uuid::Uuid` |

Implicit type coercion is supported between numeric types (`INTEGER ↔ BIGINT ↔ DOUBLE`) and string types (`TEXT ↔ VARCHAR ↔ CHAR`).

## Error Handling

```rust
pub enum HelionError {
    Parse(String),              // SQL parse error
    TableNotFound(String),      // Table does not exist
    ColumnNotFound(String),     // Column does not exist
    TableAlreadyExists(String), // Table already exists
    TypeMismatch { expected, actual }, // Type mismatch
    Transaction(String),        // Transaction error
    ConstraintViolation(String),// NOT NULL violation etc.
    Conflict(u64),              // Optimistic lock conflict
    Io(String),                 // I/O error
    Serialization(String),      // Serialization error
    Protocol(String),           // Protocol error
    PermissionDenied(String),   // Access denied
    Auth(String),               // Authentication error
    Internal(String),           // Internal error
}
```

## Complete Example

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::executor::ops::{execute, execute_as};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = DatabaseEngine::open("./exampledb".as_ref()).await?;

    // Create table
    let s = &parse("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)")?;
    execute(&engine, &plan(&s[0], &[])?).await?;

    // Create user + grant permissions
    let s = &parse("CREATE USER alice WITH PASSWORD 'secret'")?;
    execute(&engine, &plan(&s[0], &[])?).await?;

    let tables = engine.get_tables().await;
    let s = &parse("GRANT SELECT(id, name) ON users TO alice")?;
    execute(&engine, &plan(&s[0], &tables)?).await?;

    // Insert as admin (no permission check)
    let s = &parse("INSERT INTO users VALUES (1, 'Alice', 'alice@example.com')")?;
    let tables = engine.get_tables().await;
    execute(&engine, &plan(&s[0], &tables)?).await?;

    // Query as alice (with permission check)
    let s = &parse("SELECT id, name FROM users")?;
    let tables = engine.get_tables().await;
    let result = execute_as(&engine, &plan(&s[0], &tables)?, Some("alice")).await?;
    println!("{:?}", result);

    // This would fail: alice doesn't have SELECT on email column
    let s = &parse("SELECT email FROM users")?;
    let tables = engine.get_tables().await;
    let result = execute_as(&engine, &plan(&s[0], &tables)?, Some("alice")).await;
    assert!(result.is_err()); // PermissionDenied!

    engine.shutdown().await?;
    Ok(())
}
```
