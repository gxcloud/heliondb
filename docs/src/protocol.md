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

## Wire Format (Byte-Level)

Each message on the wire has this structure:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
├─────────────────────────────────────────────────────────────────┤
│                         Length (u32)                            │
│                          (big-endian)                            │
├─────────────────────────────────────────────────────────────────┤
│                         CRC32 (u32)                              │
│                     (big-endian, of payload)                     │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│                         Bincode Payload                          │
│                   (variable length, serde-encoded)                │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**Total minimum message overhead:** 8 bytes (4 for length + 4 for CRC32).

### Example Hex Dump — Auth Message

A client sending `Auth { username: "helion", password: "secret" }`:

```text
Offset  Hex                           ASCII
──────  ────────────────────────────  ──────────
000000  00 00 00 21                   ...!       ← length = 33 bytes
000004  1a 2b 3c 4d                   .+<M       ← CRC32
000008  0c 00 00 00 00 00 00 00       ........   ← bincode: variant index 3 (Auth)
000010  06 00 00 00 00 00 00 00       .......    ← bincode: string len 6
000018  68 65 6c 69 6f 6e             helion     ← username = "helion"
00001e  06 00 00 00 00 00 00 00       .......    ← bincode: string len 6
000026  73 65 63 72 65 74             secret     ← password = "secret"
```

> **Note:** Bincode encodes enums as a variant index (u64), strings as length-prefixed (u64 length + UTF-8 bytes).

## Message Size Limits

| Field | Limit | Notes |
|-------|-------|-------|
| SQL text | 1 MB | Longer queries are rejected |
| Username | 255 bytes | |
| Password | 255 bytes | |
| Column count in result | No hard limit | Practically bounded by memory |
| Row count in result | No hard limit | Practically bounded by memory |

## Error Mapping

When the server encounters an error during SQL execution, it maps `HelionError` variants to `ServerMessage` responses:

| `HelionError` Variant | `ServerMessage` |
|----------------------|----------------|
| `Parse` | `QueryResult { error: Some(...) }` |
| `TableNotFound` | `QueryResult { error: Some(...) }` |
| `ColumnNotFound` | `QueryResult { error: Some(...) }` |
| `DuplicateKey` | `QueryResult { error: Some(...) }` |
| `PermissionDenied` | `QueryResult { error: Some(...) }` |
| `Auth` | `AuthResult { success: false, error: Some(...) }` |
| `Protocol` | `Error { message: ... }` |
| `Internal` | `Error { message: ... }` |

## Implementation Notes

- Multiple streams can be opened on a single QUIC connection.
- The session token is scoped to the connection.
- Prepared statements are identified by a hash of the SQL string.
- The token must be included in every `Query`, `Prepare`, and `Execute` message.
- `Execute` (prepared statement execution) is currently a **stub** — it returns the `prepared_id` and `params` back to the client rather than executing a prepared plan. Full prepared statement support is planned.

## Client Implementation Guide

To implement a client:

1. Establish a QUIC connection (TLS 1.3) to the server's UDP endpoint
2. Open a bidirectional stream
3. Send an `Auth` message as the first message on the stream
4. Wait for `AuthResult` — if `success` is true, save the `token`
5. Send `Query` messages with the token on any stream
6. Read `QueryResult` responses — each response completes the stream
7. Close the stream after each query (or reuse for another query)

```rust
// Pseudocode for a QUIC client
let connection = quinn::Endpoint::client(addr)?.connect(server_name, "heliondb")?.await?;
let (mut send, mut recv) = connection.open_bi().await?;

// Auth
let auth = ClientMessage::Auth { username: "helion".into(), password: "pass".into() };
write_message(&mut send, &auth).await?;
let auth_result: ServerMessage = read_message(&mut recv).await?;

// Query
let query = ClientMessage::Query { sql: "SELECT 1".into(), token };
write_message(&mut send, &query).await?;
let result: ServerMessage = read_message(&mut recv).await?;
```
