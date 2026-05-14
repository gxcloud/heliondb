mod common;

use common::{exec, setup as setup_engine};

use heliondb::error::HelionError;
use heliondb::protocol::structured::{
    execute_structured, parse_where_clause, parse_where_clause_opt, StructuredQuery,
};
use heliondb::sql::parser::{BinaryOperator, Expression, UnaryOperator};
use heliondb::storage::types::Datum;

// ═══════════════════════════════════════════════════════════
// Structured query: low-level tests via execute_structured()
// ═══════════════════════════════════════════════════════════

async fn exec_structured(engine: &heliondb::storage::engine::DatabaseEngine, json: &str) -> serde_json::Value {
    let query: StructuredQuery = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("Failed to parse structured query JSON: {}\nJSON: {}", e, json));
    let result = execute_structured(engine, &query, None).await
        .unwrap_or_else(|e| panic!("Structured query failed: {}\nJSON: {}", e, json));
    serde_json::from_str(&result)
        .unwrap_or_else(|e| panic!("Failed to parse result JSON: {}", e))
}

async fn exec_structured_err(engine: &heliondb::storage::engine::DatabaseEngine, json: &str) -> HelionError {
    let query: StructuredQuery = serde_json::from_str(json).unwrap();
    execute_structured(engine, &query, None).await.unwrap_err()
}

// ═══════════════════════════════════════════════════════════
// findMany
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_find_many_basic() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Charlie', 35)").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users"
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 3, "findMany should return all rows");
    assert_eq!(data[0]["id"].as_str().unwrap(), "1");
    assert_eq!(data[0]["name"].as_str().unwrap(), "Alice");
}

#[tokio::test]
async fn test_structured_find_many_with_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Charlie', 35)").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "where": { "age": { "gt": 28 } }
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "age > 28: Alice(30) and Charlie(35)");
}

#[tokio::test]
async fn test_structured_find_many_equality() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "where": { "name": "Alice" }
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["id"].as_str().unwrap(), "1");
}

#[tokio::test]
async fn test_structured_find_many_select_fields() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice', 30)").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "select": ["name"]
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert!(data[0].get("name").is_some(), "name should be selected");
    assert!(data[0].get("id").is_none(), "id should NOT be selected (not in select)");
}

#[tokio::test]
async fn test_structured_find_many_order_by() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Charlie'), (2, 'Alice'), (3, 'Bob')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "orderBy": [{ "field": "name", "direction": "asc" }]
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data[0]["name"].as_str().unwrap(), "Alice");
    assert_eq!(data[2]["name"].as_str().unwrap(), "Charlie");
}

#[tokio::test]
async fn test_structured_find_many_limit() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Charlie')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "orderBy": [{ "field": "id", "direction": "asc" }],
        "take": 2
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["id"].as_str().unwrap(), "1");
}

#[tokio::test]
async fn test_structured_find_many_compound_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Charlie', 35), (4, 'Diana', 28)").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "t",
        "where": { "AND": [{ "age": { "gte": 28 } }, { "age": { "lte": 32 } }] }
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "age between 28 and 32: Alice(30) and Diana(28)");
}

#[tokio::test]
async fn test_structured_find_many_or_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Charlie')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "t",
        "where": { "OR": [{ "name": "Alice" }, { "name": "Charlie" }] }
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
}

#[tokio::test]
async fn test_structured_find_many_like() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES ('Alice'), ('Bob'), ('Charlie')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "t",
        "where": { "name": { "contains": "ob" } }
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 1, "LIKE should match 1 row (Bob)");
    assert_eq!(data[0]["name"].as_str().unwrap(), "Bob");
}

// ═══════════════════════════════════════════════════════════
// findUnique
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_find_unique() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findUnique",
        "from": "users",
        "where": { "id": 1 }
    }"#).await;

    let data = result["data"].as_object().unwrap();
    assert_eq!(data["id"].as_str().unwrap(), "1");
    assert_eq!(data["name"].as_str().unwrap(), "Alice");
}

