# Changelog

## [0.1.0] - 2026-05-13

### Added
- SQL: CREATE/DROP/ALTER TABLE, INSERT/SELECT/UPDATE/DELETE, EXPLAIN
- Storage: memory and disk engines, MVCC + snapshot isolation, WAL + checkpoints
- Indexes: B-tree, auto-index on PK/UNIQUE, CREATE/DROP INDEX SQL
- Users & permissions: CREATE/ALTER/DROP USER, GRANT/REVOKE column-level
- Network: QUIC transport (quinn), TLS 1.3, bincode wire protocol
- CLI: configurable data-dir, listen address, certs, durability, engine
- CI: check, test, build, coverage, docs deployment
