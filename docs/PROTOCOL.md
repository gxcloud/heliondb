# HelionDB Wire Protocol

## Transport

HelionDB uses **QUIC** (via `quinn`) as its transport protocol. Each server accepts connections on a configurable address (default `127.0.0.1:9613`).

## TLS

QUIC requires TLS 1.3. The server can:
- Auto-generate a self-signed certificate on first run (default)
- Use user-provided PEM files via `--cert` and `--key` CLI flags

## Connection Lifecycle

```
Client                          Server
  │                               │
  ├── QUIC Handshake ────────────►│
  │◄── TLS 1.3 established ──────┤
  │                               │
  │  Open bidirectional stream    │
  ├── Auth(username, password)───►│
  │◄── AuthResult(success,token) ─┤
  │                               │
  │  (optional: more streams)     │
  ├── Query(sql, token) ─────────►│
  │◄── QueryResult(columns,rows) ─┤
  │                               │
  ├── Prepare(sql, token) ───────►│
  │◄── Prepared(id) ─────────────┤
  │                               │
  ├── Execute(id, params, token) ►│
  │◄── QueryResult(columns,rows) ─┤
  │                               │
  └── Connection closed ──────────┤
```

## Message Framing

Every message follows this framing:

```
┌──────────────────────────────┐
│  Length (4 bytes, BE u32)    │
├──────────────────────────────┤
│  Bincode payload (variable)  │
└──────────────────────────────┘
```

- Length is the byte count of the bincode payload (NOT including the 4-byte length prefix)
- All integers in the bincode payload are little-endian (bincode default)

## Message Types

All messages are serialized with `bincode`.

### Client → Server

#### `Auth`

Sent as the first message on a stream to authenticate.

```rust
Auth {
    username: String,
    password: String,
}
```

#### `Query`

Execute a SQL query.

```rust
Query {
    sql: String,
    token: u64,       // Session token from AuthResult
}
```

#### `Prepare`

Prepare a SQL statement (returns an ID for repeated execution).

```rust
Prepare {
    sql: String,
    token: u64,
}
```

#### `Execute`

Execute a previously prepared statement.

```rust
Execute {
    prepared_id: u64,
    params: Vec<String>,
    token: u64,
}
```

### Server → Client

#### `AuthResult`

Response to `Auth`. If `success` is true, `token` must be included in all subsequent queries.

```rust
AuthResult {
    success: bool,
    token: u64,
    error: Option<String>,
}
```

#### `QueryResult`

Response to `Query`, `Prepare`, or `Execute`.

```rust
QueryResult {
    columns: Vec<String>,       // Column names
    rows: Vec<Vec<String>>,     // Result rows (all values as strings)
    error: Option<String>,       // Error message if failed
}
```

#### `Prepared`

Response to a successful `Prepare` statement.

```rust
Prepared {
    id: u64,
}
```

#### `Error`

Generic error response.

```rust
Error {
    message: String,
}
```

## Example Session (bytes)

```
=== Stream 1: Auth ===
Send:  [0x00, 0x00, 0x00, 0x1A]  // length = 26 bytes
       ...bincode(Auth { "alice", "secret" })...

Recv:  [0x00, 0x00, 0x00, 0x0E]  // length = 14 bytes
       ...bincode(AuthResult { true, 1, None })...

=== Stream 1: Query ===
Send:  [0x00, 0x00, 0x00, 0x22]  // length = 34 bytes
       ...bincode(Query { "SELECT 1", 1 })...

Recv:  [0x00, 0x00, 0x00, 0x20]  // length = 32 bytes
       ...bincode(QueryResult { ["1"], [["1"]], None })...
```

## Implementation Notes

- Multiple streams can be opened on a single QUIC connection. Each stream is independent.
- The session token is scoped to the connection (different connections get different tokens).
- Prepared statements are identified by a hash of the SQL string (simplified implementation).
- The token must be included in every `Query`/`Prepare`/`Execute` message.
- If the token is invalid, the server responds with `Error { message: "Invalid session token" }`.