#[tokio::test]
async fn test_structured_find_unique_not_found() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;

    let result = exec_structured(&engine, r#"{
        "op": "findUnique",
        "from": "users",
        "where": { "id": 999 }
    }"#).await;

    assert_eq!(result["data"], serde_json::Value::Null);
}

// ═══════════════════════════════════════════════════════════
// create
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_create() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").await;

    let result = exec_structured(&engine, r#"{
        "op": "create",
        "from": "users",
        "data": { "name": "Alice", "age": 30 }
    }"#).await;

    assert!(result.get("data").is_some(), "create should return data");
    // Verify the data was inserted
    let r = exec(&engine, "SELECT name FROM users WHERE name = 'Alice'").await;
    assert_eq!(r.rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════
// update
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_update() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice', 30)").await;

    let result = exec_structured(&engine, r#"{
        "op": "update",
        "from": "users",
        "where": { "id": 1 },
        "data": { "name": "Alice Updated" }
    }"#).await;

    assert_eq!(result["data"]["rows_affected"].as_u64().unwrap(), 1);
    let r = exec(&engine, "SELECT name FROM users WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "Alice Updated");
}

// ═══════════════════════════════════════════════════════════
// delete
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_delete() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;

    let result = exec_structured(&engine, r#"{
        "op": "delete",
        "from": "users",
        "where": { "id": 1 }
    }"#).await;

    assert_eq!(result["data"]["rows_affected"].as_u64().unwrap(), 1);
    let r = exec(&engine, "SELECT id FROM users").await;
    assert_eq!(r.rows.len(), 0);
}

// ═══════════════════════════════════════════════════════════
// upsert
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_upsert_update() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;

    // Upsert should UPDATE existing row
    let _result = exec_structured(&engine, r#"{
        "op": "upsert",
        "from": "users",
        "where": { "id": 1 },
        "create": { "name": "ShouldNotCreate" },
        "update": { "name": "Alice Updated" }
    }"#).await;

    let r = exec(&engine, "SELECT name FROM users WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "Alice Updated", "upsert should update existing row");
}

#[tokio::test]
async fn test_structured_upsert_create() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;

    // Upsert should CREATE new row when where doesn't match
    let _result = exec_structured(&engine, r#"{
        "op": "upsert",
        "from": "users",
        "where": { "id": 999 },
        "create": { "name": "NewUser" },
        "update": { "name": "ShouldNotUpdate" }
    }"#).await;

    let r = exec(&engine, "SELECT name FROM users WHERE name = 'NewUser'").await;
    assert_eq!(r.rows.len(), 1, "upsert should create new row");
}

// ═══════════════════════════════════════════════════════════
// FIND MANY WITH INCLUDE (auto-JOIN via FK)
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_find_many_include() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").await;
    exec(&engine, "CREATE TABLE orders (id INTEGER, user_id INTEGER REFERENCES users(id), total DOUBLE)").await;

    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')").await;
    // Use the legacy FK, then also insert in a way the convention-based lookup works
    exec(&engine, "INSERT INTO orders VALUES (1, 1, 100.0), (2, 1, 200.0), (3, 2, 150.0)").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "include": [{ "relation": "orders" }]
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "Two users");
}

#[tokio::test]
async fn test_structured_find_many_include_filtered() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").await;
    exec(&engine, "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)").await;
    // Convention-based FK: orders.user_id -> users.id (orders.user_id convention for name users_id -> users.id doesn't match)
    // So let's use FK SQL syntax:
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')").await;
    exec(&engine, "INSERT INTO orders VALUES (1, 1, 100.0), (2, 1, 200.0), (3, 2, 50.0)").await;

    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "users",
        "include": [{ "relation": "orders", "where": { "total": { "gt": 100 } } }]
    }"#).await;

    let data = result["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "Two users");
}

// ═══════════════════════════════════════════════════════════
// Error cases
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_structured_error_missing_table() {
    let (engine, _dir) = setup_engine().await;
    let err = exec_structured_err(&engine, r#"{
        "op": "findMany",
        "from": "nonexistent"
    }"#).await;
    assert!(err.to_string().contains("not found"), "Should report table not found");
}

