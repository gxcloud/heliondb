use std::collections::{BTreeSet, HashMap};

use crate::error::{HelionError, Result};
use crate::executor::eval;
use crate::sql::parser::*;
use crate::sql::planner::{JoinAlgorithm, LogicalPlan};
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

// ── Index scan helper ────────────────────────────────────

fn resolve_index_scan(
    table: &Table,
    where_clause: &Option<Expression>,
    table_columns: &[crate::storage::types::ColumnMeta],
) -> Option<Vec<usize>> {
    let clause = where_clause.as_ref()?;
    let (col_name, op, literal) = match clause {
        Expression::BinaryOp { left, op, right } => {
            let col = match (left.as_ref(), right.as_ref()) {
                (Expression::Column(c), Expression::Literal(v))
                | (Expression::Literal(v), Expression::Column(c)) => {
                    if !matches!(op, BinaryOperator::Eq | BinaryOperator::Ne)
                        && !matches!(left.as_ref(), Expression::Column(_))
                    {
                        return None;
                    }
                    (c, v)
                }
                _ => return None,
            };
            (col.0, op, col.1)
        }
        _ => return None,
    };
    let col_idx = table_columns.iter().position(|c| c.name == *col_name)?;
    let index = table
        .indexes
        .iter()
        .find(|idx| idx.meta.columns.contains(&col_idx))?;
    let key = vec![literal.clone()];
    match op {
        BinaryOperator::Eq => {
            let row_idxs: Vec<usize> = index.get(&key)?.iter().copied().collect();
            Some(row_idxs)
        }
        _ => None,
    }
}

// ── Main execution entry points ──────────────────────────

