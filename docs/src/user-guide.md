# User Guide

## Installation

### Prerequisites

- Rust 1.75+ (install via [rustup](https://rustup.rs/))

### Build from Source

```bash
git clone https://github.com/gxcloud/heliondb.git
cd heliondb
cargo build --release
```

The binaries will be at `./target/release/heliondb` (server) and `./target/release/helionctl` (CLI client).

## Running the Server

### Basic Usage

```bash
./target/release/heliondb
./target/release/heliondb --data-dir /var/lib/heliondb --listen 0.0.0.0:9613
./target/release/heliondb --default-engine disk
```

### First Run

On first start, HelionDB:

1. Creates the data directory if it doesn't exist
2. Creates an empty WAL file
3. Reads `HELION_PASSWORD` to create the default `helion` admin user
4. Starts the QUIC listener

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

```text
Usage: heliondb [OPTIONS]

Options:
      --data-dir <PATH>          Data directory [default: ./data]
      --listen <ADDR>            QUIC listen address [default: 127.0.0.1:9613]
      --cert <PATH>              TLS certificate file (PEM)
      --key <PATH>               TLS private key file (PEM)
      --default-engine <ENGINE>  memory | disk [default: memory]
      --durability <MODE>        sync | async [default: async]
      --checkpoint-interval <S>  Seconds between checkpoints [default: 60]
```

### Durability Modes

| Mode | Behavior | Data Loss Window |
|------|----------|------------------|
| `async` (default) | WAL flushed every 5ms. Fastest writes. | Up to 5ms of writes |
| `sync` | WAL flushed on every write. Safest. | None |

## Connecting To HelionDB

### Rust Client Library

```toml
[dependencies]
heliondb = { git = "https://github.com/gxcloud/heliondb" }
tokio = { version = "1", features = ["full"] }
```

```rust
use heliondb::storage::engine::DatabaseEngine;
use heliondb::executor::ops::{execute, execute_as};
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;

let mut engine = DatabaseEngine::open_with_default_engine("./db".as_ref(), "memory").await?;
assert!(engine.verify_user("helion", "my_secure_password").await);

let s = &parse("CREATE TABLE items (id INTEGER, name TEXT) ENGINE = disk")?;
execute(&engine, &plan(&s[0], &[])?).await?;
```

### helionctl CLI Client

The `helionctl` binary is an interactive SQL shell (like `psql`) that connects to a running HelionDB server over QUIC.

```bash
# Start the server first, then:
./target/release/helionctl --password my_password

# Specify host, user, password
./target/release/helionctl --host 127.0.0.1:9613 --user helion --password my_password

# Use environment variables instead of flags
export HELIONDB_USER=helion
export HELIONDB_PASSWORD=my_password
./target/release/helionctl
```

**Single query mode** — pass SQL as a positional argument:

```bash
./target/release/helionctl --password my_password "SELECT * FROM users"
```

**Self-signed certificates** — the server generates a self-signed cert by default. Connect with `--insecure` to skip TLS verification:

```bash
./target/release/helionctl --password my_password --insecure
```

**REPL commands** (inside the interactive shell):

| Command | Description |
|---------|-------------|
| `\q` | Quit |
| `\?`, `\h`, `\help` | Show help |
| `\x` | Toggle expanded (vertical) display |
| `\g` | Re-run the last SQL query |

**CLI options:**

```text
Usage: helionctl [OPTIONS] [SQL]

Options:
  -h, --host <ADDR>        Server address [default: 127.0.0.1:9613]
  -u, --user <USER>        Username [env: HELIONDB_USER] [default: helion]
  -p, --password <PASS>    Password [env: HELIONDB_PASSWORD]
      --server-name <SNI>  TLS server name (SNI) [default: heliondb.local]
      --insecure           Skip TLS certificate verification
  <SQL>                    Optional SQL to execute and exit
```

### Generic QUIC Client

Any QUIC-capable client can connect. The protocol uses length-prefixed bincode messages; see [Wire Protocol](protocol.md).

## SQL Reference

### Data Definition

```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email VARCHAR(255) UNIQUE,
    age INTEGER DEFAULT 0
);
CREATE TABLE archive_users (id INTEGER, name TEXT) ENGINE = disk;

DROP TABLE users;
DROP TABLE IF EXISTS users;

ALTER TABLE users ENGINE = memory;
ALTER TABLE archive_users ENGINE = disk;
```

### Index Management

HelionDB automatically creates unique indexes on `PRIMARY KEY` and `UNIQUE` columns.
You can also create user-defined indexes to accelerate queries:

```sql
-- Automatically created on PRIMARY KEY columns
CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL);

-- Manual index creation
CREATE INDEX idx_products_price ON products (price);
CREATE UNIQUE INDEX idx_products_name ON products (name);

-- Composite index (multiple columns)
CREATE INDEX idx_products_name_price ON products (name, price);

-- Conditional creation
CREATE INDEX IF NOT EXISTS idx_products_price ON products (price);

-- Drop indexes
DROP INDEX idx_products_price ON products;
DROP INDEX IF EXISTS idx_products_price ON products;
```

Indexes accelerate:

- **Point lookups**: `WHERE id = 42`
- **Range scans**: `WHERE price > 100` and `WHERE price BETWEEN 10 AND 50`
- **IN lists**: `WHERE id IN (1, 2, 3)`
- **Sorted queries**: `ORDER BY price` (B-tree maintains sort order)

Without an index, queries fall back to full table scans.

### Unique Constraint Enforcement

`PRIMARY KEY` and `UNIQUE` constraints are enforced at commit time:

```sql
CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE);

INSERT INTO users VALUES (1, 'alice@test.com');   -- OK
INSERT INTO users VALUES (1, 'bob@test.com');      -- ERROR: duplicate key on pk_users
INSERT INTO users VALUES (2, 'alice@test.com');    -- ERROR: duplicate key on uq_users_email
UPDATE users SET id = 1 WHERE email = 'bob@test.com'; -- ERROR if id=1 exists
```

### Data Manipulation

```sql
INSERT INTO users VALUES (1, 'Alice', 'alice@example.com', 30);
INSERT INTO users (id, name) VALUES (2, 'Bob');

SELECT * FROM users;
SELECT id, name FROM users WHERE age > 18;
SELECT * FROM users ORDER BY name DESC LIMIT 10 OFFSET 20;
SELECT name FROM users WHERE name LIKE 'A%' AND age BETWEEN 20 AND 40;
SELECT COUNT(*), AVG(age) FROM users;

UPDATE users SET email = 'new@example.com' WHERE id = 1;
DELETE FROM users WHERE id = 2;

EXPLAIN SELECT * FROM users WHERE age > 18;
EXPLAIN ANALYZE SELECT * FROM users WHERE age > 18;
```

### User Management

```sql
CREATE USER alice WITH PASSWORD 'secure_password';
CREATE USER bob PASSWORD 'another_password';

DROP USER alice;
DROP USER IF EXISTS alice;

ALTER USER bob WITH PASSWORD 'new_password';
```

### Permission Management

```sql
GRANT SELECT ON users TO alice;
GRANT SELECT(id, name) ON users TO alice;
GRANT INSERT ON users TO alice;
GRANT UPDATE(email) ON users TO alice;
GRANT DELETE ON users TO alice;
GRANT ALL ON users TO alice;

REVOKE SELECT ON users FROM alice;
REVOKE ALL ON users FROM alice;
```

### Data Types

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
| `UUIDV7` | Sortable UUIDv7 bytes | Generated with `UUIDV7()` |
| `U_SMALLINT` | 0 to 65,535 | Unsigned 16-bit integer |
| `U_INTEGER` | 0 to 4,294,967,295 | Unsigned 32-bit integer |
| `U_BIGINT` | 0 to 18,446,744,073,709,551,615 | Unsigned 64-bit integer |
