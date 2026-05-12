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