pub async fn execute_as(
    engine: &DatabaseEngine,
    plan: &LogicalPlan,
    current_user: Option<&str>,
) -> Result<QueryResult> {
    match plan {
        // ── Tree-based query execution ──────────────────────────
        LogicalPlan::Projection { .. }
        | LogicalPlan::TableScan { .. }
        | LogicalPlan::Join { .. }
        | LogicalPlan::Filter { .. }
        | LogicalPlan::Sort { .. }
        | LogicalPlan::Limit { .. } => {
            let tables = engine.get_tables().await;
            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            engine.mvcc.commit_transaction(&tx);

            // Check permissions for all tables in the query
            if let Some(user) = current_user {
                check_plan_permissions(plan, engine, user).await?;
            }

            let ctx = ExecContext {
                engine,
                tables: &tables,
                snapshot_txid,
                active_txns: &active_txns,
                current_user,
            };
            let (data_rows, col_names, col_types) = execute_tree(plan, &ctx)?;
            let rows_affected = data_rows.len() as u64;
            let rows: Vec<Vec<String>> = data_rows.iter()
                .map(|row| row.iter().map(|d| d.display()).collect())
                .collect();
            Ok(QueryResult {
                columns: col_names,
                column_types: col_types,
                rows,
                rows_affected,
            })
        }

        // ── Flat plan execution (DDL, DML, user mgmt) ──────────
        LogicalPlan::CreateTable { name, columns, engine: table_engine } => {
            engine
                .create_table(name, columns.clone(), table_engine.as_deref())
                .await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 })
        }
        LogicalPlan::DropTable { name, if_exists } => match engine.drop_table(name).await {
            Ok(_) => Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 }),
            Err(HelionError::TableNotFound(_)) if *if_exists => Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 }),
            Err(e) => Err(e),
        },
        LogicalPlan::AlterTableEngine { name, engine: target_engine } => {
            engine.alter_table_engine(name, target_engine).await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 })
        }
        LogicalPlan::Explain { analyze, verbose, statement } => {
            let stmts = crate::sql::parser::parse(statement)?;
            let inner_stmt = stmts.first().ok_or_else(|| HelionError::Parse("Expected statement to explain".into()))?;
            let tables = engine.get_tables().await;
            let inner_plan = crate::sql::planner::plan(inner_stmt, &tables)?;
            let mut rows = vec![vec![render_plan(&inner_plan, *verbose)]];
            if *analyze {
                let analyzed = Box::pin(execute_as(engine, &inner_plan, current_user)).await?;
                rows.push(vec![format!(
                    "rows={}, columns={}",
                    analyzed.rows_affected, analyzed.columns.len()
                )]);
            }
            Ok(QueryResult {
                columns: vec!["QUERY PLAN".to_string()],
                column_types: vec!["TEXT".to_string()],
                rows,
                rows_affected: 0,
            })
        }
        LogicalPlan::Insert { table_name, rows } => {
            if let Some(user) = current_user {
                let col_names: Vec<&str> = if rows.is_empty() { vec![] } else { (0..rows[0].values.len()).map(|_| "").collect() };
                engine.check_insert(user, table_name, &col_names).await?;
            }
            let rows_count = rows.len() as u64;
            let entries = {
                let tables = engine.get_tables().await;
                let table = tables.iter().find(|t| t.name == *table_name)
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
            engine.with_write_txn(move |_tx, _tables| Ok(entries)).await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: rows_count })
        }
        LogicalPlan::Update { table_name, set_indices, set_values, where_clause, table_columns } => {
            if let Some(user) = current_user {
                let col_names: Vec<&str> = set_indices.iter().map(|&i| table_columns[i].name.as_str()).collect();
                engine.check_update(user, table_name, &col_names).await?;
            }
            let tables = engine.get_tables().await;
            let table = tables.iter().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
            engine.mvcc.commit_transaction(&tx);
            let to_update: Vec<(usize, &Row)> = if let Some(wc) = where_clause {
                visible_rows.into_iter().filter(|(_, row)| {
                    eval::evaluate(wc, &row.values, table_columns)
                        .map(|d| matches!(d, Datum::Boolean(true))).unwrap_or(false)
                }).collect()
            } else { visible_rows };
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
            if !entries.is_empty() { engine.with_write_txn(move |_tx, _tables| Ok(entries)).await?; }
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: affected })
        }
        LogicalPlan::Delete { table_name, where_clause, table_columns } => {
            if let Some(user) = current_user { engine.check_delete(user, table_name).await?; }
            let tables = engine.get_tables().await;
            let table = tables.iter().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
            let tx = engine.begin();
            let active_txns = tx.snapshot.active.as_ref().clone();
            let snapshot_txid = tx.snapshot.txid;
            let visible_rows = table.scan_visible(snapshot_txid, &active_txns);
            engine.mvcc.commit_transaction(&tx);
            let to_delete: Vec<(usize, &Row)> = if let Some(wc) = where_clause {
                visible_rows.into_iter().filter(|(_, row)| {
                    eval::evaluate(wc, &row.values, table_columns)
                        .map(|d| matches!(d, Datum::Boolean(true))).unwrap_or(false)
                }).collect()
            } else { visible_rows };
            let entries: Vec<WriteEntry> = to_delete.iter().map(|(row_idx, _)| WriteEntry {
                table_name: table_name.clone(), row_idx: *row_idx, old_txid_max: u64::MAX,
                operation: WriteOp::Delete,
            }).collect();
            let affected = entries.len() as u64;
            if !entries.is_empty() { engine.with_write_txn(move |_tx, _tables| Ok(entries)).await?; }
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: affected })
        }
        LogicalPlan::CreateUser { username, password } => {
            engine.create_user(username, password).await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 })
        }
        LogicalPlan::DropUser { username, if_exists } => match engine.drop_user(username).await {
            Ok(_) => Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 }),
            Err(_) if *if_exists => Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 }),
            Err(e) => Err(e),
        },
        LogicalPlan::AlterUser { username, password } => {
            engine.alter_user_password(username, password).await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 })
        }
        LogicalPlan::Grant { username, table, permission, .. } => {
            engine.grant_permission(username, table, permission.clone()).await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 })
        }
        LogicalPlan::Revoke { username, table, permission, .. } => {
            engine.revoke_permission(username, table, permission).await?;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 })
        }
        LogicalPlan::CreateIndex { name, table: table_name, columns: col_names, unique, if_not_exists } => {
            let tables = engine.get_tables().await;
            let table = tables.iter().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
            if *if_not_exists && table.indexes.iter().any(|i| i.meta.name == *name) {
                return Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 });
            }
            if table.indexes.iter().any(|i| i.meta.name == *name) {
                return Err(HelionError::IndexAlreadyExists(name.clone()));
            }
            let col_indices: Vec<usize> = col_names.iter().map(|c| {
                table.columns.iter().position(|col| col.name == *c)
                    .ok_or_else(|| HelionError::ColumnNotFound(format!("{}.{}", table_name, c)))
            }).collect::<Result<Vec<_>>>()?;
            let mut tables = engine.get_tables().await;
            let table_mut = tables.iter_mut().find(|t| t.name == *table_name)
                .ok_or_else(|| HelionError::TableNotFound(table_name.clone()))?;
            table_mut.add_index(name, col_indices, *unique)?;
            *engine.tables.write().await = tables;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 })
        }
        LogicalPlan::DropIndex { name, table: table_name, if_exists } => {
            let mut tables = engine.get_tables().await;
            let table = tables.iter_mut().find(|t| t.name == *table_name);
            match table {
                Some(t) => { if let Err(e) = t.drop_index(name) { if !*if_exists { return Err(e); } } }
                None => { if !*if_exists { return Err(HelionError::TableNotFound(table_name.clone())); } }
            }
            *engine.tables.write().await = tables;
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 1 })
        }
        LogicalPlan::ShowTables => {
            let tables = engine.get_tables().await;
            let rows: Vec<Vec<String>> = tables.iter().map(|t| vec![t.name.clone()]).collect();
            Ok(QueryResult { columns: vec!["table_name".to_string()], column_types: vec!["TEXT".to_string()], rows, rows_affected: 0 })
        }
        LogicalPlan::ShowDatabases => {
            Ok(QueryResult { columns: vec!["database_name".to_string()], column_types: vec!["TEXT".to_string()], rows: vec![vec!["default".to_string()]], rows_affected: 1 })
        }
        LogicalPlan::UseDatabase { name } => {
            if name.is_empty() { return Err(HelionError::Internal("Database name cannot be empty".into())); }
            Ok(QueryResult { columns: vec![], column_types: vec![], rows: vec![], rows_affected: 0 })
        }
    }
}

