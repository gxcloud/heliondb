# Architecture

## Overview

HelionDB is a networked SQL database with pluggable per-table storage engines, MVCC snapshot isolation, async WAL persistence, and QUIC transport.

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
                    │  (multi-database)   │
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
                    │  disk / memory      │
                    └─────────────────────┘
```

## Layer 1: Storage Engine

### Engine Routing

**Files**: `src/storage/catalog.rs`, `src/storage/engine_trait.rs`, `src/storage/engines/*`

- The catalog stores table-to-engine metadata plus the database default engine.
- `disk` (primary) persists tables via append-only delta files with periodic compaction.
- `memory` keeps tables in RAM (ephemeral, for caching or temp data).
- `ALTER TABLE ... ENGINE = ...` migrates a table by snapshotting, restoring, and atomically switching the catalog entry.

### MVCC

**File**: `src/storage/mvcc.rs`

HelionDB implements snapshot isolation using version chains.

- Transactions get a monotonically increasing `tx_id`.
- Each transaction captures a snapshot of all active transactions.
- Each logical row is stored as a chain of row versions.
- A version is visible if it was created before the snapshot, not deleted before the snapshot, and the creator was already committed.

### Indexes

**File**: `src/storage/btree.rs`

B-tree indexes use `std::collections::BTreeMap` for `O(log n)` point lookups and range scans. Each index maps key values (`Vec<Datum>`) to a set of row indices (`BTreeSet<usize>`).

- **Auto-indexing**: Primary key and `UNIQUE` columns get automatic unique indexes at table creation.
- **User-defined indexes**: Created via `CREATE INDEX` / `DROP INDEX` SQL.
- **Serialization**: Indexes are part of `Table` and are serialized with it via bincode for WAL checkpoints and disk engine persistence.
- **Startup rebuild**: After WAL replay, indexes are rebuilt from version chain data to ensure consistency.

### Optimistic Concurrency Control

When a transaction commits:

1. Conflicts are checked against the snapshot active set.
2. Unique constraints are checked against all unique indexes.
3. Old versions are marked with `txid_max`.
4. New versions are appended.
5. All writes are appended to the WAL.
6. Indexes are updated atomically with version chains.

### WAL

**File**: `src/storage/wal.rs`

- Append-only binary file with length-prefixed, bincode-serialized records.
- Each record has a CRC32 checksum.
- On startup, WAL is replayed to reconstruct full state.

### Disk Engine (Delta Files)

**File**: `src/storage/engines/disk.rs`

The disk engine uses an append-only delta design for O(#changes) write cost instead of O(total rows):

- Each mutation appends a `delta_{txid}.bin` file containing only the changed rows.
- A `base_{txid}.bin` snapshot is written periodically (compaction) for fast startup.
- On startup, the latest base is loaded and incremental deltas applied.
- Per-table atomicity via tmp+rename for each delta file.

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
- **Index-aware SELECT**: The executor pattern-matches `WHERE` clauses against available indexes. For point lookups (`col = val`), range scans (`col > val`, `BETWEEN`), and `IN` lists, rows are fetched via the B-tree index instead of scanning all version chains. Falls back to full scan when no index matches.
- **Index DDL**: `CREATE INDEX` and `DROP INDEX` are executed by modifying the table's index list and repopulating from existing data.

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

## Deep Dives

### MVCC Version Chains

Each logical row is stored as a **version chain** — a `Vec<RowVersion>` where each entry tracks its creating and deleting transaction:

```rust
pub struct RowVersion {
    txid_min: u64,   // creating transaction
    txid_max: u64,   // deleting/overwriting transaction (u64::MAX = current)
    row: Row,        // the actual data
    is_deleted: bool,// tombstone marker
}
```

**Version chain evolution over time (row with id=1):**

```text
Step 1: INSERT INTO users VALUES (1, 'Alice', 30)      [txid=5]
  Chain[0]: [txid_min=5, txid_max=MAX, row=(1, Alice, 30), deleted=false]

Step 2: UPDATE users SET age = 31 WHERE id = 1          [txid=10]
  Chain[0]: [txid_min=5,  txid_max=10, row=(1, Alice, 30), deleted=false]  ← old
  Chain[0]: [txid_min=10, txid_max=MAX, row=(1, Alice, 31), deleted=false]  ← current

Step 3: DELETE FROM users WHERE id = 1                   [txid=15]
  Chain[0]: [txid_min=5,  txid_max=10, row=(1, Alice, 30), deleted=false]  ← old
  Chain[0]: [txid_min=10, txid_max=15, row=(1, Alice, 31), deleted=false]  ← superseded
  Chain[0]: [txid_min=15, txid_max=MAX, row=(1, Alice, 31), deleted=true]  ← tombstone
```

**Visibility algorithm** (implemented in `is_version_visible`):

```text
A version is VISIBLE to a snapshot if ALL of these hold:
  1. txid_min ≤ snapshot.txid       (version existed at snapshot time)
  AND
  2. snapshot.txid < txid_max        (version wasn't yet deleted at snapshot time)
  AND (one of:)
  3a. txid_min NOT IN snapshot.active  (creator was already committed)
  3b. txid_min == self_txid           (we created this version ourselves)
```

This means a transaction sees:
- All committed changes that existed before its snapshot
- Its own uncommitted changes
- It does NOT see uncommitted changes from other transactions
- It does NOT see changes committed after its snapshot

**Snapshot isolation guarantees:**

| Phenomenon | Prevented? |
|-----------|-----------|
| Dirty read | ✅ (never see uncommitted data) |
| Non-repeatable read | ✅ (same snapshot → same data) |
| Phantom read | ✅ (snapshot locks in the row set) |
| Lost update | ✅ (OCC detects conflicts) |
| Write skew | ❌ (not detected — see OCC below) |

### Optimistic Concurrency Control (OCC)

HelionDB uses optimistic concurrency control — no locks are held during the transaction. Conflicts are detected at commit time:

```text
function check_conflicts(transaction, all_tables):
  for each entry in transaction.write_set:
    table = all_tables[entry.table_name]
    version_chain = table.version_chains[entry.row_idx]
    latest_version = version_chain.last()
    
    if latest_version.txid_min in transaction.snapshot.active:
      return CONFLICT(conflicting_txid = latest_version.txid_min)
  
  return NO_CONFLICT
```

**Example conflict scenario:**

```text
Transaction A (txid=10)           Transaction B (txid=11)
│                                 │
├─ Snapshot: active={}            ├─ Snapshot: active={10}
│                                 │
├─ UPDATE row 5 set x=1          ├─ UPDATE row 5 set x=2
│  (reads latest=txid=3)          │  (reads latest=txid=10)
│                                 │
├─ COMMIT ───────────────────────►│
│  check_conflicts:               │
│    row 5 latest = txid=3        ├─ COMMIT ───────────────────────►│
│    txid=3 NOT in active={}      │   check_conflicts:
│    → CONFLICT-FREE ✓            │     row 5 latest = txid=10
│  (applies version chain)        │     txid=10 IN active={10}
│                                 │     → CONFLICT ✗
│                                 │   (transaction aborted)
```

Transaction A succeeds, B is aborted with `HelionError::Conflict(10)`.

### WAL Binary Format

```text
Byte offset  Field                  Description
───────────  ─────────────────────  ──────────────────────────
0-3          payload_length (u32)   Length of payload in bytes (big-endian)
4-7          crc32 (u32)            CRC32 checksum of the payload
8+           payload                Bincode-serialized WalRecord
```

**Record types** (`WalRecord` enum):

| Variant | Payload Contains | Purpose |
|---------|-----------------|---------|
| `CreateTable` | name, columns | DDL logging |
| `DropTable` | name | DDL logging |
| `Insert` | table name, Row, txid | DML logging |
| `Update` | table name, row_idx, new Row, txid | DML logging |
| `Delete` | table name, row_idx, txid | DML logging |
| `Commit` | txid | Transaction commit marker |
| `Checkpoint` | table_count, full table data | Fast recovery snapshot |
| `CreateUser` | username, password_hash | User management |
| `DropUser` | username | User management |
| `Grant` | username, table, permission | Permissions |
| `Revoke` | username, table, permission | Permissions |

**Recovery algorithm:**

```text
1. Scan data_dir for helion.wal
2. Read all records sequentially:
   a. Read 4 bytes → payload_length
   b. Read 4 bytes → expected_crc32
   c. Read payload_length bytes → payload
   d. Compute CRC32 of payload, compare to expected
   e. If mismatch: log warning, skip to next record
   f. If match: deserialize WalRecord, apply to table state
3. Scan all version chains → compute max_txid
4. Rebuild all indexes from version chain data
5. Restore tables into their assigned storage engines
6. Replay user and permission records from WAL
7. Bootstrap default admin user from HELION_PASSWORD
8. Spawn checkpoint background loop
```

Corrupted records beyond recovery are **skipped** with a warning — data before the corruption point is preserved.

### Checkpoint Lifecycle

A background task (spawned by `DatabaseEngine::open()`) periodically writes a `Checkpoint` WAL record containing a full snapshot of all in-memory tables:

```text
Time ───────────────────────────────────────────────────────►
     │           │           │           │
     ├─ WAL ─────┼─ WAL ─────┼─ WAL ─────┼─ WAL ── ...
     │           │           │           │
     └── Checkpoint ─────────┘── Checkpoint ── ...
          (t=60s)                  (t=120s)
```

Benefits:
- **Faster recovery**: Only replay records after the last checkpoint
- **WAL size control**: Checkpoints reset the "effective starting point" for recovery

Tradeoff:
- Shorter interval → faster recovery, more I/O
- Longer interval → less I/O, slower recovery

### Startup Sequence (Full)

```text
DatabaseEngine::open(data_dir)
│
├─ 1. Open catalog → load TableMeta (engine assignments)
│     File: helion.catalog (bincode-serialized)
│
├─ 2. Replay WAL → reconstruct Vec<Table>
│     File: helion.wal
│     ├─ Load checkpoint records (full table snapshots)
│     └─ Apply incremental records (DDL + DML)
│
├─ 3. Compute max_txid from version chains
│     Scan all tables, all version chains → highest txid
│
├─ 4. Rebuild indexes from version chain data
│     For each table, for each index:
│       scan_visible → extract_key → insert into BTreeMap
│
├─ 5. Restore tables into storage engines
│     For each table with engine=X:
│       catalog.get_engine(X).restore_table(table)
│
├─ 6. Replay user + permission records from WAL
│     User records → UserStore
│     Permission records → PermissionStore
│
├─ 7. Bootstrap default admin user
│     If HELION_PASSWORD is set and no users exist:
│       create "helion" user with Argon2id hashed password
│
└─ 8. Spawn checkpoint background loop
       Every N seconds (default 60):
         write_checkpoint(data_dir, tables, wal_writer)
```

### Commit Pipeline (Detail)

```text
engine.commit(tx)
│
├─ 1. MvccStore::check_conflicts(tx, tables)
│     Detects write-write conflicts using OCC
│     Returns Err(Conflict(n)) if conflict found
│
├─ 2. Table::check_unique_constraints(tx, tables)
│     For each changed row in each table:
│       Check all unique indexes for duplicate keys
│     Returns Err(DuplicateKey) on violation
│
├─ 3. MvccStore::apply_write_set(tx.write_set, tables)
│     Computes new version chains for all modified rows
│     Marks old versions with txid_max
│     Appends new RowVersions
│
├─ 4. StorageEngine::apply_write_set(table, changes)
│     Engine-specific persistence:
│       MemoryEngine: direct in-memory mutation
│       DiskEngine: clone table → mutate → persist to table.bin
│
├─ 5. Update in-memory tables + indexes
│     Replace version chains with computed ones
│     Update B-tree index entries
│
├─ 6. WalWriter::append(record) × N
│     One WAL record per Insert/Update/Delete operation
│     Serializes → computes CRC32 → appends to helion.wal
│
├─ 7. WalWriter::append(Commit { tx.tx_id })
│     Commit marker signals transaction durability
│
└─ 8. MvccStore::commit_transaction(tx)
      Removes tx_id from active_txns set
```

Steps 1-5 are reverted on error (no WAL written → no persistence). Steps 6-7 are the durability point — if the process crashes after step 7, the WAL will replay the committed writes.

### Engine Migration (ALTER TABLE ... ENGINE)

```text
catalog.migrate_table(name, target_engine)
│
├─ 1. SNAPSHOT: source_engine.snapshot_table(name)
│     Reads all version chains from the source engine
│     Returns a complete Table struct
│
├─ 2. CREATE: target_engine.create_table(meta, columns)
│     Creates an empty table structure in the target
│
├─ 3. RESTORE: target_engine.restore_table(table)
│     Writes the snapshot data into the target engine
│
├─ 4. DROP: source_engine.drop_table(name)
│     Removes the table from the source engine
│
├─ 5. UPDATE CATALOG: update TableMeta.engine entry
│     Persists the new engine assignment
│
└─ On failure at any step:
      Roll back by reversing completed operations
      (e.g., if restore fails, drop the newly created target table)
```

### QUIC Transport Architecture

HelionDB uses QUIC (via `quinn`) rather than TCP+TLS for several reasons:

| Feature | Benefit |
|---------|---------|
| 0-RTT handshake | Faster connection establishment for repeated connections |
| Stream multiplexing | Multiple queries on one connection without head-of-line blocking |
| Built-in TLS 1.3 | No separate TLS layer — encryption is mandatory and built-in |
| Connection migration | Survives network changes (IP address changes, NAT rebinding) |

**Connection model:**

```text
QUIC Connection (1 per client)
│
├── Bidi Stream 1: Auth → Query → QueryResult
├── Bidi Stream 2: Query → QueryResult  (parallel!)
├── Bidi Stream 3: Prepare → Execute → QueryResult
└── ...
```

Each bidirectional stream is independent — one slow query doesn't block others.

**Server defaults:**
- `max_concurrent_bidi_streams`: 100 (configurable via `quinn`)
- `max_idle_timeout`: 30 seconds
- Listen: UDP 9613
