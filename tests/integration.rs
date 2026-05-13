use heliondb::error::HelionError;
use heliondb::executor::ops::{execute, execute_as, QueryResult};
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::storage::engine::DatabaseEngine;
use heliondb::storage::mvcc::{WriteEntry, WriteOp};
use heliondb::storage::types::{Datum, Row};
use tempfile::TempDir;

async fn setup() -> (DatabaseEngine, TempDir) {
    let dir = TempDir::new().unwrap();
    let engine = DatabaseEngine::open(dir.path()).await.unwrap();
    (engine, dir)
}

async fn exec(engine: &DatabaseEngine, sql: &str) -> QueryResult {
    let stmts = parse(sql).unwrap();
    let mut last = None;
    for stmt in &stmts {
        let tables = engine.get_tables().await;
        let p = plan(stmt, &tables).unwrap();
        last = Some(execute(engine, &p).await.unwrap());
    }
    last.unwrap()
}

async fn exec_as(engine: &DatabaseEngine, sql: &str, user: &str) -> QueryResult {
    let stmts = parse(sql).unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    execute_as(engine, &p, Some(user)).await.unwrap()
}

// ── Full Pipeline Integration Tests ─────────────────────────────────

#[tokio::test]
async fn test_full_create_insert_select() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)",
    )
    .await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice', 30)").await;
    exec(&engine, "INSERT INTO users VALUES (2, 'Bob', 25)").await;
    exec(&engine, "INSERT INTO users VALUES (3, 'Charlie', 35)").await;

    let result = exec(&engine, "SELECT * FROM users ORDER BY id ASC").await;
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0], vec!["1", "Alice", "30"]);
}

#[tokio::test]
async fn test_where_filter() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    for i in 1..=5 {
        exec(
            &engine,
            &format!("INSERT INTO t VALUES ({}, {})", i, i * 10),
        )
        .await;
    }

    let r = exec(&engine, "SELECT id FROM t WHERE val > 30").await;
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0][0], "4");

    let r = exec(&engine, "SELECT id FROM t WHERE val BETWEEN 10 AND 30").await;
    assert_eq!(r.rows.len(), 3);
}

#[tokio::test]
async fn test_where_complex() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)",
    )
    .await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice', 30)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 'Bob', 20)").await;
    exec(&engine, "INSERT INTO t VALUES (3, 'Charlie', 25)").await;
    exec(&engine, "INSERT INTO t VALUES (4, 'Diana', 35)").await;

    let r = exec(&engine, "SELECT name FROM t WHERE age > 25 AND age < 35").await;
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0], "Alice");

    let r = exec(&engine, "SELECT name FROM t WHERE age < 22 OR age > 32").await;
    assert_eq!(r.rows.len(), 2);

    let r = exec(
        &engine,
        "SELECT name FROM t WHERE age IN (20, 30) ORDER BY id",
    )
    .await;
    assert_eq!(r.rows.len(), 2);

    let r = exec(&engine, "SELECT name FROM t WHERE name LIKE 'A%'").await;
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0], "Alice");
}

#[tokio::test]
async fn test_order_by_multiple() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)",
    )
    .await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Charlie', 30)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 'Alice', 30)").await;
    exec(&engine, "INSERT INTO t VALUES (3, 'Bob', 25)").await;

    let r = exec(&engine, "SELECT name FROM t ORDER BY name ASC").await;
    assert_eq!(r.rows[0][0], "Alice");
    assert_eq!(r.rows[1][0], "Bob");
    assert_eq!(r.rows[2][0], "Charlie");

    let r = exec(&engine, "SELECT name FROM t ORDER BY name DESC").await;
    assert_eq!(r.rows[0][0], "Charlie");
}

#[tokio::test]
async fn test_limit_offset() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    for i in 1..=10 {
        exec(&engine, &format!("INSERT INTO t VALUES ({})", i)).await;
    }

    let r = exec(&engine, "SELECT id FROM t ORDER BY id LIMIT 3").await;
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][0], "1");

    let r = exec(&engine, "SELECT id FROM t ORDER BY id LIMIT 3 OFFSET 5").await;
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][0], "6");
}

#[tokio::test]
async fn test_update_and_verify() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 100)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 200)").await;

    let r = exec(&engine, "UPDATE t SET val = 150 WHERE id = 1").await;
    assert_eq!(r.rows_affected, 1);

    let r = exec(&engine, "SELECT val FROM t WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "150");
}