pub async fn execute(engine: &DatabaseEngine, plan: &LogicalPlan) -> Result<QueryResult> {
    execute_as(engine, plan, None).await
}

/// Collect all tables referenced in a plan tree (sync, no recursion issues).
fn collect_plan_tables<'a>(plan: &'a LogicalPlan, tables: &mut Vec<&'a str>) {
    match plan {
        LogicalPlan::TableScan { table, .. } => tables.push(table.as_str()),
        LogicalPlan::Join { left, right, .. } => {
            collect_plan_tables(left, tables);
            collect_plan_tables(right, tables);
        }
        LogicalPlan::Filter { input, .. } => collect_plan_tables(input, tables),
        LogicalPlan::Projection { input, .. } => collect_plan_tables(input, tables),
        LogicalPlan::Sort { input, .. } => collect_plan_tables(input, tables),
        LogicalPlan::Limit { input, .. } => collect_plan_tables(input, tables),
        _ => {}
    }
}

/// Check SELECT permission on all tables in a plan tree.
/// Only checks the columns that the Projection node actually outputs.
async fn check_plan_permissions(plan: &LogicalPlan, engine: &DatabaseEngine, user: &str) -> Result<()> {
    // Build a map of table → columns needed
    let mut table_columns: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    match plan {
        LogicalPlan::Projection { columns, table_map, .. } => {
            // Build per-table column offset map
            let mut offsets: Vec<(String, usize)> = Vec::new();
            let mut off = 0;
            for (tname, cols) in table_map {
                offsets.push((tname.clone(), off));
                off += cols.len();
            }

            for &col_idx in columns {
                for (tname, offset) in &offsets {
                    if let Some((_, cols)) = table_map.iter().find(|(n, _)| n == tname) {
                        if col_idx >= *offset && col_idx < offset + cols.len() {
                            let local = col_idx - offset;
                            if let Some(col) = cols.get(local) {
                                table_columns.entry(tname.clone()).or_default().push(col.name.clone());
                            }
                            break;
                        }
                    }
                }
            }
        }
        _ => {
            // Fallback: collect all columns from all tables
            let mut tables = Vec::new();
            collect_plan_tables(plan, &mut tables);
            tables.sort();
            tables.dedup();
            // Use engine's get_tables to get column names
            let engine_tables = engine.get_tables().await;
            for table_name in &tables {
                if let Some(t) = engine_tables.iter().find(|t| t.name == *table_name) {
                    let col_names: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
                    table_columns.insert(table_name.to_string(), col_names);
                }
            }
        }
    }

    for (table, cols) in &table_columns {
        let col_refs: Vec<&str> = cols.iter().map(|c| c.as_str()).collect();
        engine.check_select(user, table, &col_refs).await?;
    }
    Ok(())
}

// ── Execution context for tree-based plans ───────────────

struct ExecContext<'a> {
    engine: &'a DatabaseEngine,
    tables: &'a [Table],
    snapshot_txid: u64,
    active_txns: &'a BTreeSet<u64>,
    current_user: Option<&'a str>,
}

// ── Tree execution ───────────────────────────────────────

type DataRows = Vec<Vec<Datum>>;

