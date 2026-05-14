mod common;

use common::exec;
use common::setup as setup_engine;

// ═══════════════════════════════════════════════════════
// JOIN correctness
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_join_inner_equi() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;
    exec(&engine, "INSERT INTO users VALUES (2, 'Bob')").await;
    exec(&engine, "INSERT INTO orders VALUES (1, 1, 100.0)").await;
    exec(&engine, "INSERT INTO orders VALUES (2, 1, 200.0)").await;
    exec(&engine, "INSERT INTO orders VALUES (3, 2, 150.0)").await;
    let r = exec(&engine, "SELECT users.name, orders.total FROM users JOIN orders ON users.id = orders.user_id").await;
    assert_eq!(r.rows.len(), 3);
}

#[tokio::test]
async fn test_join_left_outer() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')").await;
    exec(&engine, "INSERT INTO orders VALUES (1, 1, 100.0)").await;
    let r = exec(&engine, "SELECT users.name, orders.total FROM users LEFT JOIN orders ON users.id = orders.user_id ORDER BY users.name").await;
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[1][1], "NULL");
}

#[tokio::test]
async fn test_join_left_all_null() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t1 VALUES (1), (2), (3)").await;
    let r = exec(&engine, "SELECT t1.id, t2.val FROM t1 LEFT JOIN t2 ON t1.id = t2.id ORDER BY t1.id").await;
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0], vec!["1", "NULL"]);
}

#[tokio::test]
async fn test_join_right_outer() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice')").await;
    exec(&engine, "INSERT INTO orders VALUES (1, 1, 100.0), (2, 2, 200.0)").await;
    let r = exec(&engine, "SELECT users.name, orders.total FROM users RIGHT JOIN orders ON users.id = orders.user_id ORDER BY orders.id").await;
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[1][0], "NULL");
}

#[tokio::test]
async fn test_join_cross() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE a (x INTEGER)").await;
    exec(&engine, "CREATE TABLE b (y INTEGER)").await;
    exec(&engine, "INSERT INTO a VALUES (1), (2)").await;
    exec(&engine, "INSERT INTO b VALUES (10), (20)").await;
    let r = exec(&engine, "SELECT * FROM a CROSS JOIN b ORDER BY a.x, b.y").await;
    assert_eq!(r.rows.len(), 4);
    assert_eq!(r.rows[0], vec!["1", "10"]);
}

#[tokio::test]
async fn test_join_three_way() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE a (id INTEGER)").await;
    exec(&engine, "CREATE TABLE b (id INTEGER, a_id INTEGER)").await;
    exec(&engine, "CREATE TABLE c (id INTEGER, b_id INTEGER)").await;
    exec(&engine, "INSERT INTO a VALUES (1)").await;
    exec(&engine, "INSERT INTO b VALUES (10, 1)").await;
    exec(&engine, "INSERT INTO c VALUES (100, 10)").await;
    let r = exec(&engine, "SELECT * FROM a JOIN b ON a.id = b.a_id JOIN c ON b.id = c.b_id").await;
    assert_eq!(r.rows.len(), 1);
}

#[tokio::test]
async fn test_join_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE users (id INTEGER, name TEXT)").await;
    exec(&engine, "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)").await;
    exec(&engine, "INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob')").await;
    exec(&engine, "INSERT INTO orders VALUES (1, 1, 100.0), (2, 1, 200.0), (3, 2, 50.0)").await;
    let r = exec(&engine, "SELECT users.name, orders.total FROM users JOIN orders ON users.id = orders.user_id WHERE orders.total > 75 ORDER BY orders.total DESC").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_join_limit() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (id INTEGER, t1_id INTEGER)").await;
    exec(&engine, "INSERT INTO t1 VALUES (1), (2)").await;
    exec(&engine, "INSERT INTO t2 VALUES (10, 1), (20, 1), (30, 2)").await;
    let r = exec(&engine, "SELECT t1.id, t2.id FROM t1 JOIN t2 ON t1.id = t2.t1_id LIMIT 2").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_join_null_keys() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (ref INTEGER)").await;
    exec(&engine, "INSERT INTO t1 VALUES (1), (2)").await;
    exec(&engine, "INSERT INTO t2 VALUES (1), (NULL)").await;
    let r = exec(&engine, "SELECT t1.id FROM t1 JOIN t2 ON t1.id = t2.ref").await;
    assert_eq!(r.rows.len(), 1);
}

