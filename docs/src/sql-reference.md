# SQL Reference

## Data Types

| SQL Type | Range | Storage | Notes |
|----------|-------|---------|-------|
| `BOOLEAN` | `true` / `false` | 1 byte | |
| `SMALLINT` | -32,768 to 32,767 | 2 bytes | 16-bit signed |
| `INTEGER` / `INT` | -2^31 to 2^31-1 | 4 bytes | 32-bit signed |
| `BIGINT` | -2^63 to 2^63-1 | 8 bytes | 64-bit signed |
| `REAL` | ~±3.4×10^38 | 4 bytes | 32-bit float, 6-9 digits precision |
| `DOUBLE` / `FLOAT` | ~±1.8×10^308 | 8 bytes | 64-bit float, 15-17 digits precision |
| `VARCHAR(n)` | Up to n chars | Variable | No length enforcement currently |
| `CHAR(n)` | Exactly n chars | Variable | Padded on output (future) |
| `TEXT` | Unlimited | Variable | |
| `DATE` | 1000-01-01 to 9999-12-31 | 4 bytes | ISO 8601 format |
| `TIME` | 00:00:00 to 23:59:59 | 4 bytes | |
| `TIMESTAMP` | 1000-01-01 to 9999-12-31 | 8 bytes | No timezone |
| `UUID` | Standard UUID | 16 bytes | Random UUID v4 |
| `UUIDV7` | Sortable UUID | 16 bytes | Time-ordered UUID v7 |
| `U_SMALLINT` | 0 to 65,535 | 2 bytes | 16-bit unsigned |
| `U_INTEGER` | 0 to 4,294,967,295 | 4 bytes | 32-bit unsigned |
| `U_BIGINT` | 0 to 2^64-1 | 8 bytes | 64-bit unsigned |

## Implicit Type Coercion

Values are automatically widened when the target column type differs:

| Source Type | Coercible To |
|-------------|-------------|
| `SMALLINT` | `INTEGER`, `BIGINT`, `REAL`, `DOUBLE` |
| `INTEGER` | `BIGINT`, `REAL`, `DOUBLE` |
| `BIGINT` | `REAL`, `DOUBLE` |
| `REAL` | `DOUBLE` |
| `U_SMALLINT` | `U_INTEGER`, `U_BIGINT` |
| `U_INTEGER` | `U_BIGINT` |
| `VARCHAR` / `TEXT` | `VARCHAR`, `TEXT` (same-string-type only) |

Narrowing conversions (e.g., `BIGINT` → `INTEGER`) are NOT automatic and will produce a `TypeMismatch` error.

## NULL Behavior

- **Comparisons**: Any comparison with `NULL` yields `Datum::Null` (three-valued logic). `WHERE` clauses treat `Null` as false.
- **ORDER BY**: `NULL` values sort last (largest).
- **Aggregates**: `COUNT(*)` counts all rows. `COUNT(col)` counts non-null values. `SUM`, `AVG`, `MIN`, `MAX` skip nulls.
- **Arithmetic**: Any arithmetic with `NULL` yields `NULL`.
- **IS NULL / IS NOT NULL**: Use these to test for null, not `= NULL`.

## DDL — Data Definition Language

### CREATE TABLE

```sql
CREATE TABLE [IF NOT EXISTS] name (
    column_name data_type [PRIMARY KEY] [NOT NULL] [UNIQUE] [DEFAULT expr],
    ...
) [ENGINE = memory | disk]
```

- `PRIMARY KEY` creates a unique NOT NULL index; only one column may be PK
- `UNIQUE` creates a unique index on the column
- `NOT NULL` rejects null insertions/updates
- `DEFAULT expr` — only literal defaults supported
- `ENGINE` defaults to the server's `--default-engine` setting

### DROP TABLE

```sql
DROP TABLE [IF EXISTS] name
```

### ALTER TABLE ... ENGINE

```sql
ALTER TABLE name ENGINE = memory | disk
```

Migrates all data between storage engines. Blocks during migration.

## DML — Data Manipulation Language

### INSERT

```sql
INSERT INTO name [(col1, col2, ...)] VALUES (val1, val2, ...), (val1, val2, ...), ...
```