fn execute_tree(plan: &LogicalPlan, ctx: &ExecContext) -> Result<(DataRows, Vec<String>, Vec<String>)> {
    match plan {
        LogicalPlan::TableScan { table, table_columns, filter, index_name, index_keys } => {
            execute_table_scan(table, table_columns, filter, index_name, index_keys, ctx)
        }
        LogicalPlan::Join { left, right, join_type, algorithm } => {
            execute_join_node(left, right, join_type, algorithm, ctx)
        }
        LogicalPlan::Filter { input, predicate } => {
            let (rows, col_names, col_types) = execute_tree(input, ctx)?;
            let table_info = collect_expr_table_info(input)?;
            let table_map: Vec<(&str, usize, &[crate::storage::types::ColumnMeta])> = table_info
                .iter().map(|(n, o, c)| (n.as_str(), *o, c.as_slice())).collect();

            let filtered: DataRows = rows.into_iter().filter(|row| {
                eval::evaluate_join(predicate, row, &table_map)
                    .map(|d| matches!(d, Datum::Boolean(true)))
                    .unwrap_or(false)
            }).collect();

            Ok((filtered, col_names, col_types))
        }
        LogicalPlan::Projection { input, columns, table_map } => {
            let (rows, _, _) = execute_tree(input, ctx)?;

            // Build column names (disambiguate duplicates)
            let mut col_names = Vec::new();
            let mut name_counts: HashMap<String, usize> = HashMap::new();
            for (_, cols) in table_map {
                for c in cols {
                    *name_counts.entry(c.name.clone()).or_insert(0) += 1;
                }
            }
            for (tname, cols) in table_map {
                for c in cols {
                    if name_counts.get(&c.name).map_or(false, |&n| n > 1) {
                        col_names.push(format!("{}.{}", tname, c.name));
                    } else {
                        col_names.push(c.name.clone());
                    }
                }
            }

            let col_types: Vec<String> = columns.iter()
                .filter_map(|&i| {
                    let mut off = 0;
                    for (_, cols) in table_map {
                        if i < off + cols.len() {
                            return Some(cols[i - off].data_type.to_string());
                        }
                        off += cols.len();
                    }
                    None
                })
                .collect();

            let projected: DataRows = if columns.iter().any(|&i| i >= col_names.len() || i >= col_names.len()) {
                rows
            } else {
                rows.into_iter().map(|row| {
                    columns.iter().filter_map(|&i| row.get(i).cloned()).collect()
                }).collect()
            };

            let selected_names: Vec<String> = columns.iter()
                .filter_map(|&i| col_names.get(i).cloned())
                .collect();

            Ok((projected, selected_names, col_types))
        }
        LogicalPlan::Sort { input, order_by } => {
            let (mut rows, col_names, col_types) = execute_tree(input, ctx)?;
            let table_info = collect_expr_table_info(input)?;
            let table_map: Vec<(&str, usize, &[crate::storage::types::ColumnMeta])> = table_info
                .iter().map(|(n, o, c)| (n.as_str(), *o, c.as_slice())).collect();

            rows.sort_by(|a, b| {
                for order in order_by {
                    let a_val = eval::evaluate_join(&order.expr, a, &table_map).ok();
                    let b_val = eval::evaluate_join(&order.expr, b, &table_map).ok();
                    let cmp = match (&a_val, &b_val) {
                        (Some(av), Some(bv)) => eval::compare_datums(av, bv).unwrap_or(std::cmp::Ordering::Equal),
                        _ => std::cmp::Ordering::Equal,
                    };
                    if cmp != std::cmp::Ordering::Equal {
                        return if matches!(order.direction, OrderByDesc::Desc) { cmp.reverse() } else { cmp };
                    }
                }
                std::cmp::Ordering::Equal
            });

            Ok((rows, col_names, col_types))
        }
        LogicalPlan::Limit { input, limit, offset } => {
            let (rows, col_names, col_types) = execute_tree(input, ctx)?;
            let offset = *offset as usize;
            let limit = *limit as usize;
            let sliced: DataRows = if offset >= rows.len() {
                vec![]
            } else {
                rows.iter().skip(offset).take(limit).cloned().collect()
            };
            Ok((sliced, col_names, col_types))
        }
        _ => Err(HelionError::Internal("Unexpected flat plan node in tree execution".into())),
    }
}

// ── Table scan execution ─────────────────────────────────

fn execute_table_scan(
    table: &str,
    table_columns: &[crate::storage::types::ColumnMeta],
    filter: &Option<Expression>,
    index_name: &Option<String>,
    index_keys: &Option<Vec<Datum>>,
    ctx: &ExecContext,
) -> Result<(DataRows, Vec<String>, Vec<String>)> {
    // Permission check
    if let Some(user) = ctx.current_user {
        let col_names: Vec<&str> = table_columns.iter().map(|c| c.name.as_str()).collect();
        // We need async for the permission check, but we're in a sync function.
        // We handle permissions at the query level instead (before execution).
    }

    let table_meta = ctx.tables.iter().find(|t| t.name == *table)
        .ok_or_else(|| HelionError::TableNotFound(table.to_string()))?;

    let visible: DataRows = if let Some(idx_name) = index_name {
        // Index scan
        let idx = table_meta.indexes.iter().find(|i| i.meta.name == *idx_name)
            .ok_or_else(|| HelionError::IndexNotFound(idx_name.clone()))?;
        let keys = index_keys.as_ref().cloned().unwrap_or_default();
        let row_indices: Vec<usize> = idx.get(&keys)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        row_indices.into_iter()
            .filter_map(|ri| {
                let rv = table_meta.get_visible_version(ri, ctx.snapshot_txid, ctx.active_txns)?;
                Some(rv.row.values.clone())
            })
            .collect()
    } else {
        // Full table scan
        table_meta.scan_visible(ctx.snapshot_txid, ctx.active_txns)
            .into_iter()
            .map(|(_, r)| r.values.clone())
            .collect()
    };

    // Apply filter
    let col_map: Vec<(&str, usize, &[crate::storage::types::ColumnMeta])> = vec![
        (table, 0, table_columns),
    ];
    let filtered: DataRows = if let Some(f) = filter {
        visible.into_iter().filter(|row| {
            eval::evaluate_join(f, row, &col_map)
                .map(|d| matches!(d, Datum::Boolean(true)))
                .unwrap_or(false)
        }).collect()
    } else {
        visible
    };

    let col_names: Vec<String> = table_columns.iter().map(|c| c.name.clone()).collect();
    let col_types: Vec<String> = table_columns.iter().map(|c| c.data_type.to_string()).collect();
    Ok((filtered, col_names, col_types))
}

