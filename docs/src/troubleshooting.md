# Troubleshooting & FAQ

## Common Errors

### "I forgot the HELION_PASSWORD"

The password is hashed with Argon2id — there is no recovery. Options:
1. Delete the data directory and start fresh (all data is lost)
2. If you have a backup, restore it and use the password from that environment

For the future, store the password in a password manager or secrets vault.

### "Connection refused"

HelionDB uses **QUIC over UDP** — not TCP. Verify:

```bash
# Check if the server is listening
ss -uanp 'sport = 9613'

# Test with a QUIC-capable tool
# (or use the Rust client library)
```

Common causes:
- Server is not running
- Wrong port (default is `9613`, not `5432`)
- Firewall blocking UDP 9613
- Server bound to `127.0.0.1` but client connecting to external IP

### "Transaction conflict"

```
Error: Transaction error: conflict with transaction 42
```

This is normal under concurrent write load. HelionDB uses **optimistic concurrency control** — conflicts are detected at commit time, not ahead of time. Your application should retry the transaction:

```rust
loop {
    let tx = engine.begin();
    // ... perform reads and writes ...
    match engine.commit(tx).await {
        Ok(()) => break,
        Err(HelionError::Conflict(_)) => continue, // retry
        Err(e) => return Err(e),
    }
}
```

To reduce conflicts:
- Keep transactions short
- Avoid concurrent writes to the same rows
- Use higher checkpoint intervals if writes are bursty

### "Permission denied"

```sql
-- Check what permissions exist
GRANT SELECT ON users TO alice;                    -- all columns
GRANT SELECT(id, name) ON users TO alice;           -- specific columns
GRANT ALL ON users TO alice;                        -- everything

-- Verify using exec_as (not exec — exec skips checks)
exec_as(&engine, "SELECT * FROM users", "alice").await;
```

Note: `execute()` (no user) always succeeds — only `execute_as()` applies permission checks.

### "Duplicate key violation"

```
Error: Duplicate key violation on index 'pk_users': 42
```

PRIMARY KEY and UNIQUE constraints are enforced at commit time. To diagnose:
- Check if the row already exists: `SELECT * FROM users WHERE id = 42`
- Check if you're updating a row to a value that conflicts with another row
- Use `INSERT ... ON CONFLICT` is not yet supported — check first or use a transaction

### WAL File Corruption

CRC32 checksums protect each WAL record. On corruption during recovery:
- Corrupted records are **skipped** with a warning
- Data from before the corruption point is preserved
- Data after the corruption point is lost

To minimize impact:
- Set `--checkpoint-interval` to a reasonable value (default 60s)
- Run regular cold backups
- Monitor logs for CRC32 warnings

### "Table not found"

```sql
CREATE TABLE users (id INTEGER);
DROP TABLE IF EXISTS non_existent;   -- OK
DROP TABLE non_existent;             -- Error: Table 'non_existent' not found
```

Use `IF EXISTS` / `IF NOT EXISTS` clauses for idempotent operations.

### Slow Queries

Possible causes:
1. **No index** — Full table scan. Add an index: `CREATE INDEX idx_col ON table (col)`
2. **Disk engine** — Disk engine is slower than memory engine. Consider `ALTER TABLE t ENGINE = memory`
3. **Large result sets** — Add `LIMIT` and `OFFSET`
4. **Heavy WAL writes** — Use `--durability async` (default) instead of `sync`

Diagnose with:
```sql
EXPLAIN SELECT * FROM users WHERE age > 18;
EXPLAIN ANALYZE SELECT * FROM users WHERE age > 18;
```

## FAQ

### Can I connect with psql?

No. HelionDB uses a **custom QUIC-based protocol**, not the PostgreSQL wire protocol. Use the Rust client library or implement the protocol (see [Wire Protocol](protocol.md)).

### Is HelionDB production-ready?

HelionDB is v0.1.0 — it's under active development. While the core features work (SQL, MVCC, persistence, authentication), it lacks production features like replication, online backup, and monitoring integrations.

### What's the difference between memory and disk engines?

- **Memory**: All data in RAM. Fastest possible reads/writes. Data survives restarts only via WAL replay.
- **Disk**: Each table persisted to a `table.bin` file. Slower but uses less RAM for large datasets.

Per-table engine selection lets you optimize: keep hot indexes in `memory`, archive tables on `disk`.

### How do I reset everything?

```bash
# Stop the server, then:
rm -rf /var/lib/heliondb
# Start fresh — data directory is recreated on startup
```

### Does HelionDB support JOINs?

Not yet. The SQL parser supports single-table queries only. Cross-table queries can be simulated in application code with multiple queries and client-side joining.

### How do I file a bug report?

Open an issue at [github.com/gxcloud/heliondb/issues](https://github.com/gxcloud/heliondb/issues). Include:

- HelionDB version (`./target/release/heliondb --version`)
- Operating system and Rust version
- Steps to reproduce (SQL statements, client code)
- Output with `RUST_LOG=heliondb=debug`
- WAL file (if relevant and not containing sensitive data)

## Debugging Checklist

1. Check server logs: `journalctl -u heliondb -n 50 --no-pager` or `RUST_LOG=debug`
2. Verify the server is listening: `ss -uanp 'sport = 9613'`
3. Test authentication: verify `HELION_PASSWORD` is set correctly
4. Check SQL syntax: `EXPLAIN` the query first
5. Check permissions: use `execute_as()` not `execute()`
6. Check for indexes: `EXPLAIN SELECT ...` shows scan type
7. Check engine: `ALTER TABLE t ENGINE = memory` for performance comparison
8. Isolate the issue: try the same SQL in a fresh `cargo test` with `setup()`
