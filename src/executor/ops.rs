use crate::error::{HelionError, Result};
use crate::executor::eval;
use crate::sql::planner::LogicalPlan;
use crate::storage::engine::DatabaseEngine;
use crate::storage::mvcc::{WriteEntry, WriteOp};
use crate::storage::types::{Datum, Row};

/// Result of executing a query.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub rows_affected: u64,
}

/// Execute a logical plan against the database engine.
pub async fn execute(
    engine: &DatabaseEngine,
    plan: &LogicalPlan,
) -> Result<QueryResult> {
    match plan {
        LogicalPlan::CreateTable { name, columns } => {
            engine.create_table(name, columns.clone()).await?;
            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: 0,
            })
        }
        LogicalPlan::DropTable { name, if_exists } => {
            match engine.drop_table(name).await {
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
            }
        }
        LogicalPlan::Insert { table_name, rows } => {
            let rows_count = rows.len() as u64;
            let entries = {
                let tables = engine.get_tables().await;
                let table = tables.iter().find(|t| t.name == *table_name)
                    .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
                let start_idx = table.row_count();
                let mut entries = Vec::new();
                for (i, row) in rows.iter().enumerate() {
                    let entry = WriteEntry {
                        table_name: table_name.clone(),
                        row_idx: start_idx + i,
                        old_txid_max: u64::MAX,
                        operation: WriteOp::Insert(row.clone()),
                    };
                    entries.push(entry);
                }
                entries
            };

            engine.with_write_txn(move |_tx, _tables| {
                Ok(entries)
            }).await?;

            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: rows_count,
            })
        }
        LogicalPlan::Select { table_name, columns, wildcard, where_clause, order_by, limit, offset, table_columns } => {
            let tables = engine.get_tables().await;
            let table = tables.iter().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;

            // Get visible rows using MVCC
            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            engine.mvcc.commit_transaction(&tx); // read-only, no need to keep active

            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);

            // Apply WHERE filter
            let filtered: Vec<(usize, &Row)> = if let Some(where_clause) = where_clause {
                visible_rows.into_iter()
                    .filter(|(_, row)| {
                        eval::evaluate(where_clause, &row.values, table_columns)
                            .map(|d| matches!(d, Datum::Boolean(true)))
                            .unwrap_or(false)
                    })
                    .collect()
            } else {
                visible_rows
            };

            // Apply ORDER BY (simple sort)
            let mut sorted: Vec<(usize, &Row)> = filtered;
            if !order_by.is_empty() {
                sorted.sort_by(|a, b| {
                    for order in order_by {
                        let a_val = eval::evaluate(&order.expr, &a.1.values, table_columns).ok();
                        let b_val = eval::evaluate(&order.expr, &b.1.values, table_columns).ok();
                        let cmp = match (&a_val, &b_val) {
                            (Some(av), Some(bv)) => av.partial_cmp(bv).unwrap_or(std::cmp::Ordering::Equal),
                            _ => std::cmp::Ordering::Equal,
                        };
                        if cmp != std::cmp::Ordering::Equal {
                            return if matches!(order.direction, crate::sql::parser::OrderByDesc::Desc) {
                                cmp.reverse()
                            } else {
                                cmp
                            };
                        }
                    }
                    std::cmp::Ordering::Equal
                });
            }

            // Apply OFFSET and LIMIT
            let offset = offset.unwrap_or(0) as usize;
            let sliced: Vec<&Row> = if let Some(limit) = limit {
                sorted.iter().skip(offset).take(*limit as usize).map(|(_, r)| *r).collect()
            } else {
                sorted.iter().skip(offset).map(|(_, r)| *r).collect()
            };

            // Apply projection
            let col_names: Vec<String> = if *wildcard || columns.is_empty() {
                table_columns.iter().map(|c| c.name.clone()).collect()
            } else {
                columns.iter().map(|&i| table_columns[i].name.clone()).collect()
            };

            let col_types: Vec<String> = if *wildcard || columns.is_empty() {
                table_columns.iter().map(|c| c.data_type.to_string()).collect()
            } else {
                columns.iter().map(|&i| table_columns[i].data_type.to_string()).collect()
            };

            let projected_indices: Vec<usize> = if *wildcard || columns.is_empty() {
                (0..table_columns.len()).collect()
            } else {
                columns.clone()
            };

            let rows: Vec<Vec<String>> = sliced.iter().map(|row| {
                projected_indices.iter().map(|&i| {
                    row.values.get(i).map(|d| d.display()).unwrap_or_else(|| "NULL".to_string())
                }).collect()
            }).collect();
            let rows_affected = rows.len() as u64;

            Ok(QueryResult {
                columns: col_names,
                column_types: col_types,
                rows,
                rows_affected,
            })
        }
        LogicalPlan::Update { table_name, set_indices, set_values, where_clause, table_columns } => {
            // First get the rows to update
            let tables = engine.get_tables().await;
            let table = tables.iter().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;

            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
            engine.mvcc.commit_transaction(&tx);

            // Filter and determine which rows to update
            let to_update: Vec<(usize, &Row)> = if let Some(wc) = where_clause {
                visible_rows.into_iter()
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
                let new_row = Row::new(new_values);
                entries.push(WriteEntry {
                    table_name: table_name.clone(),
                    row_idx: *row_idx,
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Update(new_row),
                });
            }

            let affected = entries.len() as u64;
            if !entries.is_empty() {
                engine.with_write_txn(move |_tx, _tables| {
                    Ok(entries)
                }).await?;
            }

            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: affected,
            })
        }
        LogicalPlan::Delete { table_name, where_clause, table_columns } => {
            let tables = engine.get_tables().await;
            let table = tables.iter().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;

            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
            engine.mvcc.commit_transaction(&tx);

            let to_delete: Vec<(usize, &Row)> = if let Some(wc) = where_clause {
                visible_rows.into_iter()
                    .filter(|(_, row)| {
                        eval::evaluate(wc, &row.values, table_columns)
                            .map(|d| matches!(d, Datum::Boolean(true)))
                            .unwrap_or(false)
                    })
                    .collect()
            } else {
                visible_rows
            };

            let entries: Vec<WriteEntry> = to_delete.iter().map(|(row_idx, _)| {
                WriteEntry {
                    table_name: table_name.clone(),
                    row_idx: *row_idx,
                    old_txid_max: u64::MAX,
                    operation: WriteOp::Delete,
                }
            }).collect();

            let affected = entries.len() as u64;
            if !entries.is_empty() {
                engine.with_write_txn(move |_tx, _tables| {
                    Ok(entries)
                }).await?;
            }

            Ok(QueryResult {
                columns: vec![],
                column_types: vec![],
                rows: vec![],
                rows_affected: affected,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::parse;
    use crate::sql::planner::plan;
    use crate::storage::types::Datum;
    use tempfile::TempDir;

    async fn setup_engine() -> (DatabaseEngine, TempDir) {
        let dir = TempDir::new().unwrap();
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        (engine, dir)
    }

    #[tokio::test]
    async fn test_execute_create_table() {
        let (engine, _dir) = setup_engine().await;
        let sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)";
        let stmts = parse(sql).unwrap();
        let logical_plan = plan(&stmts[0], &[]).unwrap();
        let result = execute(&engine, &logical_plan).await.unwrap();
        assert_eq!(result.rows_affected, 0);

        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "users");
    }

    #[tokio::test]
    async fn test_execute_insert_and_select() {
        let (engine, _dir) = setup_engine().await;

        // Create table
        let create_sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)";
        execute(&engine, &plan(&parse(create_sql).unwrap()[0], &[]).unwrap()).await.unwrap();

        // Insert rows
        let insert_sql = "INSERT INTO users VALUES (1, 'Alice', 30)";
        let stmts = parse(insert_sql).unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &logical_plan).await.unwrap();
        assert_eq!(result.rows_affected, 1);

        // Select *
        let select_sql = "SELECT * FROM users";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &logical_plan).await.unwrap();
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], "1");
        assert_eq!(result.rows[0][1], "Alice");
        assert_eq!(result.rows[0][2], "30");
    }

    #[tokio::test]
    async fn test_execute_where_clause() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(&parse("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").unwrap()[0], &[]).unwrap()).await.unwrap();

        // Insert rows
        for (id, name, age) in &[(1, "Alice", 30), (2, "Bob", 25), (3, "Charlie", 35)] {
            let sql = format!("INSERT INTO users VALUES ({}, '{}', {})", id, name, age);
            let stmts = parse(&sql).unwrap();
            let tables = engine.get_tables().await;
            execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        }

        // Select with WHERE
        let select_sql = "SELECT name FROM users WHERE age > 30";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &logical_plan).await.unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], "Charlie");
    }

    #[tokio::test]
    async fn test_execute_update() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(&parse("CREATE TABLE users (id INTEGER, name TEXT)").unwrap()[0], &[]).unwrap()).await.unwrap();

        let stmts = parse("INSERT INTO users VALUES (1, 'Alice')").unwrap();
        let tables = engine.get_tables().await;
        execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();

        // Update
        let update_sql = "UPDATE users SET name = 'Alicia' WHERE id = 1";
        let stmts = parse(update_sql).unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &logical_plan).await.unwrap();
        assert_eq!(result.rows_affected, 1);

        // Verify
        let select_sql = "SELECT name FROM users";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let verify_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &verify_plan).await.unwrap();
        assert_eq!(result.rows[0][0], "Alicia");
    }

    #[tokio::test]
    async fn test_execute_delete() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(&parse("CREATE TABLE users (id INTEGER, name TEXT)").unwrap()[0], &[]).unwrap()).await.unwrap();

        for i in 1..=3 {
            let sql = format!("INSERT INTO users VALUES ({}, 'User{}')", i, i);
            let stmts = parse(&sql).unwrap();
            let tables = engine.get_tables().await;
            execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        }

        // Delete one
        let delete_sql = "DELETE FROM users WHERE id = 2";
        let stmts = parse(delete_sql).unwrap();
        let tables = engine.get_tables().await;
        let result = execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        assert_eq!(result.rows_affected, 1);

        // Verify
        let select_sql = "SELECT * FROM users";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let result = execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        assert_eq!(result.rows.len(), 2);
    }

    #[tokio::test]
    async fn test_execute_order_by_limit_offset() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(&parse("CREATE TABLE items (id INTEGER, val INTEGER)").unwrap()[0], &[]).unwrap()).await.unwrap();

        for i in 1..=5 {
            let sql = format!("INSERT INTO items VALUES ({}, {})", i, 6 - i);
            let stmts = parse(&sql).unwrap();
            let tables = engine.get_tables().await;
            execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        }

        let select_sql = "SELECT id FROM items ORDER BY val ASC LIMIT 2 OFFSET 1";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let logical_plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &logical_plan).await.unwrap();
        assert_eq!(result.rows.len(), 2);
        // val order: (5,1), (4,2), (3,3), (2,4), (1,5) after sort by val ASC
        // OFFSET 1, LIMIT 2: (4,2), (3,3) -> ids: 4, 3
        assert_eq!(result.rows[0][0], "4");
        assert_eq!(result.rows[1][0], "3");
    }

    #[tokio::test]
    async fn test_execute_drop_table() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(&parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[]).unwrap()).await.unwrap();
        execute(&engine, &plan(&parse("DROP TABLE t").unwrap()[0], &[]).unwrap()).await.unwrap();
        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 0);
    }

    #[tokio::test]
    async fn test_execute_table_not_found() {
        let (_engine, _dir) = setup_engine().await;
        let stmts = parse("SELECT * FROM nonexistent").unwrap();
        let result = plan(&stmts[0], &[]);
        assert!(result.is_err());
    }
}