#[tokio::test]
async fn test_join_empty() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (id INTEGER)").await;
    let r = exec(&engine, "SELECT * FROM t1 JOIN t2 ON t1.id = t2.id").await;
    assert_eq!(r.rows.len(), 0);
}

#[tokio::test]
async fn test_join_ambiguous_column() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER, val INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t1 VALUES (1, 10)").await;
    exec(&engine, "INSERT INTO t2 VALUES (1, 20)").await;
    let stmts = heliondb::sql::parser::parse("SELECT id FROM t1 JOIN t2 ON t1.id = t2.id").unwrap();
    let tables = engine.get_tables().await;
    let result = heliondb::sql::planner::plan(&stmts[0], &tables);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_join_on_between() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (lo INTEGER, hi INTEGER)").await;
    exec(&engine, "INSERT INTO t1 VALUES (5), (15), (25)").await;
    exec(&engine, "INSERT INTO t2 VALUES (1, 10), (20, 30)").await;
    let r = exec(&engine, "SELECT t1.id, t2.lo, t2.hi FROM t1 JOIN t2 ON t1.id BETWEEN t2.lo AND t2.hi ORDER BY t1.id").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_join_inequality() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (val INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (val INTEGER)").await;
    exec(&engine, "INSERT INTO t1 VALUES (10), (20)").await;
    exec(&engine, "INSERT INTO t2 VALUES (5), (15), (25)").await;
    let r = exec(&engine, "SELECT t1.val, t2.val FROM t1 JOIN t2 ON t1.val < t2.val ORDER BY t1.val, t2.val").await;
    assert_eq!(r.rows.len(), 3);
}

#[tokio::test]
async fn test_join_permission_denied() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE pub_t (id INTEGER)").await;
    exec(&engine, "CREATE TABLE sec_t (id INTEGER)").await;
    exec(&engine, "CREATE USER usr WITH PASSWORD 'pw'").await;
    let tables = engine.get_tables().await;
    let s = heliondb::sql::parser::parse("GRANT SELECT ON pub_t TO usr").unwrap();
    let p = heliondb::sql::planner::plan(&s[0], &tables).unwrap();
    heliondb::executor::ops::execute(&engine, &p).await.unwrap();
    let result = common::exec_as(&engine, "SELECT * FROM pub_t JOIN sec_t ON pub_t.id = sec_t.id", "usr").await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════
// Expression evaluation (WHERE clause)
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_where_is_null() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, NULL), (2, 'Alice')").await;
    let r = exec(&engine, "SELECT id FROM t WHERE name IS NULL").await;
    assert_eq!(r.rows[0][0], "1");
    let r = exec(&engine, "SELECT id FROM t WHERE name IS NOT NULL").await;
    assert_eq!(r.rows[0][0], "2");
}

#[tokio::test]
async fn test_where_in_list() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2), (3), (4), (5)").await;
    let r = exec(&engine, "SELECT id FROM t WHERE id IN (2, 4) ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_where_between() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2), (3), (4), (5)").await;
    let r = exec(&engine, "SELECT id FROM t WHERE id BETWEEN 2 AND 4 ORDER BY id").await;
    assert_eq!(r.rows.len(), 3);
}

#[tokio::test]
async fn test_where_like() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES ('Alice'), ('Bob'), ('Charlie'), ('alex')").await;
    let r = exec(&engine, "SELECT name FROM t WHERE name LIKE 'A%' ORDER BY name").await;
    assert_eq!(r.rows.len(), 1);
    let r = exec(&engine, "SELECT name FROM t WHERE name LIKE '%e' ORDER BY name").await;
    assert_eq!(r.rows.len(), 2);
    let r = exec(&engine, "SELECT name FROM t WHERE name LIKE '___' ORDER BY name").await;
    // 'Bob' is 3 chars, 'alex' is 4, 'Alex' not in data
    assert_eq!(r.rows.len(), 1, "Only 'Bob' is 3 letters (case-sensitive)");
}