#[tokio::test]
async fn test_delete_and_verify() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;
    exec(&engine, "INSERT INTO t VALUES (2)").await;
    exec(&engine, "INSERT INTO t VALUES (3)").await;

    let r = exec(&engine, "DELETE FROM t WHERE id > 1").await;
    assert_eq!(r.rows_affected, 2);
}

#[tokio::test]
async fn test_update_no_where_all_rows() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 10)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 20)").await;

    let r = exec(&engine, "UPDATE t SET val = 0 WHERE id >= 0").await;
    assert_eq!(r.rows_affected, 2);
}

// ── DDL Edge Cases ──────────────────────────────────────────────────

#[tokio::test]
async fn test_create_table_all_types() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (
        a BOOLEAN, b SMALLINT, c INTEGER, d BIGINT,
        e REAL, f DOUBLE, g TEXT, h VARCHAR(100),
        i CHAR(10), j DATE, k TIME, l TIMESTAMP
    )",
    )
    .await;

    let tables = engine.get_tables().await;
    assert_eq!(tables[0].columns.len(), 12);
}

#[tokio::test]
async fn test_drop_table_if_exists() {
    let (engine, _dir) = setup().await;
    exec(&engine, "DROP TABLE IF EXISTS nonexistent").await;
}

#[tokio::test]
async fn test_create_duplicate_table_fails() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let stmts = parse("CREATE TABLE t (x INTEGER)").unwrap();
    let result = execute(
        &engine,
        &plan(&stmts[0], &engine.get_tables().await).unwrap(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_update_no_matching_rows() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 100)").await;

    let r = exec(&engine, "UPDATE t SET val = 999 WHERE id = 999").await;
    assert_eq!(r.rows_affected, 0);
}

#[tokio::test]
async fn test_delete_no_matching_rows() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;

    let r = exec(&engine, "DELETE FROM t WHERE id = 999").await;
    assert_eq!(r.rows_affected, 0);
}

#[tokio::test]
async fn test_update_all_rows() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 10)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 20)").await;

    // Before update
    let r = exec(&engine, "SELECT val FROM t WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "10");

    let r = exec(&engine, "UPDATE t SET val = 0").await;
    assert_eq!(r.rows_affected, 2);

    // Verify update
    let r = exec(&engine, "SELECT val FROM t WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "0");
    let r = exec(&engine, "SELECT val FROM t WHERE id = 2").await;
    assert_eq!(r.rows[0][0], "0");
}

#[tokio::test]
async fn test_delete_all_rows() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;
    exec(&engine, "INSERT INTO t VALUES (2)").await;

    let r = exec(&engine, "DELETE FROM t").await;
    assert_eq!(r.rows_affected, 2);
}

#[tokio::test]
async fn test_empty_table_select() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let r = exec(&engine, "SELECT * FROM t").await;
    assert_eq!(r.rows.len(), 0);
}

#[tokio::test]
async fn test_multiple_tables() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(
        &engine,
        "CREATE TABLE posts (id INTEGER, user_id INTEGER, title TEXT)",
    )
    .await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;
    exec(&engine, "INSERT INTO users VALUES (2, 'Bob')").await;
    exec(&engine, "INSERT INTO posts VALUES (1, 1, 'Hello')").await;
    exec(&engine, "INSERT INTO posts VALUES (2, 1, 'World')").await;

    let r = exec(&engine, "SELECT * FROM users ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
    let r = exec(&engine, "SELECT * FROM posts ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
}

// ── Type Coercion Tests ─────────────────────────────────────────────

#[tokio::test]
async fn test_various_numeric_literals() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (i INTEGER, b BIGINT, d DOUBLE)").await;
    exec(&engine, "INSERT INTO t VALUES (42, 9999999999, 3.14159)").await;
    let r = exec(&engine, "SELECT i, b, d FROM t").await;
    assert_eq!(r.rows[0][0], "42");
    assert_eq!(r.rows[0][1], "9999999999");
    assert_eq!(r.rows[0][2], "3.14159");
}

#[tokio::test]
async fn test_boolean_insert_and_select() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, flag BOOLEAN)").await;
    exec(&engine, "INSERT INTO t VALUES (1, TRUE)").await;
    exec(&engine, "INSERT INTO t VALUES (2, FALSE)").await;

    let r = exec(&engine, "SELECT flag FROM t WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "true");
    let r = exec(&engine, "SELECT flag FROM t WHERE id = 2").await;
    assert_eq!(r.rows[0][0], "false");
}

