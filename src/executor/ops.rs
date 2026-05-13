use crate::error::{HelionError, Result};
use crate::executor::eval;
use crate::sql::parser::{BinaryOperator, Expression};
use crate::sql::planner::LogicalPlan;
use crate::storage::engine::DatabaseEngine;
use crate::storage::mvcc::{WriteEntry, WriteOp};
use crate::storage::table::Table;
use crate::storage::types::{Datum, Row};

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub rows_affected: u64,
}

/// Try to resolve a WHERE clause using available indexes.
/// Returns Some(sorted row indices) if an index can be used, None to fall back to full scan.
fn resolve_index_scan(
    table: &Table,
    where_clause: &Option<Expression>,
    table_columns: &[crate::storage::types::ColumnMeta],
) -> Option<Vec<usize>> {
    let clause = where_clause.as_ref()?;

    // Try to match simple patterns that use a single index
    // First, find the best index match for this expression
    let (col_name, op, literal) = match clause {
        Expression::BinaryOp {
            left,
            op,
            right,
        } => {
            let col = match (left.as_ref(), right.as_ref()) {
                (Expression::Column(c), Expression::Literal(v))
                | (Expression::Literal(v), Expression::Column(c)) => {
                    // For non-equality, column must be on the left for correct ordering
                    if !matches!(op, BinaryOperator::Eq | BinaryOperator::Ne) {
                        if !matches!(left.as_ref(), Expression::Column(_)) {
                            return None;
                        }
                    }
                    (c, v)
                }
                _ => return None,
            };
            (col.0, op, col.1)
        }
        Expression::Between { .. }
        | Expression::In { .. } => { return None; }
        _ => return None,
    };

    // Find column index
    let col_idx = table_columns.iter().position(|c| c.name == *col_name)?;

    // Find an index that covers this column
    let index = table.indexes.iter().find(|idx| idx.meta.columns.contains(&col_idx))?;

    let key = vec![literal.clone()];

    match op {
        BinaryOperator::Eq => {
            // Point lookup
            let row_idxs: Vec<usize> = index
                .get(&key)?
                .iter()
                .copied()
                .collect();
            Some(row_idxs)
        }
        // Range scans via B-tree - disabled pending investigation of correctness
        // BinaryOperator::Gt => Some(index.scan_from(key)),
        // BinaryOperator::Ge => Some(index.scan_from(key)),
        // BinaryOperator::Lt => Some(index.scan_to(key)),
        // BinaryOperator::Le => Some(index.scan_to(key)),
        _ => None,
    }
}

