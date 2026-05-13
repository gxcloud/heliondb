# API Reference

## Using HelionDB as a Library

```toml
[dependencies]
heliondb = { git = "https://github.com/gxcloud/heliondb" }
tokio = { version = "1", features = ["full"] }
```

## Core Types

### `DatabaseEngine`

```rust
use heliondb::storage::engine::DatabaseEngine;

let mut engine = DatabaseEngine::open_with_default_engine("./mydb".as_ref(), "memory").await?;
```

**Methods:**

| Method | Description |
|--------|-------------|
| `open(data_dir)` | Open or create database, replay WAL |
| `open_with_default_engine(data_dir, engine)` | Open with a default table engine |
| `begin()` | Start a new MVCC transaction |
| `commit(tx)` | Commit a transaction |
| `rollback(tx)` | Roll back a transaction |
| `with_read_txn(f)` | Execute a read-only closure in a transaction |
| `with_write_txn(f)` | Execute a write closure in a transaction |
| `get_tables()` | Get snapshot of all tables |
| `create_table(name, columns, engine)` | Create a new table |
| `alter_table_engine(name, engine)` | Migrate a table to another engine |
| `drop_table(name)` | Drop a table |
| `explain(sql)` | Run `EXPLAIN` and return planned output |
| `explain_analyze(sql)` | Run `EXPLAIN ANALYZE` and execute the statement |
| `create_user(username, password)` | Create a new database user |
| `drop_user(username)` | Drop a user |
| `alter_user_password(username, password)` | Change a user password |
| `verify_user(username, password)` | Verify login credentials |
| `grant_permission(username, table, permission)` | Grant a permission |
| `revoke_permission(username, table, permission)` | Revoke a permission |
| `check_select(username, table, columns)` | Check `SELECT` permission |
| `check_insert(username, table, columns)` | Check `INSERT` permission |
| `check_update(username, table, columns)` | Check `UPDATE` permission |
| `check_delete(username, table)` | Check `DELETE` permission |
| `shutdown()` | Write checkpoint, flush WAL, shut down |
| `get_users()` | Get all users |

### `Transaction`

```rust
pub struct Transaction {
    pub tx_id: TransactionId,
    pub status: TransactionStatus,
    pub snapshot: Snapshot,
    pub write_set: Vec<WriteEntry>,
}
```

### `WriteEntry`

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

```rust
pub enum DataType {
    Boolean, SmallInt, UnsignedSmallInt, Integer, UnsignedInteger, BigInt, UnsignedBigInt, Real, Double,
    VarChar(Option<usize>), Char(Option<usize>), Text,
    Binary, Date, Time, Timestamp, TimestampTz, Uuid, UuidV7, Null,
}

pub enum Datum {
    Boolean(bool), SmallInt(i16), UnsignedSmallInt(u16), Integer(i32), UnsignedInteger(u32), BigInt(i64), UnsignedBigInt(u64),
    Real(f32), Double(f64), VarChar(String), Char(String),
    Text(String), Binary(Vec<u8>), Date(NaiveDate),
    Time(NaiveTime), Timestamp(NaiveDateTime),
    TimestampTz(i64), Uuid(uuid::Uuid), UuidV7([u8; 16]), Null,
}
```

### `ColumnMeta`

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

### `Index` / `IndexMeta`

```rust
pub struct Index {
    pub meta: IndexMeta,
    // entries: BTreeMap<Vec<Datum>, BTreeSet<usize>>,
}

pub struct IndexMeta {
    pub name: String,
    pub columns: Vec<usize>,
    pub is_unique: bool,
}
```

Indexes are automatically created on `PRIMARY KEY` and `UNIQUE` columns.
User-defined indexes can be added via `CREATE INDEX` SQL or programmatically:

```rust
use heliondb::storage::btree::{Index, IndexMeta};

// Create a unique index on column 0
let mut idx = Index::new_unique("my_index", vec![0]);
idx.insert(&[Datum::Integer(42)], 0)?;
assert!(idx.contains(&[Datum::Integer(42)]));

// Range scan
let results: Vec<usize> = idx.scan_from(vec![Datum::Integer(10)]);
```

## SQL Parsing

```rust
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;

let stmts = parse("SELECT * FROM users WHERE age > 18")?;
let logical_plan = plan(&stmts[0], &engine.get_tables().await)?;
```

## Supported Statements

- `CREATE TABLE name (col TYPE [PRIMARY KEY] [NOT NULL] [UNIQUE], ...) [ENGINE = memory|disk]`
- `DROP TABLE [IF EXISTS] name`
- `ALTER TABLE name ENGINE = memory|disk`
- `INSERT INTO name [(cols)] VALUES (vals), ...`
- `SELECT [cols|*] FROM name [WHERE expr] [ORDER BY col [ASC|DESC]] [LIMIT n] [OFFSET n]`
- `UPDATE name SET col = val [, ...] [WHERE expr]`
- `DELETE FROM name [WHERE expr]`
- `EXPLAIN [ANALYZE] statement`
- `CREATE USER name [WITH] PASSWORD '...'`
- `DROP USER [IF EXISTS] name`
- `ALTER USER name [WITH] PASSWORD '...'`
- `GRANT {SELECT[(cols)]|INSERT[(cols)]|UPDATE[(cols)]|DELETE|ALL} ON table TO user`
- `REVOKE {SELECT[(cols)]|INSERT[(cols)]|UPDATE[(cols)]|DELETE|ALL} ON table FROM user`
- `CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table (col1, col2, ...)`
- `DROP INDEX [IF EXISTS] name ON table`