#[tokio::test]
async fn test_where_boolean() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, active BOOLEAN)").await;
    exec(&engine, "INSERT INTO t VALUES (1, true), (2, false), (3, true)").await;
    let r = exec(&engine, "SELECT id FROM t WHERE active ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_where_compound() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Charlie', 35)").await;
    let r = exec(&engine, "SELECT id FROM t WHERE age > 25 AND name LIKE 'A%'").await;
    assert_eq!(r.rows[0][0], "1");
    let r = exec(&engine, "SELECT id FROM t WHERE age = 25 OR name = 'Charlie' ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_where_comparison() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2), (3), (4), (5)").await;
    let r = exec(&engine, "SELECT id FROM t WHERE id >= 3 ORDER BY id").await;
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][0], "3");
    let r = exec(&engine, "SELECT id FROM t WHERE id < 3 ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
}

// ═══════════════════════════════════════════════════════
// ORDER BY / LIMIT / OFFSET
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_order_asc_desc() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Charlie'), (2, 'Alice'), (3, 'Bob')").await;
    let r = exec(&engine, "SELECT name FROM t ORDER BY name ASC").await;
    assert_eq!(r.rows[0][0], "Alice");
    let r = exec(&engine, "SELECT name FROM t ORDER BY name DESC").await;
    assert_eq!(r.rows[0][0], "Charlie");
}

#[tokio::test]
async fn test_order_multi_column() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, age INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 30, 'Charlie'), (2, 30, 'Alice'), (3, 25, 'Bob')").await;
    let r = exec(&engine, "SELECT name FROM t ORDER BY age DESC, name ASC").await;
    assert_eq!(r.rows[0][0], "Alice");
    assert_eq!(r.rows[2][0], "Bob");
}

#[tokio::test]
async fn test_order_limit_offset() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    for i in 1..=10 { exec(&engine, &format!("INSERT INTO t VALUES ({})", i)).await; }
    let r = exec(&engine, "SELECT id FROM t ORDER BY id LIMIT 3").await;
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][0], "1");
    let r = exec(&engine, "SELECT id FROM t ORDER BY id LIMIT 3 OFFSET 5").await;
    assert_eq!(r.rows[0][0], "6");
    let r = exec(&engine, "SELECT id FROM t ORDER BY id OFFSET 8").await;
    assert_eq!(r.rows.len(), 2);
    let r = exec(&engine, "SELECT id FROM t ORDER BY id DESC LIMIT 3").await;
    assert_eq!(r.rows[0][0], "10");
}

// ═══════════════════════════════════════════════════════
// Data types
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_type_numeric() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (i INTEGER, d DOUBLE)").await;
    exec(&engine, "INSERT INTO t VALUES (42, 3.14)").await;
    let r = exec(&engine, "SELECT i, d FROM t").await;
    assert_eq!(r.rows[0][0], "42");
    assert_eq!(r.rows[0][1], "3.14");
}

#[tokio::test]
async fn test_type_varchar() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (name VARCHAR(100), code CHAR(3))").await;
    exec(&engine, "INSERT INTO t VALUES ('Hello', 'ABC')").await;
    let r = exec(&engine, "SELECT name, code FROM t").await;
    assert_eq!(r.rows[0][0], "Hello");
    assert_eq!(r.rows[0][1], "ABC");
}

#[tokio::test]
async fn test_type_boolean() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, flag BOOLEAN)").await;
    exec(&engine, "INSERT INTO t VALUES (1, true), (2, false)").await;
    let r = exec(&engine, "SELECT flag FROM t ORDER BY id").await;
    assert_eq!(r.rows[0][0], "true");
    assert_eq!(r.rows[1][0], "false");
}

// ═══════════════════════════════════════════════════════
// NULL handling
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_null_is_null() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (NULL), (1)").await;
    let r = exec(&engine, "SELECT val FROM t WHERE val IS NULL").await;
    assert_eq!(r.rows.len(), 1);
}

// ═══════════════════════════════════════════════════════
// INSERT / UPDATE / DELETE
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_insert_multi_row() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, name TEXT)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Charlie')").await;
    let r = exec(&engine, "SELECT name FROM t ORDER BY id").await;
    assert_eq!(r.rows.len(), 3);
}