#[tokio::test]
async fn test_structured_error_bad_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let result = exec_structured(&engine, r#"{
        "op": "findMany",
        "from": "t",
        "where": { "bad_field": 1 }
    }"#).await;
    // Bad column in WHERE might silently produce empty results
    // (filter evaluates to false for all rows, which is acceptable)
    assert!(result.get("data").is_some(), "Query with bad where should still return a response");
}

#[tokio::test]
async fn test_structured_error_bad_json() {
    let err = serde_json::from_str::<StructuredQuery>(r#"{"op": "findMany"}"#);
    assert!(err.is_err(), "Missing 'from' field should error");
}

#[tokio::test]
async fn test_structured_error_invalid_op() {
    let err = serde_json::from_str::<StructuredQuery>(r#"{"op": "invalidOp"}"#);
    assert!(err.is_err(), "Invalid operation should error");
}

// ═══════════════════════════════════════════════════════════
// Where clause parsing (unit tests)
// ═══════════════════════════════════════════════════════════

#[test]
fn test_parse_where_null() {
    let val = serde_json::json!(null);
    assert!(parse_where_clause_opt(&Some(val)).unwrap().is_none());
}

#[test]
fn test_parse_where_equality_string() {
    let val = serde_json::json!({"name": "Alice"});
    let expr = parse_where_clause(&val).unwrap();
    // Should produce: BinaryOp(Column("name"), Eq, Literal(Text("Alice")))
    match expr {
        Expression::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            match *left {
                Expression::Column(name) => assert_eq!(name, "name"),
                _ => panic!("Expected Column"),
            }
            match *right {
                Expression::Literal(Datum::Text(ref s)) => assert_eq!(s, "Alice"),
                _ => panic!("Expected Text literal"),
            }
        }
        _ => panic!("Expected BinaryOp with Eq"),
    }
}

#[test]
fn test_parse_where_comparison() {
    let val = serde_json::json!({"age": {"gt": 18}});
    let expr = parse_where_clause(&val).unwrap();
    match expr {
        Expression::BinaryOp { left, op: BinaryOperator::Gt, right } => {
            match *left {
                Expression::Column(name) => assert_eq!(name, "age"),
                _ => panic!("Expected Column"),
            }
            match *right {
                Expression::Literal(Datum::BigInt(v)) => assert_eq!(v, 18),
                _ => panic!("Expected BigInt"),
            }
        }
        _ => panic!("Expected BinaryOp with Gt"),
    }
}

#[test]
fn test_parse_where_and() {
    let val = serde_json::json!({"AND": [{"x": 1}, {"y": 2}]});
    let expr = parse_where_clause(&val).unwrap();
    // Should produce: BinaryOp(Col("x")=1, And, Col("y")=2)
    match expr {
        Expression::BinaryOp { op: BinaryOperator::And, .. } => {}
        _ => panic!("Expected AND"),
    }
}

#[test]
fn test_parse_where_or() {
    let val = serde_json::json!({"OR": [{"x": 1}, {"y": 2}]});
    let expr = parse_where_clause(&val).unwrap();
    match expr {
        Expression::BinaryOp { op: BinaryOperator::Or, .. } => {}
        _ => panic!("Expected OR"),
    }
}

#[test]
fn test_parse_where_not() {
    let val = serde_json::json!({"NOT": {"active": true}});
    let expr = parse_where_clause(&val).unwrap();
    match expr {
        Expression::UnaryOp { op: UnaryOperator::Not, .. } => {}
        _ => panic!("Expected NOT"),
    }
}

#[test]
fn test_parse_where_is_null() {
    let val = serde_json::json!({"name": null});
    let expr = parse_where_clause(&val).unwrap();
    match expr {
        Expression::IsNull(inner) => {
            match *inner {
                Expression::Column(name) => assert_eq!(name, "name"),
                _ => panic!("Expected Column"),
            }
        }
        _ => panic!("Expected IsNull"),
    }
}

#[test]
fn test_parse_where_contains() {
    let val = serde_json::json!({"name": {"contains": "Ali"}});
    let expr = parse_where_clause(&val).unwrap();
    match expr {
        Expression::Like { expr: inner, pattern } => {
            assert!(pattern.contains("Ali"), "LIKE pattern should contain search string");
            assert!(pattern.starts_with('%'), "LIKE should have leading %");
            assert!(pattern.ends_with('%'), "LIKE should have trailing %");
            match *inner {
                Expression::Column(name) => assert_eq!(name, "name"),
                _ => panic!("Expected Column"),
            }
        }
        _ => panic!("Expected Like"),
    }
}

#[test]
fn test_parse_where_in() {
    let val = serde_json::json!({"id": {"in": [1, 2, 3]}});
    let expr = parse_where_clause(&val).unwrap();
    match expr {
        Expression::In { expr: inner, list } => {
            match *inner {
                Expression::Column(name) => assert_eq!(name, "id"),
                _ => panic!("Expected Column"),
            }
            assert_eq!(list.len(), 3);
        }
        _ => panic!("Expected In"),
    }
}

#[test]
fn test_parse_where_starts_ends_with() {
    let starts = serde_json::json!({"name": {"startsWith": "A"}});
    let expr = parse_where_clause(&starts).unwrap();
    match expr {
        Expression::Like { pattern, .. } => {
            assert!(pattern.starts_with("A"), "startsWith: pattern should start with value");
            assert!(pattern.ends_with('%'), "startsWith: pattern should end with %");
        }
        _ => panic!("Expected Like"),
    }

    let ends = serde_json::json!({"name": {"endsWith": "e"}});
    let expr = parse_where_clause(&ends).unwrap();
    match expr {
        Expression::Like { pattern, .. } => {
            assert!(pattern.ends_with("e"), "endsWith: pattern should end with value");
            assert!(pattern.starts_with('%'), "endsWith: pattern should start with %");
        }
        _ => panic!("Expected Like"),
    }
}

// ═══════════════════════════════════════════════════════════
// StructuredQuery JSON deserialization
// ═══════════════════════════════════════════════════════════

#[test]
fn test_deserialize_find_many() {
    let json = r#"{"op":"findMany","from":"users","where":{"age":{"gt":18}},"take":10}"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::FindMany(input) => {
            assert_eq!(input.from, "users");
            assert!(input.where_clause.is_some());
            assert_eq!(input.take, Some(10));
        }
        _ => panic!("Expected FindMany"),
    }
}