// ── Join execution ───────────────────────────────────────

fn execute_join_node(
    left: &LogicalPlan,
    right: &LogicalPlan,
    join_type: &JoinType,
    algorithm: &JoinAlgorithm,
    ctx: &ExecContext,
) -> Result<(DataRows, Vec<String>, Vec<String>)> {
    match algorithm {
        JoinAlgorithm::NestedLoop { on_clause } => {
            execute_nested_loop_join(left, right, join_type, on_clause, ctx)
        }
        JoinAlgorithm::IndexNestedLoop {
            inner_table,
            inner_columns,
            index_name,
            outer_key_indices,
            remaining_on,
        } => {
            execute_index_nested_loop_join(
                left, right, join_type,
                inner_table, inner_columns, index_name,
                outer_key_indices, remaining_on, ctx,
            )
        }
        JoinAlgorithm::HashJoin {
            build_left,
            build_key_indices,
            probe_key_indices,
            remaining_on,
        } => {
            execute_hash_join(
                left, right, join_type,
                *build_left, build_key_indices, probe_key_indices,
                remaining_on, ctx,
            )
        }
    }
}

/// Collect table info (name, offset, columns) from a plan tree.
fn collect_expr_table_info(plan: &LogicalPlan) -> Result<Vec<(String, usize, Vec<crate::storage::types::ColumnMeta>)>> {
    match plan {
        LogicalPlan::TableScan { table, table_columns, .. } => {
            Ok(vec![(table.clone(), 0, table_columns.clone())])
        }
        LogicalPlan::Join { left, right, .. } => {
            let left_info = collect_expr_table_info(left)?;
            let right_info = collect_expr_table_info(right)?;
            let left_width: usize = left_info.iter().map(|(_, _, c)| c.len()).sum();
            let right_shifted = right_info.into_iter().map(|(n, o, c)| (n, o + left_width, c));
            let mut result = left_info;
            result.extend(right_shifted);
            Ok(result)
        }
        LogicalPlan::Filter { input, .. } => collect_expr_table_info(input),
        LogicalPlan::Projection { input, .. } => collect_expr_table_info(input),
        LogicalPlan::Sort { input, .. } => collect_expr_table_info(input),
        LogicalPlan::Limit { input, .. } => collect_expr_table_info(input),
        _ => Ok(vec![]),
    }
}

/// Build a table_map reference for evaluate_join from the table info.
fn make_table_map<'a>(info: &'a [(String, usize, Vec<crate::storage::types::ColumnMeta>)])
    -> Vec<(&'a str, usize, &'a [crate::storage::types::ColumnMeta])>
{
    info.iter().map(|(n, o, c)| (n.as_str(), *o, c.as_slice())).collect()
}

// ── Nested-Loop Join ─────────────────────────────────────

