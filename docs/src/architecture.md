# Architecture

## Overview

HelionDB is a SQL database with PostgreSQL-compatible syntax, MVCC concurrency control, async WAL persistence, pluggable storage engines, and QUIC transport.

```text
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
                    │  MVCC + WAL + auth  │
                    └─────────────────────┘
```

## Layer 1: Storage Engine

### Engine Routing

**Files**: `src/storage/catalog.rs`, `src/storage/engine_trait.rs`, `src/storage/engines/*`

- The catalog stores table-to-engine metadata plus the database default engine.
- `memory` keeps tables in RAM.
- `disk` stores each table under its own directory on disk.
- `ALTER TABLE ... ENGINE = ...` migrates a table by snapshotting, restoring, and atomically switching the catalog entry.

### MVCC

**File**: `src/storage/mvcc.rs`

HelionDB implements snapshot isolation using version chains.

- Transactions get a monotonically increasing `tx_id`.
- Each transaction captures a snapshot of all active transactions.
- Each logical row is stored as a chain of row versions.
- A version is visible if it was created before the snapshot, not deleted before the snapshot, and the creator was already committed.

### Optimistic Concurrency Control

When a transaction commits:

1. Conflicts are checked against the snapshot active set.
2. Old versions are marked with `txid_max`.
3. New versions are appended.
4. All writes are appended to the WAL.

### WAL

**File**: `src/storage/wal.rs`

- Append-only binary file with length-prefixed, bincode-serialized records.
- Each record has a CRC32 checksum.
- On startup, WAL is replayed to reconstruct full state.

### Checkpoints

**File**: `src/storage/checkpoint.rs`

A background task writes a full snapshot of all tables periodically. On restart, the latest checkpoint is loaded first, then remaining WAL records are applied.

## Layer 2: SQL Layer

### Parser

**File**: `src/sql/parser.rs`

- Uses `sqlparser-rs` with `PostgreSqlDialect` for standard SQL.
- A custom fallback handles database-specific statements.
- Parsed statements are converted into `HelionStatement` variants.

### Planner

**File**: `src/sql/planner.rs`

- Converts `HelionStatement` to `LogicalPlan`.
- Resolves column names against table schemas.
- Type-checks `INSERT` and `UPDATE` values.

## Layer 3: Executor

### Expression Evaluator

**File**: `src/executor/eval.rs`

- Recursive expression evaluation against a row.
- Supports comparisons, arithmetic, logical ops, `IS NULL`, `IN`, `BETWEEN`, and `LIKE`.
- Built-in functions include `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `LOWER`, `UPPER`, `LENGTH`, `COALESCE`, `IFNULL`, `ABS`, `ROUND`, and `UUIDV7`.

### Physical Operators

**File**: `src/executor/ops.rs`

- `execute(engine, plan)` executes without permission checks.
- `execute_as(engine, plan, current_user)` executes with column-level permission checks.

## Layer 4: Server & Protocol

### Protocol

**File**: `src/protocol/messages.rs`

Custom binary protocol over QUIC with length-prefixed bincode payloads.

### Session Management

**File**: `src/protocol/auth.rs`

- Session tokens are `u64` values.
- Active sessions are stored behind a `RwLock`.

### QUIC Server

**File**: `src/server/quic.rs`

- Uses `quinn` for transport.
- Supports auto-generated self-signed certs or user-provided PEM files.
- Default listen address is `127.0.0.1:9613`.