/// Execute a plan with an optional current user (None = skip permission checks).
pub async fn execute_as(
    engine: &DatabaseEngine,
    plan: &LogicalPlan,
    current_user: Option<&str>,
) -> Result<QueryResult> {
    match plan {
        // ── Table DDL ─────────────────────────────────────────────────
        LogicalPlan::CreateTable {
            name,
            columns,
            engine: table_engine,
        } => {
            engine
                .create_table(name, columns.clone(), table_engine.as_deref())
                .await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 0,
            })
        }
        LogicalPlan::DropTable { name, if_exists } => match engine.drop_table(name).await {
            Ok(_) => Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 0,
            }),
            Err(HelionError::TableNotFound(_)) if *if_exists => Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 0,
            }),
            Err(e) => Err(e),
        },
        LogicalPlan::AlterTableEngine {
            name,
            engine: target_engine,
        } => {
            engine.alter_table_engine(name, target_engine).await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 0,
            })
        }
        LogicalPlan::Explain {
            analyze,
            verbose,
            statement,
        } => {
            let stmts = crate::sql::parser::parse(statement)?;
            let inner_stmt = stmts
                .first()
                .ok_or_else(|| HelionError::Parse("Expected statement to explain".into()))?;
            let tables = engine.get_tables().await;
            let inner_plan = crate::sql::planner::plan(inner_stmt, &tables)?;

            let mut rows = vec![vec![render_plan(&inner_plan, *verbose)]];
            if *analyze {
                let analyzed = Box::pin(execute_as(engine, &inner_plan, current_user)).await?;
                rows.push(vec![format!(
                    "rows={}, columns={}",
                    analyzed.rows_affected,
                    analyzed.columns.len()
                )]);
            }

            Ok(QueryResult {
                columns: vec!["QUERY PLAN".to_string()],
                column_types: vec!["TEXT".to_string()],
                rows,
                rows_affected: 0,
            })
        }

        // ── DML ───────────────────────────────────────────────────────
        LogicalPlan::Insert { table_name, rows } => {
            // Permission check
            if let Some(user) = current_user {
                let col_names: Vec<&str> = if rows.is_empty() {
                    vec![]
                } else {
                    (0..rows[0].values.len()).map(|_| "").collect()
                };
                engine.check_insert(user, table_name, &col_names).await?;
            }

            let rows_count = rows.len() as u64;
            let entries = {
                let tables = engine.get_tables().await;
                let table = tables
                    .iter()
                    .find(|t| t.name == *table_name)
                    .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
                let start_idx = table.row_count();
                let mut entries = Vec::new();
                for (i, row) in rows.iter().enumerate() {
                    entries.push(WriteEntry {
                        table_name: table_name.clone(),
                        row_idx: start_idx + i,
                        old_txid_max: u64::MAX,
                        operation: WriteOp::Insert(row.clone()),
                    });
                }
                entries
            };
            engine
                .with_write_txn(move |_tx, _tables| Ok(entries))
                .await?;

            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: rows_count,
            })
        }
        LogicalPlan::Select {
            table_name,
            columns,
            wildcard,
            where_clause,
            order_by,
            limit,
            offset,
            table_columns,
        } => {
            let tables = engine.get_tables().await;
            let table = tables
                .iter()
                .find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;

            // Permission check
            if let Some(user) = current_user {
                let col_names: Vec<&str> = if *wildcard || columns.is_empty() {
                    table_columns.iter().map(|c| c.name.as_str()).collect()
                } else {
                    columns
                        .iter()
                        .map(|&i| table_columns[i].name.as_str())
                        .collect()
                };
                engine.check_select(user, table_name, &col_names).await?;
            }

            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            engine.mvcc.commit_transaction(&tx);

            // Try index scan first, fall back to full table scan
            let filtered: Vec<(usize, &Row)> = if let Some(index_rows) =
                resolve_index_scan(table, where_clause, table_columns)
            {
                // Index gave us candidate row indices — fetch and filter by MVCC
                let mut results = Vec::new();
                for &row_idx in &index_rows {
                    if let Some(rv) = table.get_visible_version(row_idx, snapshot_txid, &active_txns)
                    {
                        // If there's a WHERE clause, re-check with the evaluator
                        // (to handle expressions the index couldn't fully resolve)
                        let row = &rv.row;
                        let matches = if let Some(wc) = where_clause {
                            eval::evaluate(wc, &row.values, table_columns)
                                .map(|d| matches!(d, Datum::Boolean(true)))
                                .unwrap_or(false)
                        } else {
                            true
                        };
                        if matches {
                            results.push((row_idx, row));
                        }
                    }
                }
                results
            } else {
                // Full table scan
                let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
                if let Some(wc) = where_clause {
                    visible_rows
                        .into_iter()
                        .filter(|(_, row)| {
                            eval::evaluate(wc, &row.values, table_columns)
                                .map(|d| matches!(d, Datum::Boolean(true)))
                                .unwrap_or(false)
                        })
                        .collect()
                } else {
                    visible_rows
                }
            };

            let mut sorted: Vec<(usize, &Row)> = filtered;
            if !order_by.is_empty() {
                sorted.sort_by(|a, b| {
                    for order in order_by {
                        let a_val = eval::evaluate(&order.expr, &a.1.values, table_columns).ok();
                        let b_val = eval::evaluate(&order.expr, &b.1.values, table_columns).ok();
                        let cmp = match (&a_val, &b_val) {
                            (Some(av), Some(bv)) => {
                                eval::compare_datums(av, bv).unwrap_or(std::cmp::Ordering::Equal)
                            }
                            _ => std::cmp::Ordering::Equal,
                        };
                        if cmp != std::cmp::Ordering::Equal {
                            return if matches!(
                                order.direction,
                                crate::sql::parser::OrderByDesc::Desc
                            ) {
                                cmp.reverse()
                            } else {
                                cmp
                            };
                        }
                    }
                    std::cmp::Ordering::Equal
                });
            }

            let offset_val = offset.unwrap_or(0) as usize;
            let sliced: Vec<&Row> = if let Some(lim) = limit {
                sorted
                    .iter()
                    .skip(offset_val)
                    .take(*lim as usize)
                    .map(|(_, r)| *r)
                    .collect()
            } else {
                sorted.iter().skip(offset_val).map(|(_, r)| *r).collect()
            };

            let col_names: Vec<String> = if *wildcard || columns.is_empty() {
                table_columns.iter().map(|c| c.name.clone()).collect()
            } else {
                columns
                    .iter()
                    .map(|&i| table_columns[i].name.clone())
                    .collect()
            };
            let col_types: Vec<String> = if *wildcard || columns.is_empty() {
                table_columns
                    .iter()
                    .map(|c| c.data_type.to_string())
                    .collect()
            } else {
                columns
                    .iter()
                    .map(|&i| table_columns[i].data_type.to_string())
                    .collect()
            };
            let proj_indices: Vec<usize> = if *wildcard || columns.is_empty() {
                (0..table_columns.len()).collect()
            } else {
                columns.clone()
            };

            let rows: Vec<Vec<String>> = sliced
                .iter()
                .map(|row| {
                    proj_indices
                        .iter()
                        .map(|&i| {
                            row.values
                                .get(i)
                                .map(|d| d.display())
                                .unwrap_or_else(|| "NULL".to_string())
                        })
                        .collect()
                })
                .collect();
            let rows_affected = rows.len() as u64;
            Ok(QueryResult {
                columns: col_names,
                column_types: col_types,
                rows,
                rows_affected,
            })
        }
        LogicalPlan::Update {
            table_name,
            set_indices,
            set_values,
            where_clause,
            table_columns,
        } => {
            // Permission check
            if let Some(user) = current_user {
                let col_names: Vec<&str> = set_indices
                    .iter()
                    .map(|&i| table_columns[i].name.as_str())
                    .collect();
                engine.check_update(user, table_name, &col_names).await?;
            }

            let tables = engine.get_tables().await;
            let table = tables
                .iter()
                .find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
            engine.mvcc.commit_transaction(&tx);

            let to_update: Vec<(usize, &Row)> = if let Some(wc) = where_clause {
                visible_rows
                    .into_iter()
                    .filter(|(_, row)| {
                        eval::evaluate(wc, &row.values, table_columns)
                            .map(|d| matches!(d, Datum::Boolean(true)))
                            .unwrap_or(false)
                    })
                    .collect()
            } else {
                visible_rows
            };

            let mut entries = Vec::new();
            for (row_idx, old_row) in &to_update {
                let mut new_values = old_row.values.clone();
                for (i, &set_idx) in set_indices.iter().enumerate() {
                    if let Some(val) = set_values.get(i) {
                        new_values[set_idx] = val.clone();
                    }
                }
                entries.push(WriteEntry {
                    table_name: table_name.clone(),
                    row_idx: *row_idx,
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Update(Row::new(new_values)),
                });
            }
            let affected = entries.len() as u64;
            if !entries.is_empty() {
                engine
                    .with_write_txn(move |_tx, _tables| Ok(entries))
                    .await?;
            }
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: affected,
            })
        }
        LogicalPlan::Delete {
            table_name,
            where_clause,
            table_columns,
        } => {
            // Permission check
            if let Some(user) = current_user {
                engine.check_delete(user, table_name).await?;
            }

            let tables = engine.get_tables().await;
            let table = tables
                .iter()
                .find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
            engine.mvcc.commit_transaction(&tx);

            let to_delete: Vec<(usize, &Row)> = if let Some(wc) = where_clause {
                visible_rows
                    .into_iter()
                    .filter(|(_, row)| {
                        eval::evaluate(wc, &row.values, table_columns)
                            .map(|d| matches!(d, Datum::Boolean(true)))
                            .unwrap_or(false)
                    })
                    .collect()
            } else {
                visible_rows
            };

            let entries: Vec<WriteEntry> = to_delete
                .iter()
                .map(|(row_idx, _)| WriteEntry {
                    table_name: table_name.clone(),
                    row_idx: *row_idx,
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Delete,
                })
                .collect();
            let affected = entries.len() as u64;
            if !entries.is_empty() {
                engine
                    .with_write_txn(move |_tx, _tables| Ok(entries))
                    .await?;
            }
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: affected,
            })
        }

        // ── User Management ───────────────────────────────────────────
        LogicalPlan::CreateUser { username, password } => {
            engine.create_user(username, password).await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            })
        }
        LogicalPlan::DropUser {
            username,
            if_exists,
        } => match engine.drop_user(username).await {
            Ok(_) => Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            }),
            Err(_) if *if_exists => Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 0,
            }),
            Err(e) => Err(e),
        },
        LogicalPlan::AlterUser { username, password } => {
            engine.alter_user_password(username, password).await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            })
        }
        LogicalPlan::Grant {
            username,
            table,
            permission,
            ..
        } => {
            engine
                .grant_permission(username, table, permission.clone())
                .await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            })
        }
        LogicalPlan::Revoke {
            username,
            table,
            permission,
            ..
        } => {
            engine
                .revoke_permission(username, table, permission)
                .await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            })
        }

        // ── Index Management ───────────────────────────────────────────
        LogicalPlan::CreateIndex {
            name,
            table: table_name,
            columns: col_names,
            unique,
            if_not_exists,
        } => {
            let tables = engine.get_tables().await;
            let table = tables
                .iter()
                .find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;

            if *if_not_exists && table.indexes.iter().any(|i| i.meta.name == *name) {
                return Ok(QueryResult {
                    columns: vec![],
                    column_types: vec![],
                    rows: vec![],
                    rows_affected: 0,
                });
            }

            if table.indexes.iter().any(|i| i.meta.name == *name) {
                return Err(HelionError::IndexAlreadyExists(name.clone()));
            }

            // Resolve column names to indices
            let col_indices: Vec<usize> = col_names
                .iter()
                .map(|c| {
                    table
                        .columns
                        .iter()
                        .position(|col| col.name == *c)
                        .ok_or_else(|| HelionError::ColumnNotFound(format!("{}.{}", table_name, c)))
                })
                .collect::<Result<Vec<_>>>()?;

            // We need to get a mutable reference to the table to add the index.
            // Since we're outside the engine's internal write lock, we use
            // engine's tables API. We need a method to add an index to a table.
            // For now, we use the engine's get_tables and a direct approach.
            let mut tables = engine.get_tables().await;
            let table_mut = tables
                .iter_mut()
                .find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;

            table_mut.add_index(name, col_indices, *unique)?;

            // Write back the tables
            *engine.tables.write().await = tables;

            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            })
        }
        LogicalPlan::DropIndex {
            name,
            table: table_name,
            if_exists,
        } => {
            let mut tables = engine.get_tables().await;
            let table = tables
                .iter_mut()
                .find(|t| t.name == *table_name);

            match table {
                Some(t) => {
                    if let Err(e) = t.drop_index(name) {
                        if !*if_exists {
                            return Err(e);
                        }
                    }
                }
                None => {
                    if !*if_exists {
                        return Err(HelionError::TableNotFound(table_name.clone()));
                    }
                }
            }

            *engine.tables.write().await = tables;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 1,
            })
        }
    }
}