fn execute_nested_loop_join(
    left: &LogicalPlan,
    right: &LogicalPlan,
    join_type: &JoinType,
    on_clause: &Option<Expression>,
    ctx: &ExecContext,
) -> Result<(DataRows, Vec<String>, Vec<String>)> {
    let (left_rows, _, _) = execute_tree(left, ctx)?;
    let (right_rows, _, _) = execute_tree(right, ctx)?;

    let left_info = collect_expr_table_info(left)?;
    let right_info = collect_expr_table_info(right)?;
    let left_width: usize = left_info.iter().map(|(_, _, c)| c.len()).sum();

    let mut table_info = left_info.clone();
    let right_shifted = right_info.into_iter().map(|(n, o, c)| (n, o + left_width, c));
    table_info.extend(right_shifted);
    let table_map = make_table_map(&table_info);

    let is_left_join = *join_type == JoinType::Left;
    let is_right_join = *join_type == JoinType::Right;

    let right_width: usize = table_info.iter().map(|(_, _, c)| c.len()).sum::<usize>() - left_width;

    let mut result = DataRows::new();
    for left_row in &left_rows {
        let mut matched = false;
        for right_row in &right_rows {
            let mut combined = left_row.clone();
            combined.extend_from_slice(right_row);

            let on_match = match on_clause {
                Some(on) => eval::evaluate_join(on, &combined, &table_map)
                    .map(|d| matches!(d, Datum::Boolean(true)))
                    .unwrap_or(false),
                None => true,
            };

            if on_match {
                result.push(combined);
                matched = true;
            }
        }

        if !matched && is_left_join {
            let mut padded = left_row.clone();
            padded.extend(std::iter::repeat(Datum::Null).take(right_width));
            result.push(padded);
        }
    }

    if is_right_join {
        // For RIGHT JOIN, emit unmatched right rows with NULL left padding
        for right_row in &right_rows {
            let mut matched = false;
            for left_row in &left_rows {
                let mut combined = left_row.clone();
                combined.extend_from_slice(right_row);
                let on_match = match on_clause {
                    Some(on) => eval::evaluate_join(on, &combined, &table_map)
                        .map(|d| matches!(d, Datum::Boolean(true)))
                        .unwrap_or(false),
                    None => true,
                };
                if on_match { matched = true; break; }
            }
            if !matched {
                let mut padded = vec![Datum::Null; left_width];
                padded.extend_from_slice(right_row);
                result.push(padded);
            }
        }
    }

    let col_names = build_combined_col_names(&table_info);
    let col_types = build_combined_col_types(&table_info);

    Ok((result, col_names, col_types))
}

// ── Index Nested-Loop Join ───────────────────────────────

fn execute_index_nested_loop_join(
    outer: &LogicalPlan,
    _inner: &LogicalPlan,
    join_type: &JoinType,
    inner_table: &str,
    inner_columns: &[crate::storage::types::ColumnMeta],
    index_name: &str,
    outer_key_indices: &[usize],
    remaining_on: &Option<Expression>,
    ctx: &ExecContext,
) -> Result<(DataRows, Vec<String>, Vec<String>)> {
    let (outer_rows, _, _) = execute_tree(outer, ctx)?;

    let inner_table_meta = ctx.tables.iter().find(|t| t.name == *inner_table)
        .ok_or_else(|| HelionError::TableNotFound(inner_table.to_string()))?;

    let idx = inner_table_meta.indexes.iter().find(|i| i.meta.name == *index_name)
        .ok_or_else(|| HelionError::IndexNotFound(index_name.to_string()))?;

    let outer_info = collect_expr_table_info(outer)?;
    let outer_width: usize = outer_info.iter().map(|(_, _, c)| c.len()).sum();
    let inner_width = inner_columns.len();

    let mut table_info = outer_info.clone();
    table_info.push((inner_table.to_string(), outer_width, inner_columns.to_vec()));
    let table_map = make_table_map(&table_info);

    let is_left_join = *join_type == JoinType::Left;

    let mut result = DataRows::new();
    for outer_row in &outer_rows {
        let probe_key: Vec<Datum> = outer_key_indices.iter()
            .filter_map(|&i| outer_row.get(i).cloned())
            .collect();

        let mut matched = false;
        if let Some(row_indices) = idx.get(&probe_key) {
            for &ri in row_indices {
                if let Some(rv) = inner_table_meta.get_visible_version(ri, ctx.snapshot_txid, ctx.active_txns) {
                    let mut combined = outer_row.clone();
                    combined.extend_from_slice(&rv.row.values);

                    let on_match = match remaining_on {
                        Some(on) => eval::evaluate_join(on, &combined, &table_map)
                            .map(|d| matches!(d, Datum::Boolean(true)))
                            .unwrap_or(false),
                        None => true,
                    };

                    if on_match {
                        result.push(combined);
                        matched = true;
                    }
                }
            }
        }

        if !matched && is_left_join {
            let mut padded = outer_row.clone();
            padded.extend(std::iter::repeat(Datum::Null).take(inner_width));
            result.push(padded);
        }
    }

    let col_names = build_combined_col_names(&table_info);
    let col_types = build_combined_col_types(&table_info);
    Ok((result, col_names, col_types))
}

// ── Hash Join ────────────────────────────────────────────