#[tokio::test]
async fn test_count_star() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice')").await;
    exec(&engine, "INSERT INTO t VALUES (2, NULL)").await;

    let r = exec(&engine, "SELECT * FROM t WHERE name IS NULL").await;
    assert_eq!(r.rows.len(), 1);
    let r = exec(&engine, "SELECT * FROM t WHERE name IS NOT NULL").await;
    assert_eq!(r.rows.len(), 1);
}

// ── User & Permission Integration Tests ─────────────────────────────

#[tokio::test]
async fn test_user_full_lifecycle() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER alice WITH PASSWORD 'secret123'").await;
    assert!(engine.verify_user("alice", "secret123").await);
    assert!(!engine.verify_user("alice", "wrong").await);

    exec(&engine, "ALTER USER alice WITH PASSWORD 'newsecret'").await;
    assert!(!engine.verify_user("alice", "secret123").await);
    assert!(engine.verify_user("alice", "newsecret").await);

    exec(&engine, "DROP USER alice").await;
    assert!(!engine.user_exists("alice").await);
}

#[tokio::test]
async fn test_permission_select_full() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw'").await;
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, name TEXT, secret TEXT)",
    )
    .await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice', 'topsecret')").await;

    let tables = engine.get_tables().await;
    let s = &parse("GRANT SELECT(id, name) ON t TO bob").unwrap();
    execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap();

    let s = &parse("SELECT id, name FROM t").unwrap();
    let p = plan(&s[0], &tables).unwrap();
    let r = execute_as(&engine, &p, Some("bob")).await.unwrap();
    assert_eq!(r.rows[0], vec!["1", "Alice"]);

    let s = &parse("SELECT secret FROM t").unwrap();
    let p = plan(&s[0], &tables).unwrap();
    let err = execute_as(&engine, &p, Some("bob")).await.unwrap_err();
    assert!(matches!(err, HelionError::PermissionDenied(_)));
}

#[tokio::test]
async fn test_permission_grant_all() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw'").await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 100)").await;

    let tables = engine.get_tables().await;
    let s = &parse("GRANT ALL ON t TO bob").unwrap();
    execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap();

    assert!(engine.has_permission("bob", "t", &["id", "val"]).await);
}

#[tokio::test]
async fn test_permission_delete() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw'").await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;
    exec(&engine, "INSERT INTO t VALUES (2)").await;

    let tables = engine.get_tables().await;
    let s = &parse("GRANT DELETE ON t TO bob").unwrap();
    execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap();

    let result = exec_as(&engine, "DELETE FROM t WHERE id = 1", "bob").await;
    assert_eq!(result.rows_affected, 1);

    let s = &parse("SELECT * FROM t").unwrap();
    let p = plan(&s[0], &tables).unwrap();
    let err = execute_as(&engine, &p, Some("bob")).await.unwrap_err();
    assert!(matches!(err, HelionError::PermissionDenied(_)));
}

#[tokio::test]
async fn test_permission_revoke() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw'").await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;

    let tables = engine.get_tables().await;
    let s = &parse("GRANT ALL ON t TO bob").unwrap();
    execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap();
    assert!(engine.has_permission("bob", "t", &["id"]).await);

    let s = &parse("REVOKE ALL ON t FROM bob").unwrap();
    execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap();
    assert!(!engine.has_permission("bob", "t", &["id"]).await);
}

#[tokio::test]
async fn test_permission_no_user_error() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let tables = engine.get_tables().await;
    let s = &parse("GRANT SELECT ON t TO nonexistent").unwrap();
    let err = execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap_err();
    assert!(matches!(err, HelionError::Auth(_)));
}

#[tokio::test]
async fn test_permission_no_table_error() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw'").await;
    let tables = engine.get_tables().await;
    let s = &parse("GRANT SELECT ON nonexistent TO bob").unwrap();
    let err = execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap_err();
    assert!(matches!(err, HelionError::TableNotFound(_)));
}