/// Execute a plan (backward-compatible, no permission checks).
pub async fn execute(engine: &DatabaseEngine, plan: &LogicalPlan) -> Result<QueryResult> {
    execute_as(engine, plan, None).await
}

fn render_plan(plan: &LogicalPlan, verbose: bool) -> String {
    if verbose {
        format!("{:#?}", plan)
    } else {
        format!("{:?}", plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::parse;
    use crate::sql::planner::plan;
    use tempfile::TempDir;

    async fn setup_engine() -> (DatabaseEngine, TempDir) {
        let dir = TempDir::new().unwrap();
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        (engine, dir)
    }

    #[tokio::test]
    async fn test_execute_create_table() {
        let (engine, _dir) = setup_engine().await;
        let stmts = parse("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        execute(&engine, &plan(&stmts[0], &[]).unwrap())
            .await
            .unwrap();
        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "users");
    }

    #[tokio::test]
    async fn test_execute_insert_and_select() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let insert_sql = "INSERT INTO users VALUES (1, 'Alice', 30)";
        let stmts = parse(insert_sql).unwrap();
        let tables = engine.get_tables().await;
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();
        let select_sql = "SELECT * FROM users";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let result = execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], "1");
    }

    #[tokio::test]
    async fn test_execute_explain() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(&parse("CREATE TABLE users (id INTEGER)").unwrap()[0], &[]).unwrap(),
        )
        .await
        .unwrap();

        let stmts = parse("EXPLAIN SELECT * FROM users").unwrap();
        let tables = engine.get_tables().await;
        let result = execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();
        assert_eq!(result.columns, vec!["QUERY PLAN"]);
        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0][0].contains("Select"));
    }

    // ── User Management Tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_execute_create_user() {
        let (engine, _dir) = setup_engine().await;
        let stmts = parse("CREATE USER alice WITH PASSWORD 'secret'").unwrap();
        let plan = plan(&stmts[0], &[]).unwrap();
        let result = execute(&engine, &plan).await.unwrap();
        assert_eq!(result.rows_affected, 1);
        assert!(engine.user_exists("alice").await);
        assert!(engine.verify_user("alice", "secret").await);
    }

    #[tokio::test]
    async fn test_execute_drop_user() {
        let (engine, _dir) = setup_engine().await;
        let stmts = parse("CREATE USER alice WITH PASSWORD 'secret'").unwrap();
        execute(&engine, &plan(&stmts[0], &[]).unwrap())
            .await
            .unwrap();
        assert!(engine.user_exists("alice").await);

        let stmts = parse("DROP USER alice").unwrap();
        execute(&engine, &plan(&stmts[0], &[]).unwrap())
            .await
            .unwrap();
        assert!(!engine.user_exists("alice").await);
    }

    #[tokio::test]
    async fn test_execute_alter_user() {
        let (engine, _dir) = setup_engine().await;
        let stmts = parse("CREATE USER alice WITH PASSWORD 'old'").unwrap();
        execute(&engine, &plan(&stmts[0], &[]).unwrap())
            .await
            .unwrap();
        assert!(engine.verify_user("alice", "old").await);

        let stmts = parse("ALTER USER alice WITH PASSWORD 'new'").unwrap();
        execute(&engine, &plan(&stmts[0], &[]).unwrap())
            .await
            .unwrap();
        assert!(engine.verify_user("alice", "new").await);
        assert!(!engine.verify_user("alice", "old").await);
    }

    // ── Grant/Revoke Tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_execute_grant() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER alice WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(&parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[]).unwrap(),
        )
        .await
        .unwrap();

        let stmts = parse("GRANT SELECT ON t TO alice").unwrap();
        let tables = engine.get_tables().await;
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();

        assert!(engine
            .permissions
            .read()
            .await
            .can_select("alice", "t", &["id"]));
    }

    #[tokio::test]
    async fn test_execute_grant_select_columns() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER alice WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(
                &parse("CREATE TABLE t (id INTEGER, name TEXT, secret INTEGER)").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let stmts = parse("GRANT SELECT(id, name) ON t TO alice").unwrap();
        let tables = engine.get_tables().await;
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();

        assert!(engine
            .permissions
            .read()
            .await
            .can_select("alice", "t", &["id", "name"]));
        assert!(!engine
            .permissions
            .read()
            .await
            .can_select("alice", "t", &["secret"]));
    }

    #[tokio::test]
    async fn test_execute_grant_all() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER alice WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(&parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[]).unwrap(),
        )
        .await
        .unwrap();

        let stmts = parse("GRANT ALL ON t TO alice").unwrap();
        let tables = engine.get_tables().await;
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();

        assert!(engine
            .permissions
            .read()
            .await
            .can_select("alice", "t", &["id"]));
        assert!(engine
            .permissions
            .read()
            .await
            .can_insert("alice", "t", &["id"]));
        assert!(engine
            .permissions
            .read()
            .await
            .can_update("alice", "t", &["id"]));
        assert!(engine.permissions.read().await.can_delete("alice", "t"));
    }

    #[tokio::test]
    async fn test_execute_revoke() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER alice WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(&parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[]).unwrap(),
        )
        .await
        .unwrap();

        let tables = engine.get_tables().await;
        let stmts = parse("GRANT ALL ON t TO alice").unwrap();
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();
        assert!(engine.permissions.read().await.can_delete("alice", "t"));

        let stmts = parse("REVOKE ALL ON t FROM alice").unwrap();
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();
        assert!(!engine.permissions.read().await.can_delete("alice", "t"));
    }

    // ── Permission Check Tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_permission_check_select_denied() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER bob WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(&parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[]).unwrap(),
        )
        .await
        .unwrap();

        let tables = engine.get_tables().await;
        let stmts = parse("SELECT * FROM t").unwrap();
        let plan = plan(&stmts[0], &tables).unwrap();
        let result = execute_as(&engine, &plan, Some("bob")).await;
        assert!(result.is_err());
        assert!(matches!(result, Err(HelionError::PermissionDenied(_))));
    }

    #[tokio::test]
    async fn test_permission_check_select_granted() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER bob WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(&parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[]).unwrap(),
        )
        .await
        .unwrap();

        let tables = engine.get_tables().await;
        let stmts = parse("GRANT SELECT ON t TO bob").unwrap();
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();

        let stmts = parse("SELECT * FROM t").unwrap();
        let tables = engine.get_tables().await;
        let plan = plan(&stmts[0], &tables).unwrap();
        let result = execute_as(&engine, &plan, Some("bob")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_permission_check_column_level() {
        let (engine, _dir) = setup_engine().await;
        execute(
            &engine,
            &plan(
                &parse("CREATE USER bob WITH PASSWORD 'pw'").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        execute(
            &engine,
            &plan(
                &parse("CREATE TABLE t (id INTEGER, secret TEXT)").unwrap()[0],
                &[],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let tables = engine.get_tables().await;
        let stmts = parse("GRANT SELECT(id) ON t TO bob").unwrap();
        execute(&engine, &plan(&stmts[0], &tables).unwrap())
            .await
            .unwrap();

        // Selecting only 'id' should be allowed
        let stmts = parse("SELECT id FROM t").unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute_as(&engine, &logical_plan, Some("bob")).await;
        assert!(result.is_ok());

        // Selecting 'secret' should be denied
        let stmts = parse("SELECT secret FROM t").unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute_as(&engine, &logical_plan, Some("bob")).await;
        assert!(result.is_err());
    }
}