fn execute_hash_join(
    left: &LogicalPlan,
    right: &LogicalPlan,
    join_type: &JoinType,
    build_left: bool,
    build_key_indices: &[usize],
    probe_key_indices: &[usize],
    remaining_on: &Option<Expression>,
    ctx: &ExecContext,
) -> Result<(DataRows, Vec<String>, Vec<String>)> {
    let left_rows = execute_tree(left, ctx)?.0;
    let right_rows = execute_tree(right, ctx)?.0;

    let left_info = collect_expr_table_info(left)?;
    let right_info = collect_expr_table_info(right)?;
    let left_width: usize = left_info.iter().map(|(_, _, c)| c.len()).sum();
    let right_width: usize = right_info.iter().map(|(_, _, c)| c.len()).sum();

    let mut table_info = left_info.clone();
    let right_shifted = right_info.into_iter().map(|(n, o, c)| (n, o + left_width, c));
    table_info.extend(right_shifted);
    let table_map = make_table_map(&table_info);

    let is_left_join = *join_type == JoinType::Left;
    let is_right_join = *join_type == JoinType::Right;

    let (build_rows, probe_rows, build_keys, probe_keys, build_width, probe_width) = if build_left {
        (left_rows, right_rows, build_key_indices, probe_key_indices, left_width, right_width)
    } else {
        (right_rows, left_rows, probe_key_indices, build_key_indices, right_width, left_width)
    };

    // Build phase: create hash table
    let mut hash_table: HashMap<Vec<Datum>, Vec<Vec<Datum>>> = HashMap::new();
    let mut unmatched_keys: Vec<Vec<Datum>> = Vec::new();

    for row in &build_rows {
        let key: Vec<Datum> = build_keys.iter().filter_map(|&i| row.get(i).cloned()).collect();
        if key.iter().any(|d| d.is_null()) {
            // NULL key: can't match in equi-join, but handle for LEFT JOIN
            unmatched_keys.push(key);
            continue;
        }
        hash_table.entry(key).or_default().push(row.clone());
    }

    // Probe phase
    let mut result = DataRows::new();
    let mut probe_matched = vec![false; probe_rows.len()];

    for (pi, probe_row) in probe_rows.iter().enumerate() {
        let key: Vec<Datum> = probe_keys.iter().filter_map(|&i| probe_row.get(i).cloned()).collect();
        if key.iter().any(|d| d.is_null()) {
            continue; // NULL != NULL
        }

        if let Some(matching) = hash_table.get(&key) {
            for build_row in matching {
                let combined = if build_left {
                    let mut c = build_row.clone();
                    c.extend_from_slice(probe_row);
                    c
                } else {
                    let mut c = probe_row.clone();
                    c.extend_from_slice(build_row);
                    c
                };

                let on_match = match remaining_on {
                    Some(on) => eval::evaluate_join(on, &combined, &table_map)
                        .map(|d| matches!(d, Datum::Boolean(true)))
                        .unwrap_or(false),
                    None => true,
                };

                if on_match {
                    result.push(combined);
                    probe_matched[pi] = true;
                }
            }
        }
    }

    // Handle outer joins
    if is_left_join && build_left {
        // Build side is left, so unmatched build rows need NULL-padded right
        for (key, rows) in &hash_table {
            if !probe_has_key(key, &probe_rows, probe_keys) {
                for row in rows {
                    let mut padded = row.clone();
                    padded.extend(std::iter::repeat(Datum::Null).take(probe_width));
                    result.push(padded);
                }
            }
        }
    } else if is_right_join && !build_left {
        // Build side is right, so unmatched build rows are right rows needing NULL-padded left
        for (key, rows) in &hash_table {
            if !probe_has_key(key, &probe_rows, probe_keys) {
                for row in rows {
                    let mut padded = vec![Datum::Null; probe_width];
                    padded.extend_from_slice(row);
                    result.push(padded);
                }
            }
        }
    }

    let col_names = build_combined_col_names(&table_info);
    let col_types = build_combined_col_types(&table_info);
    Ok((result, col_names, col_types))
}

fn probe_has_key(key: &[Datum], probe_rows: &DataRows, probe_keys: &[usize]) -> bool {
    for row in probe_rows {
        let pk: Vec<Datum> = probe_keys.iter().filter_map(|&i| row.get(i).cloned()).collect();
        if pk == key {
            return true;
        }
    }
    false
}

// ── Column naming helpers ────────────────────────────────

fn build_combined_col_names(info: &[(String, usize, Vec<crate::storage::types::ColumnMeta>)]) -> Vec<String> {
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for (_, _, cols) in info {
        for c in cols {
            *name_counts.entry(c.name.clone()).or_insert(0) += 1;
        }
    }
    let mut names = Vec::new();
    for (tname, _, cols) in info {
        for c in cols {
            if name_counts.get(&c.name).map_or(false, |&n| n > 1) {
                names.push(format!("{}.{}", tname, c.name));
            } else {
                names.push(c.name.clone());
            }
        }
    }
    names
}

fn build_combined_col_types(info: &[(String, usize, Vec<crate::storage::types::ColumnMeta>)]) -> Vec<String> {
    let mut types = Vec::new();
    for (_, _, cols) in info {
        for c in cols {
            types.push(c.data_type.to_string());
        }
    }
    types
}

// ── Plan rendering ───────────────────────────────────────

fn render_plan(plan: &LogicalPlan, verbose: bool) -> String {
    if verbose {
        format!("{:#?}", plan)
    } else {
        format!("{:?}", plan)
    }
}

