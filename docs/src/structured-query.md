# Structured Query Protocol

HelionDB supports a **Prisma-style structured query protocol** over QUIC alongside raw SQL. Instead of writing SQL strings, clients send a JSON object describing the desired operation, and the server automatically resolves foreign key relationships, builds query plans, and returns nested JSON responses.

## When to Use It

| Use Case | SQL | Structured Query |
|----------|-----|-----------------|
| Ad-hoc queries | ✅ Natural | ⚠️ Verbose |
| Complex JOINs | ⚠️ Error-prone | ✅ Auto-resolved via FK |
| CRUD from code | ⚠️ String building | ✅ Type-safe construction |
| Nested results | ❌ Manual grouping | ✅ Automatic nesting |

## Message Format

The structured query is sent as the `query_json` field of `ClientMessage::StructuredQuery` (bincode-framed, same as all other messages). The response is `ServerMessage::StructuredResult` with a JSON string in `data_json`.

## Operations

### findMany

Returns an array of matching objects.

**Request:**
```json
{
    "op": "findMany",
    "from": "users",
    "where": { "age": { "gte": 18 } },
    "select": ["name", "email"],
    "include": [
        { "relation": "orders", "where": { "total": { "gt": 100 } }, "take": 10 }
    ],
    "orderBy": [{ "field": "name", "direction": "asc" }],
    "take": 20,
    "skip": 0
}
```

**Response:**
```json
{
    "data": [
        {
            "name": "Alice",
            "email": "alice@x.com",
            "orders": [
                { "id": 10, "total": 250.0 }
            ]
        }
    ]
}
```

### findUnique

Returns a single object by its primary key (or `null` if not found).

**Request:**
```json
{
    "op": "findUnique",
    "from": "users",
    "where": { "id": 1 },
    "include": [{ "relation": "orders" }]
}
```

**Response:**
```json
{
    "data": { "id": 1, "name": "Alice", "orders": [...] }
}
```

### create

Inserts a new row.

**Request:**
```json
{
    "op": "create",
    "from": "users",
    "data": { "name": "Alice", "email": "alice@x.com", "age": 30 }
}
```

**Response:**
```json
{
    "data": { "name": "Alice", "email": "alice@x.com", "age": "30", "id": "1" }
}
```

### update

Updates matching rows.

**Request:**
```json
{
    "op": "update",
    "from": "users",
    "where": { "id": 1 },
    "data": { "name": "Alice Updated" }
}
```

**Response:**
```json
{
    "data": { "rows_affected": 1 }
}
```

### delete

Deletes matching rows.

**Request:**
```json
{
    "op": "delete",
    "from": "users",
    "where": { "id": 1 }
}
```

**Response:**
```json
{
    "data": { "rows_affected": 1 }
}
```

### upsert

Tries to update a matching row; if none exists, creates one.

**Request:**
```json
{
    "op": "upsert",
    "from": "users",
    "where": { "email": "alice@x.com" },
    "update": { "name": "Alice Updated" },
    "create": { "name": "Alice", "email": "alice@x.com", "age": 30 }
}
```

## Where Conditions

### Field Reference

```json
{ "field_name": condition }
```

### Operators

| Operator | JSON Example | SQL Equivalent |
|----------|-------------|----------------|
| Equality (shorthand) | `{ "name": "Alice" }` | `name = 'Alice'` |
| `gt` | `{ "age": { "gt": 18 } }` | `age > 18` |
| `gte` | `{ "age": { "gte": 18 } }` | `age >= 18` |
| `lt` | `{ "age": { "lt": 65 } }` | `age < 65` |
| `lte` | `{ "age": { "lte": 65 } }` | `age <= 65` |
| `ne` | `{ "status": { "ne": "banned" } }` | `status <> 'banned'` |
| `contains` | `{ "name": { "contains": "Ali" } }` | `name LIKE '%Ali%'` |
| `startsWith` | `{ "name": { "startsWith": "A" } }` | `name LIKE 'A%'` |
| `endsWith` | `{ "name": { "endsWith": "ce" } }` | `name LIKE '%ce'` |
| `in` | `{ "id": { "in": [1, 2, 3] } }` | `id IN (1, 2, 3)` |
| null (IS NULL) | `{ "email": null }` | `email IS NULL` |

### Logical Combinators

**AND:**
```json
{ "AND": [{ "age": { "gte": 18 } }, { "status": "active" }] }
```

**OR:**
```json
{ "OR": [{ "role": "admin" }, { "role": "moderator" }] }
```