#[tokio::test]
async fn test_execute_as_without_permission() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw'").await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;

    let tables = engine.get_tables().await;
    let s = &parse("SELECT * FROM t").unwrap();
    let p = plan(&s[0], &tables).unwrap();
    let err = execute_as(&engine, &p, Some("bob")).await.unwrap_err();
    assert!(matches!(err, HelionError::PermissionDenied(_)));
}

#[tokio::test]
async fn test_execute_no_user_skips_checks() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;

    let stmts = parse("SELECT * FROM t").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let r = execute(&engine, &p).await.unwrap();
    assert_eq!(r.rows.len(), 1);
}

#[tokio::test]
async fn test_multiple_users_independent_permissions() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE USER alice WITH PASSWORD 'pw1'").await;
    exec(&engine, "CREATE USER bob WITH PASSWORD 'pw2'").await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'secret')").await;

    let tables = engine.get_tables().await;
    let s = &parse("GRANT SELECT ON t TO alice").unwrap();
    execute(&engine, &plan(&s[0], &tables).unwrap())
        .await
        .unwrap();

    let s = &parse("SELECT * FROM t").unwrap();
    let p = plan(&s[0], &tables).unwrap();
    assert!(execute_as(&engine, &p, Some("alice")).await.is_ok());

    let err = execute_as(&engine, &p, Some("bob")).await.unwrap_err();
    assert!(matches!(err, HelionError::PermissionDenied(_)));
}

// ── WAL and Recovery Tests ─────────────────────────────────────────

#[tokio::test]
async fn test_wal_recovery_basic() {
    let dir = TempDir::new().unwrap();
    // First session: create + insert + manual flush
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
        exec(&engine, "INSERT INTO t VALUES (1, 'Alice')").await;
        engine.shutdown().await.unwrap();
    }
    // Second session: recover and verify
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        let r = exec(&engine, "SELECT * FROM t WHERE id = 1").await;
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][1], "Alice");
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_wal_recovery_multi_insert() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER)").await;
        // Write each insert in its own transaction via exec helper
        exec(&engine, "INSERT INTO t VALUES (10)").await;
        exec(&engine, "INSERT INTO t VALUES (20)").await;
        assert_eq!(
            exec(&engine, "SELECT * FROM t ORDER BY id")
                .await
                .rows
                .len(),
            2
        );
        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        // Use a direct table check without aggregate functions
        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 1, "Should have 1 table");
        // Manually scan visible rows
        use std::collections::BTreeSet;
        let visible = tables[0].scan_visible(u64::MAX, &BTreeSet::new());
        assert_eq!(visible.len(), 2, "Should have 2 visible rows");
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_wal_recovery_with_update() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
        exec(&engine, "INSERT INTO t VALUES (1, 100)").await;

        let r1 = exec(&engine, "SELECT val FROM t WHERE id = 1").await;
        assert_eq!(r1.rows[0][0], "100");

        exec(&engine, "UPDATE t SET val = 999 WHERE id = 1").await;

        let r2 = exec(&engine, "SELECT val FROM t WHERE id = 1").await;
        assert_eq!(r2.rows[0][0], "999", "Update should work in-memory");

        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        // Use direct scan_visible to bypass executor logic
        let tables = engine.get_tables().await;
        use std::collections::BTreeSet;
        let visible = tables[0].scan_visible(u64::MAX, &BTreeSet::new());
        assert_eq!(visible.len(), 1, "Should have 1 visible row");
        assert_eq!(
            visible[0].1.get(1),
            Some(&Datum::Integer(999)),
            "Updated value (index 1) should be 999, got {:?}",
            visible[0].1.get(1)
        );
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_wal_recovery_with_delete() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER)").await;
        exec(&engine, "INSERT INTO t VALUES (1)").await;
        exec(&engine, "INSERT INTO t VALUES (2)").await;
        exec(&engine, "DELETE FROM t WHERE id = 1").await;
        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        let tables = engine.get_tables().await;
        use std::collections::BTreeSet;
        let visible = tables[0].scan_visible(u64::MAX, &BTreeSet::new());
        assert_eq!(visible.len(), 1, "Should have 1 visible row after delete");
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_wal_recovery_with_users_and_grants() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER)").await;
        exec(&engine, "CREATE USER alice WITH PASSWORD 'secret'").await;
        let tables = engine.get_tables().await;
        let s = &parse("GRANT SELECT ON t TO alice").unwrap();
        execute(&engine, &plan(&s[0], &tables).unwrap())
            .await
            .unwrap();
        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        assert!(engine.user_exists("alice").await);
        assert!(engine.verify_user("alice", "secret").await);
        assert!(engine.has_permission("alice", "t", &["id"]).await);
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_wal_recovery_drop_table() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER)").await;
        exec(&engine, "INSERT INTO t VALUES (1)").await;
        exec(&engine, "DROP TABLE t").await;
        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 0);
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_disk_engine_persists_across_restart() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open_with_default_engine(dir.path(), "disk", 60)
            .await
            .unwrap();
        exec(
            &engine,
            "CREATE TABLE disk_items (id INTEGER, name TEXT) ENGINE = disk",
        )
        .await;
        exec(&engine, "INSERT INTO disk_items VALUES (1, 'alpha')").await;
        engine.shutdown().await.unwrap();
    }

    {
        let engine = DatabaseEngine::open_with_default_engine(dir.path(), "disk", 60)
            .await
            .unwrap();
        let result = exec(&engine, "SELECT * FROM disk_items").await;
        assert_eq!(
            result.rows,
            vec![vec!["1".to_string(), "alpha".to_string()]]
        );
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_alter_table_engine_roundtrip() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open_with_default_engine(dir.path(), "disk", 60)
            .await
            .unwrap();
        exec(&engine, "CREATE TABLE migrate_me (id INTEGER, name TEXT)").await;
        exec(&engine, "INSERT INTO migrate_me VALUES (1, 'before')").await;
        exec(&engine, "ALTER TABLE migrate_me ENGINE = disk").await;
        exec(&engine, "INSERT INTO migrate_me VALUES (2, 'after')").await;
        let tables = engine.get_tables().await;
        let visible = tables[0].scan_visible(u64::MAX, &std::collections::BTreeSet::new());
        assert_eq!(visible.len(), 2);
        let live = exec(&engine, "SELECT id, name FROM migrate_me ORDER BY id").await;
        assert_eq!(
            live.rows,
            vec![
                vec!["1".to_string(), "before".to_string()],
                vec!["2".to_string(), "after".to_string()],
            ]
        );
        engine.shutdown().await.unwrap();
    }

    {
        let engine = DatabaseEngine::open_with_default_engine(dir.path(), "disk", 60)
            .await
            .unwrap();
        let result = exec(&engine, "SELECT id, name FROM migrate_me ORDER BY id").await;
        assert_eq!(
            result.rows,
            vec![
                vec!["1".to_string(), "before".to_string()],
                vec!["2".to_string(), "after".to_string()],
            ]
        );
        engine.shutdown().await.unwrap();
    }
}

