# Wire Protocol

## Transport

HelionDB uses **QUIC** via `quinn` as its transport protocol.

## TLS

QUIC requires TLS 1.3. The server can auto-generate a self-signed certificate on first run or use user-provided PEM files.

## Connection Lifecycle

```text
Client                          Server
  │                               │
  ├── QUIC Handshake ────────────►│
  │◄── TLS 1.3 established ──────┤
  │                               │
  │  Open bidirectional stream    │
  ├── Auth(username, password)───►│
  │◄── AuthResult(success,token) ─┤
  │                               │
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

Each message is framed as a 4-byte big-endian length prefix followed by a bincode payload.

## Message Types

### Client → Server

#### `Auth`

```rust
Auth {
    username: String,
    password: String,
}
```

#### `Query`

```rust
Query {
    sql: String,
    token: u64,
}
```

#### `Prepare`

```rust
Prepare {
    sql: String,
    token: u64,
}
```

#### `Execute`

```rust
Execute {
    prepared_id: u64,
    params: Vec<String>,
    token: u64,
}
```

### Server → Client

#### `AuthResult`

```rust
AuthResult {
    success: bool,
    token: u64,
    error: Option<String>,
}
```

#### `QueryResult`

```rust
QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    error: Option<String>,
}
```

#### `Prepared`

```rust
Prepared {
    id: u64,
}
```

#### `Error`

```rust
Error {
    message: String,
}
```

## Implementation Notes

- Multiple streams can be opened on a single QUIC connection.
- The session token is scoped to the connection.
- Prepared statements are identified by a hash of the SQL string.
- The token must be included in every `Query`, `Prepare`, and `Execute` message.