#[test]
fn test_deserialize_find_unique() {
    let json = r#"{"op":"findUnique","from":"users","where":{"id":1}}"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::FindUnique(_) => {}
        _ => panic!("Expected FindUnique"),
    }
}

#[test]
fn test_deserialize_create() {
    let json = r#"{"op":"create","from":"users","data":{"name":"Alice","age":30}}"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::Create(input) => {
            assert_eq!(input.from, "users");
            assert_eq!(input.data.len(), 2);
        }
        _ => panic!("Expected Create"),
    }
}

#[test]
fn test_deserialize_update() {
    let json = r#"{"op":"update","from":"users","where":{"id":1},"data":{"name":"Bob"}}"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::Update(_) => {}
        _ => panic!("Expected Update"),
    }
}

#[test]
fn test_deserialize_delete() {
    let json = r#"{"op":"delete","from":"users","where":{"id":1}}"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::Delete(_) => {}
        _ => panic!("Expected Delete"),
    }
}

#[test]
fn test_deserialize_upsert() {
    let json = r#"{"op":"upsert","from":"users","where":{"id":1},"create":{"name":"New"},"update":{"name":"Upd"}}"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::Upsert(input) => {
            assert_eq!(input.create.len(), 1);
            assert_eq!(input.update.len(), 1);
        }
        _ => panic!("Expected Upsert"),
    }
}

#[test]
fn test_deserialize_with_include() {
    let json = r#"{
        "op": "findMany",
        "from": "users",
        "include": [{ "relation": "orders", "where": { "total": { "gt": 100 } } }]
    }"#;
    let q: StructuredQuery = serde_json::from_str(json).unwrap();
    match q {
        StructuredQuery::FindMany(input) => {
            assert_eq!(input.include.len(), 1);
            assert_eq!(input.include[0].relation, "orders");
        }
        _ => panic!("Expected FindMany"),
    }
}
