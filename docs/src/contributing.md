# Contributing

## Code of Conduct

This project follows the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). All participants are expected to uphold its principles.

## Getting Started

```bash
# Fork the repository on GitHub, then:
git clone https://github.com/YOUR-USERNAME/heliondb.git
cd heliondb
cargo build
cargo test
```

## Development Workflow

1. Create a branch from `main`:
   - `feat/description` for new features
   - `fix/description` for bug fixes
   - `docs/description` for documentation
2. Make your changes
3. Run `cargo clippy -- -D warnings` and `cargo fmt`
4. Ensure all tests pass: `cargo test`
5. Open a pull request against `main`

### Commit Messages

Follow conventional commits-style messages:

```
feat: add CREATE INDEX SQL statement

Indexes can now be created via SQL with CREATE INDEX name ON table (cols).
Supports UNIQUE, IF NOT EXISTS, and composite indexes.
```

Prefixes: `feat`, `fix`, `docs`, `refactor`, `test`, `style`, `ci`, `perf`.

## How To: Add a SQL Statement

Example: adding `PURGE TABLE` (hypothetical). Each step involves a different layer:

### 1. Parser — `src/sql/parser.rs`

Add a variant to `HelionStatement`:

```rust
pub enum HelionStatement {
    // ... existing variants ...
    PurgeTable { name: String },
}
```

Add parsing logic in the main `parse()` function or `parse_custom()`:

```rust
fn parse_custom(sql: &str) -> Option<Result<Vec<HelionStatement>>> {
    // Check for PURGE TABLE pattern
    // Return Some(Ok(vec![HelionStatement::PurgeTable { name }]))
}
```

Add a test at the bottom of the file.

### 2. Planner — `src/sql/planner.rs`

Add a variant to `LogicalPlan`:

```rust
pub enum LogicalPlan {
    // ... existing variants ...
    PurgeTable { name: String },
}
```

Add planning logic in `plan()`:

```rust
HelionStatement::PurgeTable { name } => {
    // Validate table exists
    find_table(tables, name)?;
    Ok(LogicalPlan::PurgeTable { name })
}
```

### 3. Executor — `src/executor/ops.rs`

Add execution logic in `execute_as()`:

```rust
LogicalPlan::PurgeTable { name } => {
    // Verify permissions (if current_user is Some)
    // Execute the operation against the engine
    // Return QueryResult
}
```

### 4. Integration Test — `tests/integration.rs`

```rust
#[tokio::test]
async fn test_purge_table() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = setup(&dir).await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2), (3)").await;
    exec(&engine, "PURGE TABLE t").await;
    // Verify table is empty
}
```

## How To: Add a Data Type

### 1. `src/storage/types.rs`

Add variants to `DataType` and `Datum`:

```rust
pub enum DataType {
    // ... existing variants ...
    TinyInt,
}

pub enum Datum {
    // ... existing variants ...
    TinyInt(i8),
}
```

Add `Ord` support for the new `Datum` variant. Add `From` impls. Handle in `DataType::from_sql()` and `coerce_datum()`.

### 2. Parser — `src/sql/parser.rs`

Map the SQL type name in `parse_column_type()`.

### 3. Tests

Add test cases in `test_create_table_all_types` and type-specific tests.

## How To: Add a SQL Function

### 1. Executor — `src/executor/eval.rs`

Add an arm in `evaluate_function()`:

```rust
"sqrt" => {
    if args.len() != 1 { return Err(...) }
    let val = evaluate(&args[0], row, columns)?;
    match val {
        Datum::Double(f) => Ok(Datum::Double(f.sqrt())),
        Datum::Integer(i) => Ok(Datum::Double((i as f64).sqrt())),
        _ => Err(type_error(...)),
    }
}
```

### 2. Tests — Same file in `#[cfg(test)]` module

```rust
#[test]
fn test_sqrt() {
    let row = &[];
    let cols = &[];
    let result = evaluate(
        &Expression::Function { name: "sqrt".into(), args: vec![literal(9.0)] },
        row, cols,
    ).unwrap();
    assert_eq!(result, Datum::Double(3.0));
}
```

## How To: Add a Storage Engine

### 1. Create `src/storage/engines/myengine.rs`

Implement the `StorageEngine` trait (in `src/storage/engine_trait.rs`):

```rust
pub struct MyEngine { /* ... */ }

#[async_trait]
impl StorageEngine for MyEngine {
    fn name(&self) -> &str { "myengine" }
    async fn create_table(&self, meta: &TableMeta, columns: Vec<ColumnMeta>) -> Result<()> { /* ... */ }
    async fn get_table(&self, name: &str) -> Result<Table> { /* ... */ }
    async fn apply_write_set(&self, table: &str, changes: Vec<(usize, RowVersion)>) -> Result<()> { /* ... */ }
    // ... implement all required methods
}
```

### 2. Register in `src/storage/catalog.rs`

In `Catalog::open()`:

```rust
self.register_engine("myengine", Arc::new(MyEngine::open(base_dir).await?));
```

### 3. Use in SQL

```sql
CREATE TABLE my_table (id INTEGER) ENGINE = myengine;
```

## Internal Architecture Deep-Dives

### MVCC Version Chains

Each logical row is stored as a chain of `RowVersion` structs:

