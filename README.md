# HelionDB

**An extremely fast in-memory SQL database with PostgreSQL-compatible syntax, async WAL persistence, and QUIC transport.**

[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange)](https://www.rust-lang.org)

## Overview

HelionDB is an in-memory SQL database designed for speed. All data lives in RAM for maximum read/ write performance, while every mutation is asynchronously persisted to a write-ahead log (WAL) on disk. On restart, the database replays the WAL to reconstruct its full state.

### Key Features

- **In-Memory Speed**: All data stored in RAM, no disk I/O on reads
- **PostgreSQL-Compatible SQL**: Uses `sqlparser-rs` with the PostgreSQL dialect
- **MVCC Concurrency**: Snapshot isolation with optimistic concurrency control — readers never block writers
- **Async WAL Persistence**: Every write is appended to a WAL file with CRC32 integrity checks
- **Crash Recovery**: Full state reconstruction from WAL on restart
- **Checkpoint System**: Periodic full snapshots to control WAL size
- **QUIC Transport**: Fast, encrypted, multiplexed network protocol (via `quinn`)
- **Async Throughout**: Built on `tokio` for all I/O and networking

### SQL Support

| Feature | Status |
|---------|--------|
| `CREATE TABLE` with column types and constraints | ✅ |
| `DROP TABLE` | ✅ |
| `INSERT INTO ... VALUES` | ✅ |
| `SELECT ... FROM ... WHERE` | ✅ |
| `UPDATE ... SET ... WHERE` | ✅ |
| `DELETE FROM ... WHERE` | ✅ |
| `ORDER BY`, `LIMIT`, `OFFSET` | ✅ |
| `WHERE` expressions (`=`, `<`, `>`, `AND`, `OR`, `IN`, `BETWEEN`, `LIKE`, `IS NULL`) | ✅ |
| Aggregate functions (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`) | ✅ |
| Scalar functions (`LOWER`, `UPPER`, `LENGTH`, `COALESCE`, `IFNULL`, `ABS`, `ROUND`) | ✅ |
| `PRIMARY KEY`, `NOT NULL`, `UNIQUE` | ✅ |
| Implicit type coercion (Integer/BigInt, Text/VarChar, etc.) | ✅ |
| Transactions (`BEGIN`/`COMMIT`/`ROLLBACK`) | ✅ |
| MVCC snapshot isolation | ✅ |
| Subqueries, CTEs, JOINs | 🔜 |
| Indexes | 🔜 |
| Window functions | 🔜 |

### Data Types

| SQL Type | Rust Type |
|----------|-----------|
| `BOOLEAN` | `bool` |
| `SMALLINT` | `i16` |
| `INTEGER` | `i32` |
| `BIGINT` | `i64` |
| `REAL` | `f32` |
| `DOUBLE`, `FLOAT` | `f64` |
| `VARCHAR(n)`, `VARCHAR` | `String` |
| `CHAR(n)`, `CHAR` | `String` |
| `TEXT` | `String` |
| `DATE` | `NaiveDate` |
| `TIME` | `NaiveTime` |
| `TIMESTAMP` | `NaiveDateTime` |
| `UUID` | `uuid::Uuid` |
| `NULL` | - |

## Quick Start

### Using as a Library

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::executor::ops::execute;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = DatabaseEngine::open("./mydb".as_ref()).await?;

    // Create a table
    let stmts = parse("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")?;
    execute(&engine, &plan(&stmts[0], &[])?).await?;

    // Insert data
    let stmts = parse("INSERT INTO users VALUES (1, 'Alice', 30)")?;
    let tables = engine.get_tables().await;
    execute(&engine, &plan(&stmts[0], &tables)?).await?;

    // Query
    let stmts = parse("SELECT * FROM users WHERE age > 25")?;
    let tables = engine.get_tables().await;
    let result = execute(&engine, &plan(&stmts[0], &tables)?).await?;

    println!("{:?}", result);

    engine.shutdown().await?;
    Ok(())
}
```

### Running as a Server

```bash
# Build
cargo build --release

# Run with default options (binds to 127.0.0.1:9613)
./target/release/heliondb

# Run with custom options
./target/release/heliondb --data-dir /var/lib/heliondb --listen 0.0.0.0:9613

# Generate TLS cert for QUIC first run (auto-generated if missing)
```

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                     Client                           │
│  (heliondb CLI / Rust client / any QUIC client)     │
└──────────────────┬──────────────────────────────────┘
                   │ QUIC (quinn)
┌──────────────────▼──────────────────────────────────┐
│                   Server                             │
│  ┌──────────────┐  ┌──────────────┐                 │
│  │ QUIC Listener│  │  Session Mgr │                 │
│  └──────┬───────┘  └──────┬───────┘                 │
└─────────┼─────────────────┼─────────────────────────┘
          │                 │
┌─────────▼─────────────────▼─────────────────────────┐
│                   SQL Layer                          │
│  ┌──────────────┐  ┌──────────────┐                 │
│  │   Parser     │  │   Planner    │                 │
│  │ (sqlparser)  │  │ (logical     │                 │
│  │              │  │  plan build) │                 │
│  └──────────────┘  └──────────────┘                 │
└─────────────────────┬───────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────┐
│                Executor Layer                        │
│  ┌──────────────┐  ┌──────────────┐                 │
│  │  Expression  │  │ Physical Ops │                 │
│  │  Evaluator   │  │ (scan,       │                 │
│  │              │  │  filter,     │                 │
│  │              │  │  project)    │                 │
│  └──────────────┘  └──────────────┘                 │
└─────────────────────┬───────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────┐
│              Storage Engine (MVCC)                   │
│  ┌──────────────┐  ┌──────────────┐                 │
│  │  Transaction │  │   Version    │                 │
│  │  Manager     │  │   Chains     │                 │
│  ├──────────────┤  ├──────────────┤                 │
│  │  Snapshot    │  │  Optimistic  │                 │
│  │  Isolation   │  │  Concurrency │                 │
│  │              │  │  Control     │                 │
│  └──────┬───────┘  └──────┬───────┘                 │
└─────────┼─────────────────┼─────────────────────────┘
          │                 │
┌─────────▼─────────────────▼─────────────────────────┐
│              Persistence Layer                       │
│  ┌──────────────────┐  ┌──────────────────┐         │
│  │   WAL Writer     │  │  Checkpoint      │         │
│  │   (append-only,  │  │  (periodic       │         │
│  │    CRC32)        │  │   snapshots)     │         │
│  └──────────────────┘  └──────────────────┘         │
└─────────────────────────────────────────────────────┘
```

### Storage Engine

HelionDB uses **MVCC (Multi-Version Concurrency Control)** with snapshot isolation:

- **Rows** are stored as version chains — each update creates a new version rather than overwriting
- **Transactions** see a consistent snapshot of the database taken at the moment they begin
- **Writers** never block readers — readers see the version that was current at their snapshot time
- **Concurrent writes** to the same row are detected via **optimistic concurrency control** — the first to commit wins, the second is aborted with a conflict error

### Write-Ahead Log

Every mutation goes through the WAL:

1. Transaction begins → snapshot is captured
2. Reads proceed against the snapshot
3. Writes are collected in a transaction-local write set
4. On commit: conflicts are checked → writes applied to in-memory tables → WAL records appended
5. WAL is an append-only binary file with length-prefixed, bincode-serialized, CRC32-protected records
6. On restart: WAL is replayed from beginning to reconstruct state

### Checkpoints

A background task periodically writes a full snapshot of all tables as a `Checkpoint` WAL record. On restart, the latest checkpoint is loaded first (avoiding full WAL replay), then remaining WAL records are applied.

## Configuration

### CLI Options

```
Usage: heliondb [OPTIONS]

Options:
      --data-dir <PATH>        Data directory for WAL/checkpoints [default: ./data]
      --listen <ADDR>          QUIC listen address [default: 127.0.0.1:9613]
      --cert <PATH>            TLS certificate for QUIC (auto-generated if not specified)
      --key <PATH>             TLS private key for QUIC (auto-generated if not specified)
      --durability <MODE>      Durability mode: sync | async [default: async]
      --checkpoint-interval <S>  Seconds between checkpoints [default: 60]
  -h, --help                   Print help
  -V, --version                Print version
```

### Durability Modes

- **async** (default): Query results returned immediately; WAL flushed every 5ms. Maximum speed, bounded data loss window.
- **sync**: WAL is flushed before each query response. Maximum safety, slower writes.

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Run with verbose output
RUST_LOG=heliondb=debug cargo run

# Check for issues
cargo clippy
cargo check
```

### Project Structure

```
src/
├── main.rs              # CLI entry point
├── lib.rs               # Public API
├── error.rs             # Error types
├── storage/
│   ├── types.rs         # Data types (DataType, Datum, ColumnMeta, Row)
│   ├── table.rs         # Table with MVCC version chains
│   ├── mvcc.rs          # Transaction management, snapshot isolation
│   ├── wal.rs           # Write-ahead log
│   ├── checkpoint.rs    # Periodic snapshots
│   └── engine.rs        # Database engine orchestrator
├── sql/
│   ├── parser.rs        # SQL parser (wraps sqlparser-rs)
│   └── planner.rs       # Logical query planner
├── executor/
│   ├── eval.rs          # Expression evaluator
│   └── ops.rs           # Physical operators
├── server/
│   ├── quic.rs          # QUIC server
│   └── session.rs       # Session management
└── protocol/
    └── messages.rs      # Protocol message types
```

## Testing

HelionDB has 92+ unit and integration tests covering:

- MVCC visibility rules
- Optimistic concurrency conflict detection
- WAL serialization and replay
- Checkpoint save/load
- SQL parsing (all supported statements and expressions)
- Query planning and column resolution
- Full execution pipeline (CREATE → INSERT → SELECT → UPDATE → DELETE)
- Expression evaluation (all operators and functions)
- Type compatibility and constraint validation
- Transaction commit/rollback semantics
- Async WAL persistence across engine restarts

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_execute_insert_and_select

# Run with output
cargo test -- --nocapture
```

## License

MIT License — see [LICENSE](LICENSE)

## Author

Alexander Gauss ([@gxcloud](https://github.com/gxcloud))
