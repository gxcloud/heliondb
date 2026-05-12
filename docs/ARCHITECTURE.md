# HelionDB Architecture

## Overview

HelionDB is a SQL database with PostgreSQL-compatible syntax, MVCC concurrency control, async WAL persistence, pluggable storage engines, and QUIC transport. The architecture follows a layered design with clear separation of concerns.

```
                    ┌─────────────────────┐
                    │      Client         │
                    │  (QUIC connection)  │
                    └─────────┬───────────┘
                              │
                    ┌─────────▼───────────┐
                    │     Protocol        │
                    │  (bincode frames)   │
                    └─────────┬───────────┘
                              │
                    ┌─────────▼───────────┐
                    │      Server         │
                    │  (quinn + session)  │
                    └─────────┬───────────┘
                              │
                    ┌─────────▼───────────┐
                    │      SQL Layer      │
                    │  parser → planner   │
                    └─────────┬───────────┘
                              │
                    ┌─────────▼───────────┐
                    │     Executor        │
                    │  (eval + ops)       │
                    └─────────┬───────────┘
                              │
                    ┌─────────▼───────────┐
                    │   Storage Engine    │
                    │  MVCC + WAL + auth │
                    └─────────────────────┘
```

## Layer 1: Storage Engine

### Engine Routing

**Files**: `src/storage/catalog.rs`, `src/storage/engine_trait.rs`, `src/storage/engines/*`

- The catalog stores table-to-engine metadata plus the database default engine.
- `memory` keeps tables in RAM.
- `disk` stores each table under its own directory on disk.
- `ALTER TABLE ... ENGINE = ...` migrates a table by snapshotting, restoring, and atomically switching the catalog entry.

### MVCC (Multi-Version Concurrency Control)

**File**: `src/storage/mvcc.rs`

HelionDB implements snapshot isolation using version chains:

- **Transactions**: Each transaction gets a unique `tx_id` (monotonically increasing `u64`). When a transaction begins, it captures a **snapshot** of all currently-active transactions.
- **Snapshots**: `Snapshot { txid, active: BTreeSet<u64> }` — the set of transaction IDs that were in-flight when this transaction started.
- **Version Chains**: Each logical row is stored as a `Vec<RowVersion>`. A `RowVersion` has:
  - `txid_min`: the transaction that created this version
  - `txid_max`: the transaction that deleted/overwrote this version (or `u64::MAX` = ∞)
  - `row`: the actual row data
  - `is_deleted`: whether this is a tombstone (DELETE)
- **Visibility Rule**: A version is visible to a snapshot `S` if:
  1. `txid_min <= S.txid` (created at or before the snapshot)
  2. `txid_max > S.txid || txid_max == u64::MAX` (not yet deleted at snapshot time)
  3. `txid_min ∉ S.active` (creator was committed, not still in-flight)

### Optimistic Concurrency Control

When a transaction commits:

1. **Conflict Detection** (`check_conflicts`): For each row in the write set, if the latest version's `txid_min` is in the committing transaction's snapshot active set, another transaction committed concurrently → `Err(Conflict)`.
2. **Write Application** (`apply_write_set`): Old versions get `txid_max = committing_txid`, new versions are appended.
3. **WAL Append**: All writes are appended to the WAL.

### WAL (Write-Ahead Log)

**File**: `src/storage/wal.rs`

- **Format**: Append-only binary file with length-prefixed, bincode-serialized records.
- **Integrity**: Each record has a CRC32 checksum (via `crc32fast`).
- **Record Types**: `CreateTable`, `DropTable`, `Insert`, `Update`, `Delete`, `Commit`, `Checkpoint`, `CreateUser`, `DropUser`, `Grant`, `Revoke`
- **Replay**: On startup, WAL is replayed from beginning to reconstruct full state.
- **Checkpoints**: Periodic full snapshots via `CHECKPOINT_LOOP` background task.

### Checkpoints

**File**: `src/storage/checkpoint.rs`

A background task runs every 60 seconds (configurable) and writes a `Checkpoint` WAL record containing a full serialized snapshot of all tables. On restart, the checkpoint allows skipping ahead in the WAL rather than replaying from the beginning.

## Layer 2: SQL Layer

### Parser

**File**: `src/sql/parser.rs`

- **Primary**: Uses `sqlparser-rs` with `PostgreSqlDialect` for standard SQL.
- **Custom Fallback**: For non-standard statements (`CREATE USER`, `GRANT`, `CREATE TABLE ... ENGINE`, `ALTER TABLE ... ENGINE`, etc.), a custom parser handles the dialect.
- **AST**: Parsed statements are converted to `HelionStatement` enum variants.
- **Expressions**: Full expression tree with `BinaryOp`, `UnaryOp`, `Literal`, `Column`, `Function`, `IsNull`, `IsNotNull`, `In`, `Between`, `Like`.

### Planner

**File**: `src/sql/planner.rs`

- Converts `HelionStatement` → `LogicalPlan`
- Resolves column names to indices against table schemas
- Type-checks INSERT/UPDATE values
- Handles wildcard (`*`) expansion

## Layer 3: Executor

### Expression Evaluator

**File**: `src/executor/eval.rs`