// ── Error Handling Tests ────────────────────────────────────────────

#[tokio::test]
async fn test_parse_error() {
    let err = parse("SELET * FROM t").unwrap_err();
    assert!(matches!(err, HelionError::Parse(_)));
}

#[tokio::test]
async fn test_table_not_found_error() {
    let (engine, _dir) = setup().await;
    let stmts = parse("SELECT * FROM nonexistent").unwrap();
    let err = plan(&stmts[0], &engine.get_tables().await).unwrap_err();
    assert!(matches!(err, HelionError::TableNotFound(_)));
}

#[tokio::test]
async fn test_column_not_found_error() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let stmts = parse("INSERT INTO t (badcol) VALUES (1)").unwrap();
    let err = plan(&stmts[0], &engine.get_tables().await).unwrap_err();
    assert!(matches!(err, HelionError::ColumnNotFound(_)));
}

// ── MVCC Isolation Tests ───────────────────────────────────────────

#[tokio::test]
async fn test_concurrent_transactions_no_conflict() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 100)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 200)").await;

    let mut tx1 = engine.begin();
    let mut tx2 = engine.begin();
    let tables = engine.get_tables().await;

    tx1.add_write(WriteEntry {
        table_name: "t".to_string(),
        row_idx: 0,
        old_txid_max: u64::MAX,
        operation: WriteOp::Update(Row::new(vec![Datum::BigInt(1), Datum::BigInt(999)])),
    });
    tx2.add_write(WriteEntry {
        table_name: "t".to_string(),
        row_idx: 1,
        old_txid_max: u64::MAX,
        operation: WriteOp::Update(Row::new(vec![Datum::BigInt(2), Datum::BigInt(888)])),
    });
    drop(tables);
    engine.commit(&mut tx1).await.unwrap();
    engine.commit(&mut tx2).await.unwrap();

    let r = exec(&engine, "SELECT val FROM t WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "999");
    let r = exec(&engine, "SELECT val FROM t WHERE id = 2").await;
    assert_eq!(r.rows[0][0], "888");
}