#[tokio::test]
async fn test_update_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)").await;
    exec(&engine, "UPDATE t SET val = 99 WHERE id > 1").await;
    let r = exec(&engine, "SELECT val FROM t ORDER BY id").await;
    assert_eq!(r.rows[0][0], "10");
    assert_eq!(r.rows[1][0], "99");
}

#[tokio::test]
async fn test_update_all() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 10), (2, 20)").await;
    exec(&engine, "UPDATE t SET val = 99").await;
    let r = exec(&engine, "SELECT val FROM t ORDER BY id").await;
    assert_eq!(r.rows[0][0], "99");
    assert_eq!(r.rows[1][0], "99");
}

#[tokio::test]
async fn test_update_no_match() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;
    exec(&engine, "UPDATE t SET id = 999 WHERE id = 999").await;
    let r = exec(&engine, "SELECT id FROM t").await;
    assert_eq!(r.rows[0][0], "1");
}

#[tokio::test]
async fn test_delete_where() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, val INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1, 10), (2, 20), (3, 30), (4, 40)").await;
    exec(&engine, "DELETE FROM t WHERE val >= 30").await;
    let r = exec(&engine, "SELECT id FROM t ORDER BY id").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_delete_all() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2), (3)").await;
    exec(&engine, "DELETE FROM t").await;
    let r = exec(&engine, "SELECT id FROM t").await;
    assert_eq!(r.rows.len(), 0);
}

// ═══════════════════════════════════════════════════════
// Edge cases: empty results, limits
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_select_no_match() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2)").await;
    let r = exec(&engine, "SELECT id FROM t WHERE id > 100").await;
    assert_eq!(r.rows.len(), 0);
}

#[tokio::test]
async fn test_select_empty_table() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let r = exec(&engine, "SELECT id FROM t").await;
    assert_eq!(r.rows.len(), 0);
}

#[tokio::test]
async fn test_select_one_row() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (42)").await;
    let r = exec(&engine, "SELECT id FROM t").await;
    assert_eq!(r.rows[0][0], "42");
}

#[tokio::test]
async fn test_limit_gt_rows() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1), (2)").await;
    let r = exec(&engine, "SELECT id FROM t ORDER BY id LIMIT 100").await;
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn test_offset_gt_rows() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    exec(&engine, "INSERT INTO t VALUES (1)").await;
    let r = exec(&engine, "SELECT id FROM t OFFSET 100").await;
    assert_eq!(r.rows.len(), 0);
}

// ═══════════════════════════════════════════════════════
// EXPLAIN
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_explain_select() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER)").await;
    let r = exec(&engine, "EXPLAIN SELECT * FROM t").await;
    assert_eq!(r.columns[0], "QUERY PLAN");
    assert!(r.rows.len() > 0);
}

#[tokio::test]
async fn test_explain_join() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (id INTEGER)").await;
    let r = exec(&engine, "EXPLAIN SELECT * FROM t1 JOIN t2 ON t1.id = t2.id").await;
    assert_eq!(r.columns[0], "QUERY PLAN");
    assert!(r.rows.len() > 0);
}

// ═══════════════════════════════════════════════════════
// Multi-statement SQL
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_statement() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER); INSERT INTO t VALUES (1); SELECT * FROM t").await;
    let r = exec(&engine, "SELECT id FROM t").await;
    assert_eq!(r.rows.len(), 1);
}

// ═══════════════════════════════════════════════════════
// UUIDV7
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_uuidv7() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t (id INTEGER, u UUIDV7)").await;
    exec(&engine, "INSERT INTO t VALUES (1, UUIDV7())").await;
    let r = exec(&engine, "SELECT id FROM t WHERE id = 1").await;
    assert_eq!(r.rows[0][0], "1");
}

// ═══════════════════════════════════════════════════════
// SHOW TABLES
// ═══════════════════════════════════════════════════════

#[tokio::test]
async fn test_show_tables() {
    let (engine, _dir) = setup_engine().await;
    exec(&engine, "CREATE TABLE t1 (id INTEGER)").await;
    exec(&engine, "CREATE TABLE t2 (id INTEGER)").await;
    let r = exec(&engine, "SHOW TABLES").await;
    assert_eq!(r.columns[0], "table_name");
    assert_eq!(r.rows.len(), 2);
}