**NOT:**
```json
{ "NOT": { "status": "banned" } }
```

**Combined:**
```json
{
    "AND": [
        { "age": { "gte": 18 } },
        { "OR": [{ "role": "admin" }, { "role": "mod" }] }
    ]
}
```

## Auto-JOIN via include

The `include` array tells the server to automatically JOIN related tables and nest the results.

```json
{
    "findMany": {
        "from": "users",
        "include": [
            { "relation": "orders", "where": { "total": { "gt": 100 } }, "orderBy": [{ "field": "createdAt", "direction": "desc" }], "take": 10 }
        ]
    }
}
```

### How it works

1. The server looks up the `orders` table's foreign key metadata
2. Finds `orders.user_id REFERENCES users(id)`
3. Auto-generates: `LEFT JOIN orders ON orders.user_id = users.id`
4. Applies the include's `where`, `orderBy`, and `take` as sub-filters
5. Groups the flat result rows into nested JSON: each user gets an `orders` array

### Foreign Key Declaration

Foreign keys are parsed from `CREATE TABLE`:

```sql
-- Inline syntax (column-level):
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER REFERENCES users(id),
    total DOUBLE
);

-- Table-level syntax:
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER,
    total DOUBLE,
    FOREIGN KEY (user_id) REFERENCES users(id)
);
```

The relationship is **stored as metadata** in the catalog. It is not enforced as a constraint — referential integrity is the application's responsibility.

### Convention-based Fallback

If no explicit `FOREIGN KEY` is found, the server falls back to naming convention:
- A column named `{parent_table}_id` (e.g., `user_id`) is assumed to reference `{parent_table}.id`

## Order By

```json
"orderBy": [{ "field": "name", "direction": "asc" }]
// or descending:
"orderBy": [{ "field": "createdAt", "direction": "desc" }]
// multiple:
"orderBy": [{ "field": "age", "direction": "desc" }, { "field": "name", "direction": "asc" }]
```

## Field Selection

```json
// All fields (default when omitted):
"select": null

// Specific fields:
"select": ["name", "email"]
```

## Pagination

```json
{
    "take": 10,
    "skip": 20
}
```

## Error Responses

```json
{
    "error": "Table 'nonexistent' not found"
}
```

## Client Implementation Guide

### TypeScript (pseudocode)

```typescript
class HelionDBClient {
    async findMany<T>(from: string, opts?: FindManyOptions): Promise<T[]> {
        const query = { op: "findMany", from, ...opts };
        const result = await this.sendStructuredQuery(query);
        return result.data;
    }

    async create(from: string, data: Record<string, any>): Promise<Record<string, any>> {
        const query = { op: "create", from, data };
        const result = await this.sendStructuredQuery(query);
        return result.data;
    }

    private async sendStructuredQuery(query: any): Promise<any> {
        // 1. Bincode-serialize ClientMessage::StructuredQuery { query_json: JSON.stringify(query) }
        // 2. Send over QUIC stream (4-byte length prefix + bincode)
        // 3. Read ServerMessage::StructuredResult
        // 4. Parse data_json as JSON
        // 5. Return parsed result
    }
}
```

### Python (pseudocode)

```python
class HelionDBClient:
    async def find_many(self, table: str, **opts) -> list[dict]:
        query = {"op": "findMany", "from": table, **opts}
        result = await self._send_structured(query)
        return result["data"]

    async def _send_structured(self, query: dict) -> dict:
        json_str = json.dumps(query)
        msg = ClientMessage.StructuredQuery(query_json=json_str, token=self.token)
        # Serialize with bincode, send over QUIC...
```

## Comparison with SQL

| SQL | Structured Query |
|-----|-----------------|
| `SELECT * FROM users WHERE age > 18` | `{ "findMany": { "from": "users", "where": { "age": { "gt": 18 } } } }` |
| `SELECT * FROM users JOIN orders ON users.id = orders.user_id` | `{ "findMany": { "from": "users", "include": [{ "relation": "orders" }] } }` |
| `INSERT INTO users (name, age) VALUES ('Alice', 30)` | `{ "create": { "from": "users", "data": { "name": "Alice", "age": 30 } } }` |
| `UPDATE users SET name = 'Bob' WHERE id = 1` | `{ "update": { "from": "users", "where": { "id": 1 }, "data": { "name": "Bob" } } }` |
| `DELETE FROM users WHERE id = 1` | `{ "delete": { "from": "users", "where": { "id": 1 } } }` |
