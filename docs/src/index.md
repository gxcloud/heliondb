# HelionDB

HelionDB is a networked SQL database with pluggable per-table storage engines, MVCC snapshot isolation, async WAL persistence, and QUIC transport.

## Highlights

- Selectable per-table storage engines: `disk` (append-only delta persistence) and `memory`
- PostgreSQL-compatible SQL via `sqlparser-rs`
- MVCC concurrency with snapshot isolation — concurrent reads never block
- B-tree indexes with point lookups, range scans, and composite keys
- Automatic unique index on `PRIMARY KEY` and `UNIQUE` columns
- User-defined indexes via `CREATE INDEX` / `DROP INDEX`
- Unique constraint enforcement at commit time
- Async WAL persistence with CRC32 integrity checks
- Crash recovery from WAL replay, delta-based disk engine, and checkpoints
- Index-aware query optimization for WHERE, ORDER BY, IN, BETWEEN
- QUIC transport via `quinn` with TLS 1.3
- Multi-database support with per-database engines, WALs, and catalogs

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
| `CREATE [UNIQUE] INDEX name ON table (cols)` | ✅ |
| `DROP INDEX [IF EXISTS] name ON table` | ✅ |
| Unique constraint enforcement | ✅ |
| Index-accelerated `WHERE`, `ORDER BY`, `IN`, `BETWEEN` | ✅ |
| Implicit type coercion | ✅ |
| Transactions (`BEGIN`/`COMMIT`/`ROLLBACK`) | ✅ |
| MVCC snapshot isolation | ✅ |
| `JOIN` (`INNER`, `LEFT`, `RIGHT`, `CROSS`) | ✅ |
| `FOREIGN KEY` / `REFERENCES` | ✅ (parsed and stored) |
| Three join algorithms (NLJ, INLJ, Hash) | ✅ (auto-selected) |
| Predicate pushdown optimization | ✅ |
| Index selection optimization | ✅ |
| Prisma-style structured query protocol | ✅ (findMany, create, update, delete, upsert) |
| Auto-JOIN via `include` + foreign keys | ✅ |

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

- **New to HelionDB?** Start with the [User Guide](user-guide.md) and [Build Guide](build.md)
- **Deploying to production?** Read the [Deployment Guide](deployment.md) (Docker, systemd, TLS, backups)
- **Understanding the internals?** Dive into the [Architecture](architecture.md) (MVCC, WAL, commit pipeline)
- **Writing SQL?** Check the [SQL Reference](sql-reference.md) for syntax and type coercion rules
- **Building an application?** Use the [API Reference](api.md) for library documentation
- **Implementing a client?** Study the [Wire Protocol](protocol.md) and [Structured Query Protocol](structured-query.md) for the Prisma-style JSON API
- **Contributing code?** Read the [Contributing Guide](contributing.md) for development workflow
- **Fixing a problem?** Search the [Troubleshooting Guide](troubleshooting.md) for common issues
