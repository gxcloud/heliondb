# HelionDB User Guide

## Installation

### Prerequisites

- Rust 1.75+ (install via [rustup](https://rustup.rs/))

### Build from Source

```bash
git clone https://github.com/gxcloud/heliondb.git
cd heliondb
cargo build --release
```

The binary will be at `./target/release/heliondb`.

## Running the Server

### Basic Usage

```bash
# Run with default settings (data dir: ./data, listen: 127.0.0.1:9613)
./target/release/heliondb

# Run with custom data directory and listen address
./target/release/heliondb --data-dir /var/lib/heliondb --listen 0.0.0.0:9613

# Set the default engine for new tables
./target/release/heliondb --default-engine disk
```

### First Run

On first start, HelionDB:
1. Creates the data directory if it doesn't exist
2. Creates an empty WAL file
3. Reads the `HELION_PASSWORD` environment variable to create the default `helion` admin user
4. Starts the QUIC listener

**Important**: You must set `HELION_PASSWORD` on first start to create the admin user:

```bash
export HELION_PASSWORD=my_secure_password
./target/release/heliondb
```

### TLS Certificates

By default, HelionDB generates a self-signed certificate on first run. To use your own certificates:

```bash
./target/release/heliondb --cert /path/to/cert.pem --key /path/to/key.pem
```

### CLI Options

```
Usage: heliondb [OPTIONS]

Options:
      --data-dir <PATH>          Data directory [default: ./data]
      --listen <ADDR>            QUIC listen address [default: 127.0.0.1:9613]
      --cert <PATH>              TLS certificate file (PEM)
      --key <PATH>               TLS private key file (PEM)
      --default-engine <ENGINE>  memory | disk [default: memory]
      --durability <MODE>        sync | async [default: async]
      --checkpoint-interval <S>  Seconds between checkpoints [default: 60]
  -h, --help                     Print help
  -V, --version                  Print version
```

### Durability Modes

| Mode | Behavior | Data Loss Window |
|------|----------|------------------|
| `async` (default) | WAL flushed every 5ms. Fastest writes. | Up to 5ms of writes |
| `sync` | WAL flushed on every write. Safest. | None |

## Connecting to HelionDB

### Using the Rust Client Library

Add to your `Cargo.toml`:

```toml
[dependencies]
heliondb = { git = "https://github.com/gxcloud/heliondb" }
tokio = { version = "1", features = ["full"] }
```

Connect and authenticate:

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::executor::ops::{execute, execute_as};
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;

let mut engine = DatabaseEngine::open_with_default_engine("./db".as_ref(), "memory").await?;

// Authenticate
assert!(engine.verify_user("helion", "my_secure_password").await);

// Execute queries (no permission check for embedded use)
let s = &parse("CREATE TABLE items (id INTEGER, name TEXT) ENGINE = disk")?;
execute(&engine, &plan(&s[0], &[])?).await?;

let s = &parse("ALTER TABLE items ENGINE = memory")?;
let tables = engine.get_tables().await;
execute(&engine, &plan(&s[0], &tables)?).await?;
```

### Using a Generic QUIC Client

Any QUIC-capable client can connect. The protocol uses length-prefixed bincode messages (see [PROTOCOL.md](PROTOCOL.md)).

## SQL Reference

### Data Definition

```sql
-- Create table
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email VARCHAR(255),
    age INTEGER DEFAULT 0
);
CREATE TABLE archive_users (id INTEGER, name TEXT) ENGINE = disk;

-- Drop table
DROP TABLE users;
DROP TABLE IF EXISTS users;

-- Change engine
ALTER TABLE users ENGINE = memory;
ALTER TABLE archive_users ENGINE = disk;
```

### Data Manipulation

```sql
-- Insert
INSERT INTO users VALUES (1, 'Alice', 'alice@example.com', 30);
INSERT INTO users (id, name) VALUES (2, 'Bob');

-- Select
SELECT * FROM users;
SELECT id, name FROM users WHERE age > 18;
SELECT * FROM users ORDER BY name DESC LIMIT 10 OFFSET 20;
SELECT name FROM users WHERE name LIKE 'A%' AND age BETWEEN 20 AND 40;
SELECT COUNT(*), AVG(age) FROM users;

-- Update
UPDATE users SET email = 'new@example.com' WHERE id = 1;

-- Delete
DELETE FROM users WHERE id = 2;
```

### User Management

```sql
-- Create user (password hashed with Argon2id)
CREATE USER alice WITH PASSWORD 'secure_password';
CREATE USER bob PASSWORD 'another_password';

-- Drop user
DROP USER alice;
DROP USER IF EXISTS alice;

-- Change password
ALTER USER bob WITH PASSWORD 'new_password';
```

### Permission Management

```sql
-- Grant table-level SELECT (all columns)
GRANT SELECT ON users TO alice;

-- Grant column-level SELECT
GRANT SELECT(id, name) ON users TO alice;

-- Grant write permissions
GRANT INSERT ON users TO alice;
GRANT UPDATE(email) ON users TO alice;

-- Grant delete
GRANT DELETE ON users TO alice;

-- Grant all permissions
GRANT ALL ON users TO alice;

-- Revoke permissions
REVOKE SELECT ON users FROM alice;
REVOKE ALL ON users FROM alice;
```

### Permission Semantics

| Statement | Check | Example |
|-----------|-------|---------|
| `SELECT *` | All columns must be SELECT-granted | `GRANT SELECT(id,name) ON t TO u` → `SELECT *` ✓ if table has only id, name |
| `SELECT col` | That column must be SELECT-granted | `GRANT SELECT(id) ON t TO u` → `SELECT id` ✓, `SELECT email` ✗ |
| `INSERT` | All inserted columns must be INSERT-granted | `GRANT INSERT(name) ON t TO u` → `INSERT(name)` ✓, `INSERT(id,name)` ✗ |
| `UPDATE col=val` | Each SET column must be UPDATE-granted | `GRANT UPDATE(email) ON t TO u` → `UPDATE SET email=x` ✓ |
| `DELETE` | DELETE grant required (table-level) | `GRANT DELETE ON t TO u` → `DELETE FROM t` ✓ |
| `GRANT ALL` | Covers all operations, all columns | Overrides all column-level restrictions |

## Data Types

| SQL Type | Range | Notes |
|----------|-------|-------|
| `BOOLEAN` | `true`/`false` | |
| `SMALLINT` | -32,768 to 32,767 | 16-bit |
| `INTEGER` / `INT` | -2^31 to 2^31-1 | 32-bit |
| `BIGINT` | -2^63 to 2^63-1 | 64-bit |
| `REAL` | ~±3.4E38 | 32-bit float |
| `DOUBLE` / `FLOAT` | ~±1.8E308 | 64-bit float |
| `VARCHAR(n)` | Up to n chars | Variable-length |
| `CHAR(n)` | Exactly n chars | Fixed-length |
| `TEXT` | Unlimited | Variable-length |
| `DATE` | 1000-01-01 to 9999-12-31 | |
| `TIME` | 00:00:00 to 23:59:59 | |
| `TIMESTAMP` | 1000-01-01 to 9999-12-31 | |
| `UUID` | Standard UUID | |

Implicit conversions between numeric types and between string types are automatic.

## Constraints

| Constraint | Description |
|------------|-------------|
| `PRIMARY KEY` | Implies `NOT NULL` + `UNIQUE` |
| `NOT NULL` | Column cannot contain NULL |
| `UNIQUE` | All values in column must be unique |
| `NULL` | Column allows NULL (default) |

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|------------|-------|
| Point lookup (primary key) | O(n) scan | No index yet; full table scan |
| INSERT | O(1) append | |
| SELECT * (no WHERE) | O(n) | Scans all visible versions |
| SELECT with WHERE | O(n) | Full scan + filter |
| DELETE | O(n) | Scan + version chain append |
| WAL append | O(1) async | Background flush |
| Checkpoint | O(tables + rows) | Periodic |

## Backup and Restore

HelionDB's data directory contains the WAL file (`helion.wal`), the catalog, and any disk-engine table directories. To back up:

```bash
# Graceful shutdown first
kill <heliondb_pid>

# Backup the data directory
cp -r ./data ./data.backup

# Restore by pointing to the backup
./target/release/heliondb --data-dir ./data.backup
```

## Configuration

### Environment Variables

| Variable | Purpose |
|----------|---------|
| `HELION_PASSWORD` | Password for default `helion` admin user (required on first start) |
| `RUST_LOG` | Logging level: `error`, `warn`, `info`, `debug`, `trace` |