## Additional Types

### `QueryResult`

```rust
pub struct QueryResult {
    pub columns: Vec<String>,         // Column names
    pub column_types: Vec<String>,    // Column type names (for display)
    pub rows: Vec<Vec<String>>,       // Result rows (all values as strings)
    pub rows_affected: u64,           // Number of rows inserted/updated/deleted
}
```

### `Row`

```rust
pub struct Row {
    pub values: Vec<Datum>,
}
```

### `Permission`

```rust
pub enum Permission {
    Select(Vec<String>),    // Empty vec = all columns
    Insert(Vec<String>),
    Update(Vec<String>),
    Delete,
    All,
}
```

### Error Handling

All operations return `Result<T>` where errors use the `HelionError` enum:

```rust
use heliondb::HelionError;

match engine.commit(tx).await {
    Ok(()) => println!("Committed"),
    Err(HelionError::Conflict(txid)) => {
        println!("Conflict with tx {}, retrying...", txid);
        // Retry the transaction
    }
    Err(HelionError::DuplicateKey { index, key }) => {
        println!("Duplicate key on index {}: {}", index, key);
    }
    Err(HelionError::PermissionDenied(msg)) => {
        println!("Access denied: {}", msg);
    }
    Err(e) => println!("Unexpected error: {}", e),
}
```

## Examples

### Full Transaction API

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::executor::ops::execute;
use heliondb::HelionError;

let mut engine = DatabaseEngine::open("./mydb".as_ref()).await?;

// Create table
let sql = "CREATE TABLE accounts (id INTEGER PRIMARY KEY, owner TEXT, balance REAL)";
execute(&engine, &plan(&parse(sql)?[0], &engine.get_tables().await)?).await?;

// Transaction with conflict retry
let mut tx = engine.begin();

// Insert a row (within the transaction)
let insert_sql = "INSERT INTO accounts VALUES (1, 'Alice', 1000.00)";
let stmts = parse(insert_sql)?;
let insert_plan = plan(&stmts[0], &engine.get_tables().await)?;
execute(&engine, &insert_plan).await?;

// Read the row
let select_sql = "SELECT * FROM accounts WHERE id = 1";
let stmts = parse(select_sql)?;
let select_plan = plan(&stmts[0], &engine.get_tables().await)?;
let result = execute(&engine, &select_plan).await?;
println!("Rows: {:?}", result.rows);

// Commit with retry
loop {
    match engine.commit(tx).await {
        Ok(()) => break,
        Err(HelionError::Conflict(_)) => {
            // Retry: re-read, re-compute, re-try
            tx = engine.begin();
            // ... re-execute the transaction ...
        }
        Err(e) => return Err(e.into()),
    }
}
```

### Permission Checks with Users

```rust
use heliondb::executor::ops::execute_as;

// Create users and grant permissions via SQL
execute(&engine, &plan(&parse("CREATE USER alice WITH PASSWORD 'secret'")?[0], &[])?).await?;
execute(&engine, &plan(&parse("GRANT SELECT(id, name) ON users TO alice")?[0], &[])?).await?;

// Execute as a user (permission checks active)
let result = execute_as(&engine,
    &plan(&parse("SELECT id, name FROM users")?[0], &engine.get_tables().await)?,
    Some("alice"),
).await?;

// This would fail: alice doesn't have SELECT on the 'email' column
let result = execute_as(&engine,
    &plan(&parse("SELECT email FROM users")?[0], &engine.get_tables().await)?,
    Some("alice"),
).await;
assert!(result.is_err());
```

### Manual Row Creation

```rust
use heliondb::storage::types::{Row, Datum, DataType, ColumnMeta, coerce_datum};

let columns = vec![
    ColumnMeta::new("id").primary_key(),
    ColumnMeta::new("name").not_null(),
    ColumnMeta::new("score"),
];

let mut row = Row::new(vec![
    Datum::Integer(1),
    Datum::Text("Alice".into()),
    Datum::Null,
]);

// Coerce values to match column types
for (i, val) in row.values.iter_mut().enumerate() {
    *val = coerce_datum(val, &columns[i].data_type)?;
}
```

### Index Programmatic Usage

```rust
use heliondb::storage::btree::{Index, IndexMeta};

// Create a composite index on columns 0 and 1
let mut idx = Index::new_non_unique("name_idx", vec![0, 1]);

// Insert row references
idx.insert(&[Datum::Text("Alice".into()), Datum::Integer(30)], 0)?;
idx.insert(&[Datum::Text("Bob".into()), Datum::Integer(25)], 1)?;
idx.insert(&[Datum::Text("Alice".into()), Datum::Integer(35)], 2)?;

// Range scan: all entries with name >= "Alice"
let results: Vec<usize> = idx.scan_from(vec![Datum::Text("Alice".into())]);
// Returns [0, 2] — the two Alice rows

// Point lookup
if let Some(row_idxs) = idx.get(&[Datum::Text("Bob".into()), Datum::Integer(25)]) {
    println!("Found Bob, age 25 at row index {:?}", row_idxs);
}
```
