# HelionDB

HelionDB is a fast SQL database with PostgreSQL-compatible syntax, selectable per-table storage engines, async WAL persistence, and QUIC transport.

## Highlights

- Selectable storage engines: `memory` and `disk`
- PostgreSQL-compatible SQL via `sqlparser-rs`
- MVCC concurrency with snapshot isolation
- Async WAL persistence with CRC32 integrity checks
- Crash recovery from WAL replay and checkpoints
- QUIC transport via `quinn`

## SQL Support

| Feature | Status |
|---------|--------|
| `CREATE TABLE ... ENGINE = memory|disk` | ✅ |
| `DROP TABLE` | ✅ |
| `ALTER TABLE ... ENGINE = memory|disk` | ✅ |
| `EXPLAIN` / `EXPLAIN ANALYZE` | ✅ |
| `INSERT INTO ... VALUES` | ✅ |
| `SELECT ... FROM ... WHERE` | ✅ |
| `UPDATE ... SET ... WHERE` | ✅ |
| `DELETE FROM ... WHERE` | ✅ |
| `ORDER BY`, `LIMIT`, `OFFSET` | ✅ |
| `WHERE` expressions (`=`, `<`, `>`, `AND`, `OR`, `IN`, `BETWEEN`, `LIKE`, `IS NULL`) | ✅ |
| Aggregate functions (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`) | ✅ |
| Scalar functions (`LOWER`, `UPPER`, `LENGTH`, `COALESCE`, `IFNULL`, `ABS`, `ROUND`) | ✅ |
| `UUIDV7()` | ✅ |
| `PRIMARY KEY`, `NOT NULL`, `UNIQUE` | ✅ |
| Implicit type coercion | ✅ |
| Transactions (`BEGIN`/`COMMIT`/`ROLLBACK`) | ✅ |
| MVCC snapshot isolation | ✅ |

## Quick Start

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::executor::ops::execute;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = DatabaseEngine::open_with_default_engine("./mydb".as_ref(), "memory").await?;

    let stmts = parse("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER) ENGINE = disk")?;
    execute(&engine, &plan(&stmts[0], &[])?).await?;

    engine.shutdown().await?;
    Ok(())
}
```

## Where To Go Next

- Read the [User Guide](user-guide.md)
- Learn the [Architecture](architecture.md)
- Review the [API Reference](api.md)
- Inspect the [Wire Protocol](protocol.md)