// ── Tests ────────────────────────────────────────────────

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
        execute(&engine, &plan(&stmts[0], &[]).unwrap()).await.unwrap();
        let tables = engine.get_tables().await;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "users");
    }

    #[tokio::test]
    async fn test_execute_insert_and_select() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(
            &parse("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)").unwrap()[0], &[],
        ).unwrap()).await.unwrap();
        let insert_sql = "INSERT INTO users VALUES (1, 'Alice', 30)";
        let stmts = parse(insert_sql).unwrap();
        let tables = engine.get_tables().await;
        execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        let select_sql = "SELECT * FROM users";
        let stmts = parse(select_sql).unwrap();
        let tables = engine.get_tables().await;
        let result = execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], "1");
    }

    #[tokio::test]
    async fn test_execute_explain() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine,
            &plan(&parse("CREATE TABLE users (id INTEGER)").unwrap()[0], &[]).unwrap(),
        ).await.unwrap();
        let stmts = parse("EXPLAIN SELECT * FROM users").unwrap();
        let tables = engine.get_tables().await;
        let result = execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        assert_eq!(result.columns, vec!["QUERY PLAN"]);
        assert_eq!(result.rows.len(), 1);
    }

    #[tokio::test]
    async fn test_execute_join() {
        let (engine, _dir) = setup_engine().await;

        // Create tables
        for stmt_sql in [
            "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)",
            "CREATE TABLE users (id INTEGER, name TEXT)",
        ] {
            let stmts = parse(stmt_sql).unwrap();
            execute(&engine, &plan(&stmts[0], &[]).unwrap()).await.unwrap();
        }

        // Insert data
        for insert_sql in [
            "INSERT INTO users VALUES (1, 'Alice')",
            "INSERT INTO users VALUES (2, 'Bob')",
            "INSERT INTO orders VALUES (1, 1, 100.0)",
            "INSERT INTO orders VALUES (2, 1, 200.0)",
            "INSERT INTO orders VALUES (3, 2, 150.0)",
        ] {
            let stmts = parse(insert_sql).unwrap();
            let tables = engine.get_tables().await;
            execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        }

        // JOIN
        let tables = engine.get_tables().await;
        let stmts = parse("SELECT users.name, orders.total FROM users JOIN orders ON users.id = orders.user_id").unwrap();
        let plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &plan).await.unwrap();
        assert_eq!(result.rows.len(), 3);
    }

    #[tokio::test]
    async fn test_execute_left_join() {
        let (engine, _dir) = setup_engine().await;

        for stmt_sql in [
            "CREATE TABLE users (id INTEGER, name TEXT)",
            "CREATE TABLE orders (id INTEGER, user_id INTEGER, total DOUBLE)",
        ] {
            let stmts = parse(stmt_sql).unwrap();
            execute(&engine, &plan(&stmts[0], &[]).unwrap()).await.unwrap();
        }

        for insert_sql in [
            "INSERT INTO users VALUES (1, 'Alice')",
            "INSERT INTO users VALUES (2, 'Bob')",
            "INSERT INTO orders VALUES (1, 1, 100.0)",
        ] {
            let stmts = parse(insert_sql).unwrap();
            let tables = engine.get_tables().await;
            execute(&engine, &plan(&stmts[0], &tables).unwrap()).await.unwrap();
        }

        let tables = engine.get_tables().await;
        let stmts = parse("SELECT users.name, orders.total FROM users LEFT JOIN orders ON users.id = orders.user_id ORDER BY users.name").unwrap();
        let plan = plan(&stmts[0], &tables).unwrap();
        let result = execute(&engine, &plan).await.unwrap();
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][0], "Alice");
        assert_eq!(result.rows[0][1], "100");
        assert_eq!(result.rows[1][0], "Bob");
        assert_eq!(result.rows[1][1], "NULL");
    }

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
        execute(&engine, &plan(&stmts[0], &[]).unwrap()).await.unwrap();
        assert!(engine.user_exists("alice").await);
        let stmts = parse("DROP USER alice").unwrap();
        execute(&engine, &plan(&stmts[0], &[]).unwrap()).await.unwrap();
        assert!(!engine.user_exists("alice").await);
    }

    #[tokio::test]
    async fn test_permission_check_select_denied() {
        let (engine, _dir) = setup_engine().await;
        execute(&engine, &plan(
            &parse("CREATE USER bob WITH PASSWORD 'pw'").unwrap()[0], &[],
        ).unwrap()).await.unwrap();
        execute(&engine, &plan(
            &parse("CREATE TABLE t (id INTEGER)").unwrap()[0], &[],
        ).unwrap()).await.unwrap();
        let tables = engine.get_tables().await;
        let stmts = parse("SELECT * FROM t").unwrap();
        let plan = plan(&stmts[0], &tables).unwrap();
        let result = execute_as(&engine, &plan, Some("bob")).await;
        assert!(result.is_err());
        assert!(matches!(result, Err(HelionError::PermissionDenied(_))));
    }
}