```text
Row 0 (INSERT txid=5):
  ┌───────────────────────────────────────┐
  │ RowVersion { txid_min: 5, txid_max:  │
  │   MAX, row: (1, "Alice", 30),        │
  │   is_deleted: false }                 │
  └───────────────────────────────────────┘

Row 0 (UPDATE txid=10):
  ┌───────────────────────┐  ┌───────────────────────┐
  │ txid_min: 5           │  │ txid_min: 10          │
  │ txid_max: 10          │←│ txid_max: MAX          │
  │ row: (1, "Alice", 30) │  │ row: (1, "Alice", 31) │
  │ is_deleted: false     │  │ is_deleted: false      │
  └───────────────────────┘  └───────────────────────┘
    (old version, hidden)      (current visible version)
```

Visibility rule (`is_version_visible`):

```
A version is visible to a snapshot if:
  txid_min <= snapshot_txid   (version was created at or before the snapshot)
  AND snapshot_txid < txid_max    (version wasn't deleted/overwritten yet)
  AND txid_min NOT IN active_txns    (creator wasn't uncommitted at snapshot time)
  UNLESS txid_min == self_txid     (... unless we created it ourselves)
```

### Optimistic Concurrency Control

On commit, `check_conflicts()` detects write-write conflicts:

```
For each entry in the transaction's write_set:
  Look up the latest RowVersion in the table for that row index
  If RowVersion.txid_min was in our snapshot's active set:
    CONFLICT — another transaction modified this row concurrently
    Return HelionError::Conflict(conflicting_txid)
```

This means HelionDB uses **optimistic** concurrency: no locks are held during the transaction. Conflicts are detected at commit time. Applications should retry on `HelionError::Conflict`.

### WAL Format

```text
┌─────────────────────────────────────────────────────────┐
│                    WAL File Layout                       │
├─────────────────────────────────────────────────────────┤
│ Record 1:                                                │
│   4 bytes: payload length (big-endian u32)               │
│   4 bytes: CRC32 checksum of payload                     │
│   N bytes: bincode-serialized WalRecord                  │
├─────────────────────────────────────────────────────────┤
│ Record 2: ...                                            │
├─────────────────────────────────────────────────────────┤
│ ...                                                      │
└─────────────────────────────────────────────────────────┘
```

Record types (`WalRecord`):
- `CreateTable`, `DropTable` — DDL operations
- `Insert`, `Update`, `Delete` — DML operations
- `Commit { txid }` — transaction commit marker
- `Checkpoint { table_count, tables }` — full table snapshot for faster recovery
- `CreateUser`, `DropUser` — user management
- `Grant`, `Revoke` — permission changes

### Recovery Process

```
1. Check for latest .checkpoint file → load full table snapshot
2. Open helion.wal, seek to end of checkpoint record
3. Read remaining records sequentially:
   a. Validate CRC32 — skip corrupted records with a warning
   b. Apply each record to in-memory table state
4. Compute max_txid from all version chains
5. Rebuild all indexes from version chain data
6. Restore tables into their assigned storage engines
7. Replay user and permission records from WAL
8. Bootstrap default admin user from HELION_PASSWORD
9. Spawn checkpoint background loop
```

### Commit Pipeline

```
1. MvccStore::check_conflicts()     — OCC conflict detection
2. Table::check_unique_constraints() — PK and UNIQUE enforcement
3. MvccStore::apply_write_set()     — compute new version chains
4. StorageEngine::apply_write_set() — persist to engine
5. Update in-memory tables + indexes
6. WalWriter::append(record) × N    — log each mutation
7. WalWriter::append(Commit)        — log commit marker
8. MvccStore::commit_transaction()  — remove from active set
```

On failure before step 6, the transaction is safe to retry (no WAL entries written). On failure during 6-7, WAL recovery will handle incomplete writes.

### Engine Migration (ALTER TABLE ... ENGINE)

```
1. Snapshot: read all data from source engine into memory
2. Create: create the table in the target engine
3. Restore: write all data into the target engine
4. Drop: remove the table from the source engine
5. Update catalog entry
```

If any step fails, the system attempts to roll back: if the target was already created, it drops it.

## Debugging Tips

```bash
# Enable debug logging
RUST_LOG=heliondb=debug ./target/release/heliondb

# Trace-level logging (extremely verbose)
RUST_LOG=heliondb=trace ./target/release/heliondb

# Inspect WAL contents (hexdump)
xxd /var/lib/heliondb/helion.wal | head -50
```

### Common Pitfalls

- **Missing `#[async_trait]`** — When implementing `StorageEngine`, don't forget the `#[async_trait]` macro on both the trait definition and the impl block.
- **Forgetting `coerce_datum`** — When inserting values, always call `coerce_datum()` to handle type widening (e.g., `INTEGER` → `BIGINT`).
- **MVCC snapshot staleness** — `get_tables()` returns a snapshot. If you hold it across async yield points, the snapshot may be stale. Re-fetch if needed.
- **WAL path** — The WAL is always at `{data_dir}/helion.wal`. Don't move or modify it while the server is running.

## Testing Guidelines

- **Unit tests**: test one function/behavior in isolation. Use `#[cfg(test)]` modules within the source file.
- **Integration tests**: test the full parse → plan → execute pipeline. Add to `tests/integration.rs`.
- **MVCC tests**: test concurrent transactions explicitly using `tokio::spawn` with `join_all`.
- **WAL tests**: test persistence by shutting down the engine, re-opening, and verifying data.
- **Permission tests**: test both `exec()` (no checks) and `exec_as()` (with checks).
- **Error tests**: test that invalid operations produce the correct `HelionError` variant.

## Release Process

1. Update version in `Cargo.toml`
2. Update `CHANGELOG.md` with the new version and changes
3. Create a git tag: `git tag v0.2.0`
4. Push tag: `git push origin v0.2.0`
5. Publish to crates.io (future): `cargo publish`