- Recursive expression evaluation against a row of data
- Supports: comparisons, arithmetic, logical ops, `IS NULL`, `IN`, `BETWEEN`, `LIKE`
- Built-in functions: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `LOWER`, `UPPER`, `LENGTH`, `COALESCE`, `IFNULL`, `ABS`, `ROUND`

### Physical Operators

**File**: `src/executor/ops.rs`

- **`execute(engine, plan)`**: Executes a plan without permission checks (embedded library use).
- **`execute_as(engine, plan, current_user)`**: Executes with column-level permission checks.
- Operations: `CREATE TABLE`, `DROP TABLE`, `ALTER TABLE ... ENGINE`, `INSERT`, `SELECT` (with WHERE/ORDER BY/LIMIT/OFFSET), `UPDATE`, `DELETE`
- User operations: `CREATE USER`, `DROP USER`, `ALTER USER`, `GRANT`, `REVOKE`

## Layer 4: Server & Protocol

### Protocol

**File**: `src/protocol/messages.rs`

Custom binary protocol over QUIC:

- **Framing**: Each message is length-prefixed (4 bytes big-endian) + bincode payload.
- **Client Messages**: `Auth { username, password }`, `Query { sql, token }`, `Prepare { sql, token }`, `Execute { prepared_id, params, token }`
- **Server Messages**: `AuthResult { success, token }`, `QueryResult { columns, rows }`, `Error { message }`

### Session Management

**File**: `src/protocol/auth.rs`

- Tokens are `u64`, generated via `AtomicU64`
- Active sessions stored in `HashMap<u64, String>` behind `parking_lot::RwLock`
- Token verified on each query message

### QUIC Server

**File**: `src/server/quic.rs`

- Uses `quinn` crate for QUIC transport
- TLS with auto-generated self-signed certificates (via `rcgen`) or user-provided PEM files
- Default listen: `127.0.0.1:9613`
- Each connection spawns a task; each bidirectional stream spawns a sub-task

## Layer 5: User & Permission System

### Users

**File**: `src/storage/users.rs`

- Passwords hashed with Argon2id (via `argon2` crate)
- Case-insensitive username matching
- Stored in WAL via `CreateUser`/`DropUser` records

### Permissions

**File**: `src/storage/permissions.rs`

- `Permission::Select(columns)`: SELECT on specific columns (empty = all columns)
- `Permission::Insert(columns)`: INSERT into specific columns
- `Permission::Update(columns)`: UPDATE specific columns
- `Permission::Delete`: DELETE table-level
- `Permission::All`: All operations, all columns
- Grants are stored as `HashMap<(username, tablename), Vec<Permission>>`
- Case-insensitive matching for both usernames and table names

### Permission Checks

Each DML operation in the executor checks:
- **SELECT**: All projected columns must be granted
- **INSERT**: All inserted columns must be granted
- **UPDATE**: All SET columns must be granted
- **DELETE`: Table-level DELETE grant required
- **GRANT ALL**: Implicitly covers all current and future columns

## Data Flow: Query Execution

```
Client                    Server                     Engine
  │                         │                          │
  ├─ Auth(username,pass)───►│                          │
  │                         ├── verify_user()─────────►│
  │                         │◄── true/false ──────────┤
  │◄─ AuthResult(token) ───┤                          │
  │                         │                          │
  ├─ Query(sql, token)─────►│                          │
  │                         ├── verify_token()         │
  │                         ├── parse(sql)             │
  │                         ├── plan(stmt, tables)     │
  │                         ├── execute_as(plan, user) │
  │                         │    ├─ permission check   │
  │                         │    ├─ MVCC read          │
  │                         │    ├─ WAL append         │
  │                         │    └─ result             │
  │◄─ QueryResult(rows) ───┤                          │
```

## Directory Structure

```
src/
├── main.rs                    # CLI entry point
├── lib.rs                     # Public API exports
├── error.rs                   # Unified error types
├── storage/
│   ├── mod.rs
│   ├── types.rs               # DataType, Datum, ColumnMeta, Row
│   ├── table.rs               # Table with MVCC version chains
│   ├── mvcc.rs                # Transaction management, OCC
│   ├── wal.rs                 # Write-ahead log
│   ├── checkpoint.rs          # Periodic snapshots
│   ├── engine_trait.rs        # StorageEngine trait + table metadata
│   ├── catalog.rs              # Table routing, default engine, migration
│   ├── engine.rs              # Database engine orchestrator
│   ├── engines/
│   │   ├── memory.rs           # RAM engine
│   │   └── disk.rs             # Per-table disk engine
│   ├── users.rs               # User management + password hashing
│   └── permissions.rs         # Column-level permission system
├── sql/
│   ├── mod.rs
│   ├── parser.rs              # SQL parser (sqlparser + custom fallback)
│   └── planner.rs             # Logical query planner
├── executor/
│   ├── mod.rs
│   ├── eval.rs                # Expression evaluator
│   └── ops.rs                 # Physical operators
├── server/
│   ├── mod.rs
│   ├── quic.rs                # QUIC server (quinn)
│   └── session.rs             # Connection + stream handling
└── protocol/
    ├── mod.rs
    ├── messages.rs            # Message types (Client/Server)
    └── auth.rs                # Session token management
```