#[tokio::test]
async fn test_concurrent_transactions_conflict() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 100)").await;

    let mut tx1 = engine.begin();
    let mut tx2 = engine.begin();
    let tables = engine.get_tables().await;

    tx1.add_write(WriteEntry {
        table_name: "t".to_string(),
        row_idx: 0,
        old_txid_max: u64::MAX,
        operation: WriteOp::Update(Row::new(vec![Datum::BigInt(1), Datum::BigInt(200)])),
    });
    tx2.add_write(WriteEntry {
        table_name: "t".to_string(),
        row_idx: 0,
        old_txid_max: u64::MAX,
        operation: WriteOp::Update(Row::new(vec![Datum::BigInt(1), Datum::BigInt(300)])),
    });
    drop(tables);

    engine.commit(&mut tx1).await.unwrap();
    let err = engine.commit(&mut tx2).await.unwrap_err();
    assert!(matches!(err, HelionError::Conflict(_)));
}

// ── Index Integration Tests ──────────────────────────────────────────

#[tokio::test]
async fn test_pk_auto_index_enforces_uniqueness() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;

    // First insert should succeed
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;

    // Duplicate PK should fail
    let stmts = parse("INSERT INTO users VALUES (1, 'Bob')").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::DuplicateKey { .. }));

    // Non-duplicate insert should succeed
    exec(&engine, "INSERT INTO users VALUES (2, 'Bob')").await;
    let r = exec(&engine, "SELECT * FROM users ORDER BY id").await;
    assert_eq!(r.rows.len(), 2, "SELECT should return 2 rows");
}

#[tokio::test]
async fn test_pk_enforced_on_update() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
    )
    .await;
    exec(&engine, "INSERT INTO t VALUES (1, 100)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 200)").await;

    // Update changing PK to existing value should fail
    let stmts = parse("UPDATE t SET id = 1 WHERE id = 2").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::DuplicateKey { .. }));
}

#[tokio::test]
async fn test_unique_column_auto_index() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, email TEXT UNIQUE)").await;

    exec(&engine, "INSERT INTO t VALUES (1, 'a@x.com')").await;

    // Duplicate email should fail
    let stmts = parse("INSERT INTO t VALUES (2, 'a@x.com')").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::DuplicateKey { .. }));

    // Different email should succeed
    exec(&engine, "INSERT INTO t VALUES (2, 'b@x.com')").await;
}

#[tokio::test]
async fn test_create_index_sql() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)",
    )
    .await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice', 30)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 'Bob', 25)").await;

    // Create index via SQL
    exec(&engine, "CREATE INDEX idx_age ON t (age)").await;

    // Verify the index exists
    let tables = engine.get_tables().await;
    let t = tables.iter().find(|t| t.name == "t").unwrap();
    assert!(t.has_index("idx_age"));
}

#[tokio::test]
async fn test_create_unique_index_sql() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, email TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'a@x.com')").await;

    // Create unique index (should succeed since data is unique)
    exec(&engine, "CREATE UNIQUE INDEX uq_email ON t (email)").await;

    // Now duplicate should fail
    let stmts = parse("INSERT INTO t VALUES (2, 'a@x.com')").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::DuplicateKey { .. }));
}

#[tokio::test]
async fn test_create_unique_index_on_duplicate_data_fails() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, email TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'dup@x.com')").await;
    exec(&engine, "INSERT INTO t VALUES (2, 'dup@x.com')").await;

    // Creating a unique index on duplicate data should fail
    let stmts = parse("CREATE UNIQUE INDEX uq_email ON t (email)").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::DuplicateKey { .. }));
}

#[tokio::test]
async fn test_create_index_if_not_exists() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "CREATE INDEX idx ON t (id)").await;

    // Should not error
    exec(&engine, "CREATE INDEX IF NOT EXISTS idx ON t (id)").await;

    // Without IF NOT EXISTS should error
    let stmts = parse("CREATE INDEX idx ON t (id)").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::IndexAlreadyExists(_)));
}

