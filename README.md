# HelionDB

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange)](https://www.rust-lang.org)
[![Docs](https://img.shields.io/badge/docs-mdBook-2ea44f)](https://gxcloud.github.io/heliondb/)

HelionDB is a SQL database with PostgreSQL-compatible syntax, per-table storage engines, MVCC snapshot isolation, async WAL persistence, and QUIC transport.

## What It Offers

- `memory` and `disk` storage engines on a per-table basis
- PostgreSQL-flavored SQL parsing with custom HelionDB statements
- MVCC with optimistic concurrency control
- Async write-ahead logging and checkpoint recovery
- QUIC server transport with TLS 1.3
- Column-level permissions and user management

## Documentation

The full documentation is published as an mdBook site:

https://gxcloud.github.io/heliondb/

## Quick Start

```bash
cargo build --release
export HELION_PASSWORD=change-me
./target/release/heliondb --default-engine disk
```

## Example

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::executor::ops::execute;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = DatabaseEngine::open_with_default_engine("./mydb".as_ref(), "memory").await?;
    let stmts = parse("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT) ENGINE = disk")?;
    execute(&engine, &plan(&stmts[0], &[])?).await?;
    Ok(())
}
```

## Project Layout

```text
src/
  storage/       storage engines, MVCC, WAL, permissions, users
  sql/           parser and planner
  executor/      expression evaluation and physical operators
  protocol/      wire protocol and auth
  server/        QUIC server implementation
docs/            mdBook source for the published documentation
```

## Development

```bash
cargo build
cargo test
cargo clippy
mdbook build docs
```