- Missing columns are filled with `NULL` (or the column's `DEFAULT`)
- Values are type-coerced to match column types

### SELECT

```sql
SELECT [col1, col2, ... | *]
FROM name
[WHERE expr]
[ORDER BY col [ASC | DESC] [, ...]]
[LIMIT n]
[OFFSET n]
```

- `WHERE` supports `=`, `<>`, `<`, `>`, `<=`, `>=`, `AND`, `OR`, `NOT`, `IN`, `BETWEEN`, `LIKE`, `IS NULL`, `IS NOT NULL`
- `ORDER BY` supports multiple columns and mixed directions
- `LIKE`: `%` matches any sequence, `_` matches any single character
- Aggregates (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`) in the column list apply to the whole result set
- No `GROUP BY` support yet; all aggregates are over the full filtered set

### UPDATE

```sql
UPDATE name SET col1 = val1 [, col2 = val2, ...] [WHERE expr]
```

- Without `WHERE`, updates ALL rows
- Values are type-coerced
- `PRIMARY KEY` / `UNIQUE` constraints are enforced on commit

### DELETE

```sql
DELETE FROM name [WHERE expr]
```

- Without `WHERE`, deletes ALL rows

## Index SQL

```sql
-- Auto-created on PRIMARY KEY and UNIQUE columns

-- User-defined index
CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table (col1 [, col2, ...])

-- Drop
DROP INDEX [IF EXISTS] name ON table
```

### When Indexes Are Used

The executor uses an index when the `WHERE` clause contains:

| Pattern | Index Used? |
|---------|-------------|
| `col = literal` | ✅ Point lookup (exact match) |
| `col > literal`, `col < literal`, `col >=`, `col <=` | ✅ Range scan |
| `col BETWEEN a AND b` | ✅ Range scan |
| `col IN (a, b, c)` | ✅ Multi-point lookup |
| `ORDER BY col` | ✅ Sorted scan (B-tree maintains order) |
| Any other pattern | ❌ Full table scan |

Without a matching index, queries fall back to scanning all version chains (full table scan). Use `EXPLAIN` to check whether an index is used.

### EXPLAIN / EXPLAIN ANALYZE

```sql
EXPLAIN SELECT * FROM users WHERE age > 18;
EXPLAIN ANALYZE SELECT * FROM users WHERE age > 18;
```

`EXPLAIN` shows the logical plan (scan type, filters, sort order). `EXPLAIN ANALYZE` also executes the query and includes rows-affected counts.

## User Management

```sql
CREATE USER name PASSWORD 'password'
CREATE USER name WITH PASSWORD 'password'

ALTER USER name [WITH] PASSWORD 'new_password'

DROP USER [IF EXISTS] name
```

Passwords are hashed with Argon2id before storage.

## Permissions

```sql
-- Grant
GRANT { SELECT [(cols)] | INSERT [(cols)] | UPDATE [(cols)] | DELETE | ALL } ON table TO user

-- Revoke
REVOKE { SELECT [(cols)] | INSERT [(cols)] | UPDATE [(cols)] | DELETE | ALL } ON table FROM user
```

- Columns are optional; without them, the permission applies to all columns
- `ALL` grants SELECT, INSERT, UPDATE, and DELETE on all columns
- Permissions are checked by `execute_as()` when a `current_user` is provided
- `execute()` (without user) skips all permission checks

## Functions

### Aggregate Functions

| Function | Description |
|----------|-------------|
| `COUNT(*)` | Count all rows |
| `COUNT(expr)` | Count non-null values of expression |
| `SUM(expr)` | Sum of numeric values |
| `AVG(expr)` | Average of numeric values |
| `MIN(expr)` | Minimum value (uses `Datums` ordering) |
| `MAX(expr)` | Maximum value |

### Scalar Functions

| Function | Description |
|----------|-------------|
| `LOWER(str)` / `LCASE(str)` | Lowercase string |
| `UPPER(str)` / `UCASE(str)` | Uppercase string |
| `LENGTH(str)` / `LEN(str)` | Character count |
| `COALESCE(a, b, ...)` | First non-null argument |
| `IFNULL(a, b)` | `b` if `a` is null, else `a` |
| `ABS(num)` | Absolute value |
| `ROUND(num [, decimals])` | Round to decimal places (default 0) |
| `UUIDV7()` | Generate a time-ordered UUID v7 |

## Reserved Keywords

Keywords specific to HelionDB (in addition to standard SQL reserved words):

```
ENGINE, UUIDV7, U_SMALLINT, U_INTEGER, U_BIGINT
```