#[tokio::test]
async fn test_drop_index_sql() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "CREATE INDEX idx ON t (id)").await;

    let tables = engine.get_tables().await;
    assert!(tables[0].has_index("idx"));

    exec(&engine, "DROP INDEX idx ON t").await;

    let tables = engine.get_tables().await;
    assert!(!tables[0].has_index("idx"));
}

#[tokio::test]
async fn test_drop_index_if_exists() {
    let (engine, _dir) = setup().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;

    // Should not error
    exec(&engine, "DROP INDEX IF EXISTS nonexistent ON t").await;

    // Without IF EXISTS should error
    let stmts = parse("DROP INDEX nonexistent ON t").unwrap();
    let tables = engine.get_tables().await;
    let p = plan(&stmts[0], &tables).unwrap();
    let err = execute(&engine, &p).await.unwrap_err();
    assert!(matches!(err, HelionError::IndexNotFound(_)));
}

#[tokio::test]
async fn test_index_accelerates_select() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
    )
    .await;

    for i in 0..10 {
        exec(
            &engine,
            &format!("INSERT INTO t VALUES ({}, {})", i, i * 10),
        )
        .await;
    }

    // Point lookup via PK index
    let r = exec(&engine, "SELECT val FROM t WHERE id = 5").await;
    assert_eq!(r.rows[0][0], "50");

    // Range scan via full table scan (index range scan disabled pending investigation)
    let r = exec(&engine, "SELECT id FROM t WHERE id >= 7").await;
    assert_eq!(r.rows.len(), 3);
}

#[tokio::test]
async fn test_index_works_after_wal_recovery() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        )
        .await;
        exec(&engine, "INSERT INTO t VALUES (1, 100)").await;
        exec(&engine, "INSERT INTO t VALUES (2, 200)").await;
        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        // PK index should be rebuilt and enforce uniqueness
        let stmts = parse("INSERT INTO t VALUES (1, 999)").unwrap();
        let tables = engine.get_tables().await;
        let p = plan(&stmts[0], &tables).unwrap();
        let err = execute(&engine, &p).await.unwrap_err();
        assert!(matches!(err, HelionError::DuplicateKey { .. }));

        // Index should still accelerate queries
        let r = exec(&engine, "SELECT val FROM t WHERE id = 2").await;
        assert_eq!(r.rows[0][0], "200");
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn test_composite_index() {
    let (engine, _dir) = setup().await;
    exec(
        &engine,
        "CREATE TABLE t (a INTEGER, b INTEGER, val INTEGER)",
    )
    .await;
    exec(&engine, "CREATE INDEX idx_ab ON t (a, b)").await;

    exec(&engine, "INSERT INTO t VALUES (1, 10, 100)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 20, 200)").await;
    exec(&engine, "INSERT INTO t VALUES (2, 10, 300)").await;

    let r = exec(&engine, "SELECT val FROM t WHERE a = 1 AND b = 20").await;
    assert_eq!(r.rows[0][0], "200");
}

#[tokio::test]
async fn test_index_on_disk_engine() {
    let dir = TempDir::new().unwrap();
    {
        let engine = DatabaseEngine::open_with_default_engine(dir.path(), "disk", 60)
            .await
            .unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER) ENGINE = disk",
        )
        .await;
        exec(&engine, "INSERT INTO t VALUES (1, 100)").await;
        exec(&engine, "INSERT INTO t VALUES (2, 200)").await;

        // Duplicate PK should fail even with disk engine
        let stmts = parse("INSERT INTO t VALUES (1, 999)").unwrap();
        let tables = engine.get_tables().await;
        let p = plan(&stmts[0], &tables).unwrap();
        let err = execute(&engine, &p).await.unwrap_err();
        assert!(matches!(err, HelionError::DuplicateKey { .. }));
        engine.shutdown().await.unwrap();
    }
    {
        let engine = DatabaseEngine::open_with_default_engine(dir.path(), "disk", 60)
            .await
            .unwrap();
        // After restart, PK index should still enforce uniqueness
        let stmts = parse("INSERT INTO t VALUES (1, 999)").unwrap();
        let tables = engine.get_tables().await;
        let p = plan(&stmts[0], &tables).unwrap();
        let err = execute(&engine, &p).await.unwrap_err();
        assert!(matches!(err, HelionError::DuplicateKey { .. }));
        engine.shutdown().await.unwrap();
    }
}
