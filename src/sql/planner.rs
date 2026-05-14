use crate::error::{HelionError, Result};
use crate::sql::parser::*;
use crate::storage::permissions::Permission;
use crate::storage::table::Table;
use crate::storage::types::{coerce_datum, ColumnMeta, Datum, Row};

/// Join algorithm chosen by the optimizer.
#[derive(Debug, Clone)]
pub enum JoinAlgorithm {
    NestedLoop { on_clause: Option<Expression> },
    IndexNestedLoop {
        inner_table: String,
        inner_columns: Vec<ColumnMeta>,
        index_name: String,
        outer_key_indices: Vec<usize>,
        remaining_on: Option<Expression>,
    },
    HashJoin {
        build_left: bool,
        build_key_indices: Vec<usize>,
        probe_key_indices: Vec<usize>,
        remaining_on: Option<Expression>,
    },
}

/// A logical plan node representing a database operation.
/// SELECT queries produce a tree of query nodes; DDL/DML uses flat nodes.
#[derive(Debug, Clone)]
pub enum LogicalPlan {
    // ── Tree-based query plan nodes ───────────────────────
    TableScan {
        table: String,
        table_columns: Vec<ColumnMeta>,
        filter: Option<Expression>,
        index_name: Option<String>,
        index_keys: Option<Vec<Datum>>,
    },
    Join {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        join_type: JoinType,
        algorithm: JoinAlgorithm,
    },
    Filter {
        input: Box<LogicalPlan>,
        predicate: Expression,
    },
    Projection {
        input: Box<LogicalPlan>,
        columns: Vec<usize>,
        table_map: Vec<(String, Vec<ColumnMeta>)>,
    },
    Sort {
        input: Box<LogicalPlan>,
        order_by: Vec<OrderByExpr>,
    },
    Limit {
        input: Box<LogicalPlan>,
        limit: u64,
        offset: u64,
    },

    // ── Flat plan nodes (DDL, DML, user management) ──────
    CreateTable {
        name: String,
        columns: Vec<ColumnMeta>,
        engine: Option<String>,
    },
    Explain {
        analyze: bool,
        verbose: bool,
        statement: String,
    },
    DropTable {
        name: String,
        if_exists: bool,
    },
    AlterTableEngine {
        name: String,
        engine: String,
    },
    Insert {
        table_name: String,
        rows: Vec<Row>,
    },
    Update {
        table_name: String,
        set_indices: Vec<usize>,
        set_values: Vec<Datum>,
        where_clause: Option<Expression>,
        table_columns: Vec<ColumnMeta>,
    },
    Delete {
        table_name: String,
        where_clause: Option<Expression>,
        table_columns: Vec<ColumnMeta>,
    },
    CreateUser {
        username: String,
        password: String,
    },
    DropUser {
        username: String,
        if_exists: bool,
    },
    AlterUser {
        username: String,
        password: String,
    },
    Grant {
        username: String,
        table: String,
        columns: Vec<String>,
        permission: Permission,
    },
    Revoke {
        username: String,
        table: String,
        columns: Vec<String>,
        permission: Permission,
    },
    CreateIndex {
        name: String,
        table: String,
        columns: Vec<String>,
        unique: bool,
        if_not_exists: bool,
    },
    DropIndex {
        name: String,
        table: String,
        if_exists: bool,
    },
    ShowTables,
    ShowDatabases,
    UseDatabase {
        name: String,
    },
}

/// Plan a parsed statement against the available table schemas.
pub fn plan(statement: &HelionStatement, tables: &[Table]) -> Result<LogicalPlan> {
    match statement {
        HelionStatement::CreateTable { name, columns, engine } => {
            Ok(LogicalPlan::CreateTable {
                name: name.clone(),
                columns: columns.clone(),
                engine: engine.clone(),
            })
        }
        HelionStatement::DropTable { name, if_exists } => Ok(LogicalPlan::DropTable {
            name: name.clone(),
            if_exists: *if_exists,
        }),
        HelionStatement::Explain { analyze, verbose, statement } => {
            Ok(LogicalPlan::Explain {
                analyze: *analyze,
                verbose: *verbose,
                statement: statement.clone(),
            })
        }
        HelionStatement::AlterTableEngine { name, engine } => {
            Ok(LogicalPlan::AlterTableEngine {
                name: name.clone(),
                engine: engine.clone(),
            })
        }
        HelionStatement::Insert { table_name, columns, values } => {
            let table = find_table(tables, table_name)?;
            let col_indices = if columns.is_empty() {
                (0..table.columns.len()).collect()
            } else {
                columns
                    .iter()
                    .map(|c| {
                        table.column_index(c).ok_or_else(|| {
                            HelionError::ColumnNotFound(format!("{}.{}", table_name, c))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            };

            let mut rows = Vec::new();
            for row_values in values {
                let mut full_row = vec![Datum::Null; table.columns.len()];
                for (i, val) in row_values.iter().enumerate() {
                    if i < col_indices.len() {
                        let target_type = &table.columns[col_indices[i]].data_type;
                        full_row[col_indices[i]] = coerce_datum(val, target_type)?;
                    }
                }
                let row = Row::new(full_row);
                table.validate_row(&row)?;
                rows.push(row);
            }

            Ok(LogicalPlan::Insert {
                table_name: table_name.clone(),
                rows,
            })
        }
        HelionStatement::Select {
            table_name,
            columns,
            where_clause,
            order_by,
            limit,
            offset,
            joins,
        } => {
            let mut plan = plan_select(
                table_name, columns, where_clause, order_by, *limit, *offset, joins, tables,
            )?;
            plan = optimize(plan, tables)?;
            Ok(plan)
        }
        HelionStatement::Update { table_name, assignments, where_clause } => {
            let table = find_table(tables, table_name)?;
            let mut set_indices = Vec::new();
            let mut set_values = Vec::new();
            for (col_name, val) in assignments {
                let idx = table.column_index(col_name).ok_or_else(|| {
                    HelionError::ColumnNotFound(format!("{}.{}", table_name, col_name))
                })?;
                set_indices.push(idx);
                let coerced = coerce_datum(val, &table.columns[idx].data_type)?;
                set_values.push(coerced);
            }
            Ok(LogicalPlan::Update {
                table_name: table_name.clone(),
                set_indices,
                set_values,
                where_clause: where_clause.clone(),
                table_columns: table.columns.clone(),
            })
        }
        HelionStatement::Delete { table_name, where_clause } => {
            let table = find_table(tables, table_name)?;
            Ok(LogicalPlan::Delete {
                table_name: table_name.clone(),
                where_clause: where_clause.clone(),
                table_columns: table.columns.clone(),
            })
        }
        HelionStatement::CreateUser { username, password } => {
            Ok(LogicalPlan::CreateUser {
                username: username.clone(),
                password: password.clone(),
            })
        }
        HelionStatement::DropUser { username, if_exists } => {
            Ok(LogicalPlan::DropUser {
                username: username.clone(),
                if_exists: *if_exists,
            })
        }
        HelionStatement::AlterUser { username, password } => {
            Ok(LogicalPlan::AlterUser {
                username: username.clone(),
                password: password.clone(),
            })
        }
        HelionStatement::Grant { username, table, columns, permission_type } => {
            let permission = match permission_type {
                GrantPermissionType::Select => Permission::Select(columns.clone()),
                GrantPermissionType::Insert => Permission::Insert(columns.clone()),
                GrantPermissionType::Update => Permission::Update(columns.clone()),
                GrantPermissionType::Delete => Permission::Delete,
                GrantPermissionType::All => Permission::All,
            };
            Ok(LogicalPlan::Grant {
                username: username.clone(),
                table: table.clone(),
                columns: columns.clone(),
                permission,
            })
        }
        HelionStatement::Revoke { username, table, columns, permission_type } => {
            let permission = match permission_type {
                GrantPermissionType::Select => Permission::Select(columns.clone()),
                GrantPermissionType::Insert => Permission::Insert(columns.clone()),
                GrantPermissionType::Update => Permission::Update(columns.clone()),
                GrantPermissionType::Delete => Permission::Delete,
                GrantPermissionType::All => Permission::All,
            };
            Ok(LogicalPlan::Revoke {
                username: username.clone(),
                table: table.clone(),
                columns: columns.clone(),
                permission,
            })
        }
        HelionStatement::CreateIndex { name, table, columns, unique, if_not_exists } => {
            let _table_meta = find_table(tables, table)?;
            Ok(LogicalPlan::CreateIndex {
                name: name.clone(),
                table: table.clone(),
                columns: columns.clone(),
                unique: *unique,
                if_not_exists: *if_not_exists,
            })
        }
        HelionStatement::DropIndex { name, table, if_exists } => {
            Ok(LogicalPlan::DropIndex {
                name: name.clone(),
                table: table.clone(),
                if_exists: *if_exists,
            })
        }
        HelionStatement::ShowTables => Ok(LogicalPlan::ShowTables),
        HelionStatement::ShowDatabases => Ok(LogicalPlan::ShowDatabases),
        HelionStatement::UseDatabase { name } => {
            Ok(LogicalPlan::UseDatabase { name: name.clone() })
        }
    }
}

// ── SELECT planning ──────────────────────────────────────

fn plan_select(
    table_name: &str,
    columns: &[SelectColumn],
    where_clause: &Option<Expression>,
    order_by: &[OrderByExpr],
    limit: Option<u64>,
    offset: Option<u64>,
    joins: &[JoinClause],
    tables: &[Table],
) -> Result<LogicalPlan> {
    let primary_table = find_table(tables, table_name)?;

    let mut plan: LogicalPlan = LogicalPlan::TableScan {
        table: table_name.to_string(),
        table_columns: primary_table.columns.clone(),
        filter: None,
        index_name: None,
        index_keys: None,
    };

    for jc in joins {
        let right_table = find_table(tables, &jc.right_table)?;
        let right_scan = LogicalPlan::TableScan {
            table: jc.right_table.clone(),
            table_columns: right_table.columns.clone(),
            filter: None,
            index_name: None,
            index_keys: None,
        };
        plan = LogicalPlan::Join {
            left: Box::new(plan),
            right: Box::new(right_scan),
            join_type: jc.join_type.clone(),
            algorithm: JoinAlgorithm::NestedLoop {
                on_clause: jc.on_clause.clone(),
            },
        };
    }

    if let Some(wc) = where_clause {
        plan = LogicalPlan::Filter {
            input: Box::new(plan),
            predicate: wc.clone(),
        };
    }

    let all_tables = resolve_table_list(table_name, joins, tables)?;
    let table_map: Vec<(String, Vec<ColumnMeta>)> = all_tables
        .iter()
        .map(|t| (t.name.clone(), t.columns.clone()))
        .collect();

    let combined_len: usize = table_map.iter().map(|(_, cols)| cols.len()).sum();
    let proj_indices = resolve_select_columns(columns, &table_map, combined_len)?;

    // Ordering: Sort before Projection so ORDER BY can reference any column
    if !order_by.is_empty() {
        plan = LogicalPlan::Sort {
            input: Box::new(plan),
            order_by: order_by.to_vec(),
        };
    }

    if limit.is_some() || offset.unwrap_or(0) > 0 {
        plan = LogicalPlan::Limit {
            input: Box::new(plan),
            limit: limit.unwrap_or(u64::MAX),
            offset: offset.unwrap_or(0),
        };
    }

    // Projection last — trims to only the columns requested by SELECT
    plan = LogicalPlan::Projection {
        input: Box::new(plan),
        columns: proj_indices,
        table_map: table_map.clone(),
    };

    Ok(plan)
}

// ── Optimizer ────────────────────────────────────────────

fn optimize(plan: LogicalPlan, tables: &[Table]) -> Result<LogicalPlan> {
    let plan = push_down_predicates(plan, tables)?;
    let plan = select_indexes(plan, tables)?;
    let plan = choose_join_algorithms(plan, tables)?;
    Ok(plan)
}

// ── Predicate Pushdown ───────────────────────────────────

fn push_down_predicates(plan: LogicalPlan, tables: &[Table]) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::Filter { input, predicate } => {
            let conjuncts = decompose_conjunction(&predicate);
            let table_info = collect_table_info(&input, tables)?;
            let (pushable, keep): (Vec<_>, Vec<_>) = conjuncts
                .into_iter()
                .partition(|e| {
                    match referenced_tables(e, &table_info) {
                        Ok(tnames) => tnames.len() <= 1,
                        Err(_) => false,
                    }
                });
            let mut input = push_predicates_into(*input, pushable, tables)?;
            if keep.is_empty() {
                Ok(input)
            } else {
                Ok(LogicalPlan::Filter {
                    input: Box::new(input),
                    predicate: recompose_conjunction(keep),
                })
            }
        }
        LogicalPlan::Join { left, right, join_type, algorithm } => {
            let left = push_down_predicates(*left, tables)?;
            let right = push_down_predicates(*right, tables)?;
            Ok(LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
                join_type,
                algorithm,
            })
        }
        LogicalPlan::Projection { input, columns, table_map } => {
            let input = push_down_predicates(*input, tables)?;
            Ok(LogicalPlan::Projection {
                input: Box::new(input),
                columns,
                table_map,
            })
        }
        LogicalPlan::Sort { input, order_by } => {
            let input = push_down_predicates(*input, tables)?;
            Ok(LogicalPlan::Sort {
                input: Box::new(input),
                order_by,
            })
        }
        LogicalPlan::Limit { input, limit, offset } => {
            let input = push_down_predicates(*input, tables)?;
            Ok(LogicalPlan::Limit {
                input: Box::new(input),
                limit,
                offset,
            })
        }
        other => Ok(other),
    }
}

/// Information about each table in a plan tree.
#[derive(Debug, Clone)]
struct TableInfo {
    name: String,
    columns: Vec<ColumnMeta>,
}

fn collect_table_info(plan: &LogicalPlan, tables: &[Table]) -> Result<Vec<TableInfo>> {
    match plan {
        LogicalPlan::TableScan { table, table_columns, .. } => {
            Ok(vec![TableInfo {
                name: table.clone(),
                columns: table_columns.clone(),
            }])
        }
        LogicalPlan::Join { left, right, .. } => {
            let mut left_info = collect_table_info(left, tables)?;
            let right_info = collect_table_info(right, tables)?;
            left_info.extend(right_info);
            Ok(left_info)
        }
        LogicalPlan::Filter { input, .. } => collect_table_info(input, tables),
        LogicalPlan::Projection { input, .. } => collect_table_info(input, tables),
        LogicalPlan::Sort { input, .. } => collect_table_info(input, tables),
        LogicalPlan::Limit { input, .. } => collect_table_info(input, tables),
        _ => Ok(vec![]),
    }
}

fn referenced_tables(expr: &Expression, tables: &[TableInfo]) -> Result<Vec<String>> {
    match expr {
        Expression::Column(name) => {
            let mut found = Vec::new();
            for ti in tables {
                if ti.columns.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
                    found.push(ti.name.clone());
                }
            }
            if found.is_empty() {
                return Err(HelionError::ColumnNotFound(name.clone()));
            }
            Ok(found)
        }
        Expression::QualifiedColumn(t, _) => Ok(vec![t.clone()]),
        Expression::Literal(_) => Ok(vec![]),
        Expression::BinaryOp { left, right, .. } => {
            let mut l = referenced_tables(left, tables)?;
            let r = referenced_tables(right, tables)?;
            l.extend(r);
            l.sort();
            l.dedup();
            Ok(l)
        }
        Expression::UnaryOp { expr: inner, .. } => referenced_tables(inner, tables),
        Expression::IsNull(inner) | Expression::IsNotNull(inner) => {
            referenced_tables(inner, tables)
        }
        Expression::In { expr: inner, .. } => referenced_tables(inner, tables),
        Expression::Between { expr: inner, low, high, .. } => {
            let mut t = referenced_tables(inner, tables)?;
            t.extend(referenced_tables(low, tables)?);
            t.extend(referenced_tables(high, tables)?);
            t.sort();
            t.dedup();
            Ok(t)
        }
        Expression::Like { expr: inner, .. } => referenced_tables(inner, tables),
        Expression::QualifiedColumn(t, _) => Ok(vec![t.clone()]),
        Expression::Function { args, .. } => {
            let mut ref_tables = Vec::new();
            for arg in args {
                ref_tables.extend(referenced_tables(arg, tables)?);
            }
            ref_tables.sort();
            ref_tables.dedup();
            Ok(ref_tables)
        }
    }
}

fn push_predicates_into(
    plan: LogicalPlan,
    predicates: Vec<Expression>,
    _tables: &[Table],
) -> Result<LogicalPlan> {
    if predicates.is_empty() {
        return Ok(plan);
    }
    match plan {
        LogicalPlan::TableScan {
            table,
            table_columns,
            filter,
            index_name,
            index_keys,
        } => {
            let combined = if let Some(existing) = filter {
                let mut all = decompose_conjunction(&existing);
                all.extend(predicates);
                recompose_conjunction(all)
            } else {
                recompose_conjunction(predicates)
            };
            Ok(LogicalPlan::TableScan {
                table,
                table_columns,
                filter: Some(combined),
                index_name,
                index_keys,
            })
        }
        LogicalPlan::Join { left, right, join_type, algorithm } => {
            // Collect table info BEFORE destructuring to avoid partial move
            let table_info = {
                let left_info = collect_table_info(&left, _tables).unwrap_or_default();
                let right_info = collect_table_info(&right, _tables).unwrap_or_default();
                let mut combined = left_info.clone();
                combined.extend(right_info);
                combined
            };
            let (push_left, rest): (Vec<_>, Vec<_>) = predicates
                .into_iter()
                .partition(|p| {
                    referenced_tables(p, &table_info)
                        .map(|t| {
                            let left_info = collect_table_info(&left, _tables).unwrap_or_default();
                            let left_names: Vec<&str> =
                                left_info.iter().map(|ti| ti.name.as_str()).collect();
                            t.len() == 1 && t.iter().all(|tn| left_names.contains(&tn.as_str()))
                        })
                        .unwrap_or(false)
                });
            let (push_right, keep): (Vec<_>, Vec<_>) = rest.into_iter().partition(|p| {
                referenced_tables(p, &table_info)
                    .map(|t| {
                        let right_info =
                            collect_table_info(&right, _tables).unwrap_or_default();
                        let right_names: Vec<&str> =
                            right_info.iter().map(|ti| ti.name.as_str()).collect();
                        t.len() == 1 && t.iter().all(|tn| right_names.contains(&tn.as_str()))
                    })
                    .unwrap_or(false)
            });
            let left = push_predicates_into(*left, push_left, _tables)?;
            let right = push_predicates_into(*right, push_right, _tables)?;
            let mut result: LogicalPlan = LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
                join_type,
                algorithm,
            };
            if !keep.is_empty() {
                result = LogicalPlan::Filter {
                    input: Box::new(result),
                    predicate: recompose_conjunction(keep),
                };
            }
            Ok(result)
        }
        LogicalPlan::Projection { input, columns, table_map } => {
            let input = push_predicates_into(*input, predicates, _tables)?;
            Ok(LogicalPlan::Projection {
                input: Box::new(input),
                columns,
                table_map,
            })
        }
        LogicalPlan::Sort { input, order_by } => {
            let input = push_predicates_into(*input, predicates, _tables)?;
            Ok(LogicalPlan::Sort {
                input: Box::new(input),
                order_by,
            })
        }
        LogicalPlan::Limit { input, limit, offset } => {
            let input = push_predicates_into(*input, predicates, _tables)?;
            Ok(LogicalPlan::Limit {
                input: Box::new(input),
                limit,
                offset,
            })
        }
        other => {
            // Can't push down; wrap in Filter
            Ok(LogicalPlan::Filter {
                input: Box::new(other),
                predicate: recompose_conjunction(predicates),
            })
        }
    }
}

fn decompose_conjunction(expr: &Expression) -> Vec<Expression> {
    match expr {
        Expression::BinaryOp { left, op: BinaryOperator::And, right } => {
            let mut left_parts = decompose_conjunction(left);
            let mut right_parts = decompose_conjunction(right);
            left_parts.append(&mut right_parts);
            left_parts
        }
        other => vec![other.clone()],
    }
}

fn recompose_conjunction(parts: Vec<Expression>) -> Expression {
    parts.into_iter().reduce(|a, b| Expression::BinaryOp {
        left: Box::new(a),
        op: BinaryOperator::And,
        right: Box::new(b),
    })
    .unwrap_or(Expression::Literal(Datum::Boolean(true)))
}

// ── Index Selection ──────────────────────────────────────

fn select_indexes(plan: LogicalPlan, _tables: &[Table]) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::TableScan {
            table,
            table_columns,
            filter,
            index_name,
            index_keys,
        } => {
            if index_name.is_some() {
                return Ok(LogicalPlan::TableScan {
                    table,
                    table_columns,
                    filter,
                    index_name,
                    index_keys,
                });
            }

            let table_meta = find_table(_tables, &table).ok();
            let chosen = table_meta.and_then(|t| {
                let filter = filter.as_ref()?;
                choose_index_for_filter(t, filter)
            });

            if let Some((idx_name, keys)) = chosen {
                let remaining = remove_indexed_predicate(filter.as_ref(), &idx_name, &table_columns);
                Ok(LogicalPlan::TableScan {
                    table,
                    table_columns,
                    filter: remaining,
                    index_name: Some(idx_name),
                    index_keys: Some(keys),
                })
            } else {
                Ok(LogicalPlan::TableScan {
                    table,
                    table_columns,
                    filter,
                    index_name: None,
                    index_keys: None,
                })
            }
        }
        LogicalPlan::Join { left, right, join_type, algorithm } => {
            let left = select_indexes(*left, _tables)?;
            let right = select_indexes(*right, _tables)?;
            Ok(LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
                join_type,
                algorithm,
            })
        }
        LogicalPlan::Filter { input, predicate } => {
            let input = select_indexes(*input, _tables)?;
            Ok(LogicalPlan::Filter {
                input: Box::new(input),
                predicate,
            })
        }
        LogicalPlan::Projection { input, columns, table_map } => {
            let input = select_indexes(*input, _tables)?;
            Ok(LogicalPlan::Projection {
                input: Box::new(input),
                columns,
                table_map,
            })
        }
        LogicalPlan::Sort { input, order_by } => {
            let input = select_indexes(*input, _tables)?;
            Ok(LogicalPlan::Sort {
                input: Box::new(input),
                order_by,
            })
        }
        LogicalPlan::Limit { input, limit, offset } => {
            let input = select_indexes(*input, _tables)?;
            Ok(LogicalPlan::Limit {
                input: Box::new(input),
                limit,
                offset,
            })
        }
        other => Ok(other),
    }
}

fn choose_index_for_filter(table: &Table, filter: &Expression) -> Option<(String, Vec<Datum>)> {
    match filter {
        Expression::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            let (col_name, lit_val) = match (left.as_ref(), right.as_ref()) {
                (Expression::Column(c), Expression::Literal(v))
                | (Expression::Literal(v), Expression::Column(c)) => (c.clone(), v.clone()),
                _ => return None,
            };
            let col_idx = table.columns.iter().position(|c| c.name.eq_ignore_ascii_case(&col_name))?;
            // Coerce the literal to the column's data type to ensure index lookup matches stored values
            let coerced = coerce_datum(&lit_val, &table.columns[col_idx].data_type).ok()?;
            for idx in &table.indexes {
                if idx.meta.columns.len() == 1 && idx.meta.columns[0] == col_idx {
                    return Some((idx.meta.name.clone(), vec![coerced]));
                }
            }
            None
        }
        _ => None,
    }
}

fn remove_indexed_predicate(
    filter: Option<&Expression>,
    idx_name: &str,
    columns: &[ColumnMeta],
) -> Option<Expression> {
    let filter = filter?;
    let conjuncts = decompose_conjunction(filter);
    // Remove any equality predicate that matches the index's column
    let remaining: Vec<Expression> = conjuncts
        .into_iter()
        .filter(|pred| !matches_indexed_predicate(pred, idx_name, columns))
        .collect();
    if remaining.is_empty() {
        None
    } else {
        Some(recompose_conjunction(remaining))
    }
}

/// Check if a predicate is an equality on an indexed column.
fn matches_indexed_predicate(pred: &Expression, _idx_name: &str, columns: &[ColumnMeta]) -> bool {
    match pred {
        Expression::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            let col_name = match (left.as_ref(), right.as_ref()) {
                (Expression::Column(c), Expression::Literal(_))
                | (Expression::Literal(_), Expression::Column(c)) => c.clone(),
                _ => return false,
            };
            columns.iter().any(|c| c.name.eq_ignore_ascii_case(&col_name))
        }
        _ => false,
    }
}

// ── Join Algorithm Selection ─────────────────────────────

fn choose_join_algorithms(plan: LogicalPlan, _tables: &[Table]) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::Join { left, right, join_type, algorithm } => {
            let left = choose_join_algorithms(*left, _tables)?;
            let right = choose_join_algorithms(*right, _tables)?;

            let algorithm = match &algorithm {
                JoinAlgorithm::NestedLoop { on_clause } => {
                    select_join_algorithm(
                        &left, &right, &join_type, on_clause, _tables,
                    )
                }
                other => other.clone(),
            };

            Ok(LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
                join_type,
                algorithm,
            })
        }
        LogicalPlan::Filter { input, predicate } => {
            let input = choose_join_algorithms(*input, _tables)?;
            Ok(LogicalPlan::Filter {
                input: Box::new(input),
                predicate,
            })
        }
        LogicalPlan::Projection { input, columns, table_map } => {
            let input = choose_join_algorithms(*input, _tables)?;
            Ok(LogicalPlan::Projection {
                input: Box::new(input),
                columns,
                table_map,
            })
        }
        LogicalPlan::Sort { input, order_by } => {
            let input = choose_join_algorithms(*input, _tables)?;
            Ok(LogicalPlan::Sort {
                input: Box::new(input),
                order_by,
            })
        }
        LogicalPlan::Limit { input, limit, offset } => {
            let input = choose_join_algorithms(*input, _tables)?;
            Ok(LogicalPlan::Limit {
                input: Box::new(input),
                limit,
                offset,
            })
        }
        other => Ok(other),
    }
}

fn select_join_algorithm(
    left: &LogicalPlan,
    right: &LogicalPlan,
    join_type: &JoinType,
    on_clause: &Option<Expression>,
    tables: &[Table],
) -> JoinAlgorithm {
    let on = match on_clause {
        Some(e) => e,
        None => {
            return JoinAlgorithm::NestedLoop { on_clause: None };
        }
    };

    // Extract equi-join conditions
    let equi_pairs = extract_equi_join_pairs(on);

    // Try Index Nested-Loop Join: check if right table has index on any equi-join key
    if let Some((right_tname, right_cols)) = extract_single_table(right) {
        if let Some(rt) = tables.iter().find(|t| t.name == right_tname) {
            for (left_expr, right_expr) in &equi_pairs {
                // Check if the right side of the equi-join is a column in this table with an index
                let (right_col_name, outer_expr) = match (left_expr, right_expr) {
                    (Expression::Column(c), e) if rt.columns.iter().any(|col| col.name.eq_ignore_ascii_case(c)) => (c.clone(), e.clone()),
                    (e, Expression::Column(c)) if rt.columns.iter().any(|col| col.name.eq_ignore_ascii_case(c)) => (c.clone(), e.clone()),
                    _ => continue,
                };
                if let Some(ci) = rt.columns.iter().position(|col| col.name.eq_ignore_ascii_case(&right_col_name)) {
                    for idx in &rt.indexes {
                        if idx.meta.columns.len() == 1 && idx.meta.columns[0] == ci {
                            if let Some(oi) = extract_column_indices(&outer_expr, left) {
                                let remaining = remove_equi_from_on(on, &equi_pairs, &right_col_name);
                                return JoinAlgorithm::IndexNestedLoop {
                                    inner_table: right_tname.clone(),
                                    inner_columns: right_cols.clone(),
                                    index_name: idx.meta.name.clone(),
                                    outer_key_indices: oi,
                                    remaining_on: remaining,
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    // Try Hash Join for equi-joins (non-indexed)
    if !equi_pairs.is_empty() {
        let left_cols = count_output_columns(left);
        let right_cols = count_output_columns(right);
        let (left_keys, right_keys): (Vec<_>, Vec<_>) = equi_pairs
            .iter()
            .filter_map(|(l, r)| {
                let li = extract_column_indices(l, left);
                let ri = extract_column_indices(r, right);
                match (li, ri) {
                    (Some(lv), Some(rv)) if lv.len() == 1 && rv.len() == 1 => {
                        Some((lv[0], rv[0]))
                    }
                    _ => None,
                }
            })
            .unzip();

        if !left_keys.is_empty() && !right_keys.is_empty() {
            // Build from smaller side
            let build_left = left_cols <= right_cols;
            let remaining = remove_equi_pairs_from_on(on, &equi_pairs);
            return JoinAlgorithm::HashJoin {
                build_left,
                build_key_indices: if build_left { left_keys.clone() } else { right_keys.clone() },
                probe_key_indices: if build_left { right_keys.clone() } else { left_keys.clone() },
                remaining_on: remaining,
            };
        }
    }

    // Fallback: Nested Loop
    JoinAlgorithm::NestedLoop {
        on_clause: on_clause.clone(),
    }
}

fn extract_equi_join_pairs(expr: &Expression) -> Vec<(Expression, Expression)> {
    match expr {
        Expression::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            vec![(left.as_ref().clone(), right.as_ref().clone())]
        }
        Expression::BinaryOp { left, op: BinaryOperator::And, right } => {
            let mut l = extract_equi_join_pairs(left);
            let r = extract_equi_join_pairs(right);
            l.extend(r);
            l
        }
        _ => vec![],
    }
}

fn extract_single_table(plan: &LogicalPlan) -> Option<(String, Vec<ColumnMeta>)> {
    match plan {
        LogicalPlan::TableScan { table, table_columns, .. } => {
            Some((table.clone(), table_columns.clone()))
        }
        _ => None,
    }
}

fn extract_column_indices(expr: &Expression, plan: &LogicalPlan) -> Option<Vec<usize>> {
    match (expr, plan) {
        (Expression::Column(c), LogicalPlan::TableScan { table_columns, .. }) => {
            let idx = table_columns.iter().position(|col| col.name.eq_ignore_ascii_case(c))?;
            Some(vec![idx])
        }
        _ => None,
    }
}

fn count_output_columns(plan: &LogicalPlan) -> usize {
    match plan {
        LogicalPlan::TableScan { table_columns, .. } => table_columns.len(),
        LogicalPlan::Join { left, right, .. } => {
            count_output_columns(left) + count_output_columns(right)
        }
        LogicalPlan::Filter { input, .. } => count_output_columns(input),
        LogicalPlan::Projection { columns, .. } => columns.len(),
        LogicalPlan::Sort { input, .. } => count_output_columns(input),
        LogicalPlan::Limit { input, .. } => count_output_columns(input),
        _ => 0,
    }
}

fn remove_equi_from_on(
    on: &Expression,
    equi_pairs: &[(Expression, Expression)],
    _col_name: &str,
) -> Option<Expression> {
    let remaining = remove_equi_pairs_from_on(on, equi_pairs);
    remaining
}

fn remove_equi_pairs_from_on(
    expr: &Expression,
    _pairs: &[(Expression, Expression)],
) -> Option<Expression> {
    // Conservative: keep the full ON clause for the remaining predicate.
    // A production implementation would remove matched equi-join conditions.
    Some(expr.clone())
}

// ── Column Resolution ────────────────────────────────────

fn resolve_table_list<'a>(
    table_name: &str,
    joins: &[JoinClause],
    tables: &'a [Table],
) -> Result<Vec<&'a Table>> {
    let mut result = Vec::new();
    result.push(find_table(tables, table_name)?);
    for jc in joins {
        result.push(find_table(tables, &jc.right_table)?);
    }
    Ok(result)
}

fn resolve_select_columns(
    columns: &[SelectColumn],
    table_map: &[(String, Vec<ColumnMeta>)],
    combined_len: usize,
) -> Result<Vec<usize>> {
    if columns.is_empty() {
        return Ok((0..combined_len).collect());
    }

    // Compute per-table column offsets
    let mut offsets: Vec<(String, usize)> = Vec::new();
    let mut off = 0;
    for (tname, cols) in table_map {
        offsets.push((tname.clone(), off));
        off += cols.len();
    }

    let mut result = Vec::new();
    let mut wildcard = false;

    for col in columns {
        match col {
            SelectColumn::Wildcard => {
                wildcard = true;
            }
            SelectColumn::QualWildcard(table) => {
                // Expand to all columns of that table
                for (tname, offset) in &offsets {
                    if tname.eq_ignore_ascii_case(table) {
                        if let Some((_, cols)) = table_map.iter().find(|(n, _)| n == tname) {
                            for i in 0..cols.len() {
                                result.push(offset + i);
                            }
                        }
                        break;
                    }
                }
            }
            SelectColumn::Qualified { name, .. } => {
                // Try to parse as table.column
                if let Some(dot) = name.find('.') {
                    let t = &name[..dot];
                    let c = &name[dot + 1..];
                    if let Some((toff, tcols)) = resolve_qualified_column(t, c, table_map)? {
                        result.push(toff + tcols);
                    }
                } else {
                    // Bare name: find in all tables
                    resolve_unqualified_column(name, table_map, &mut result)?;
                }
            }
            SelectColumn::Expr(Expression::Column(name)) => {
                resolve_unqualified_column(name, table_map, &mut result)?;
            }
            SelectColumn::Expr(Expression::QualifiedColumn(t, c)) => {
                if let Some((toff, tcols)) = resolve_qualified_column(t, c, table_map)? {
                    result.push(toff + tcols);
                }
            }
            SelectColumn::Expr(_) => {
                // Expression column; handled at execution time
                wildcard = true;
            }
        }
    }

    if wildcard && result.is_empty() {
        result = (0..combined_len).collect();
    }

    Ok(result)
}

fn resolve_qualified_column(
    table: &str,
    col: &str,
    table_map: &[(String, Vec<ColumnMeta>)],
) -> Result<Option<(usize, usize)>> {
    for (tname, offset, cols) in table_map.iter().flat_map(|(n, cols)| {
        let off = table_map.iter().take_while(|(tn, _)| tn != n).map(|(_, c)| c.len()).sum::<usize>();
        std::iter::once((n.as_str(), off, cols))
    }) {
        if tname.eq_ignore_ascii_case(table) {
            if let Some(ci) = cols.iter().position(|c| c.name.eq_ignore_ascii_case(col)) {
                return Ok(Some((offset, ci)));
            }
            return Err(HelionError::ColumnNotFound(format!("{}.{}", table, col)));
        }
    }
    // Table not found in join; might be single-table with qualified reference
    if table_map.len() == 1 {
        let (_, cols) = &table_map[0];
        if let Some(ci) = cols.iter().position(|c| c.name.eq_ignore_ascii_case(col)) {
            return Ok(Some((0, ci)));
        }
        return Err(HelionError::ColumnNotFound(format!("{}.{}", table, col)));
    }
    Err(HelionError::ColumnNotFound(format!("{}.{}", table, col)))
}

fn resolve_unqualified_column(
    name: &str,
    table_map: &[(String, Vec<ColumnMeta>)],
    result: &mut Vec<usize>,
) -> Result<()> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0;
    for (_, cols) in table_map {
        if let Some(ci) = cols.iter().position(|c| c.name.eq_ignore_ascii_case(name)) {
            found.push((offset, ci));
        }
        offset += cols.len();
    }

    if found.is_empty() {
        return Err(HelionError::ColumnNotFound(name.to_string()));
    }
    if found.len() > 1 {
        return Err(HelionError::AmbiguousColumn(name.to_string()));
    }

    result.push(found[0].0 + found[0].1);
    Ok(())
}

fn find_table<'a>(tables: &'a [Table], name: &str) -> Result<&'a Table> {
    tables
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| HelionError::TableNotFound(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::parse;
    use crate::storage::types::*;

    fn make_tables() -> Vec<Table> {
        let columns = vec![
            ColumnMeta::new("id", DataType::Integer).primary_key(),
            ColumnMeta::new("name", DataType::Text),
            ColumnMeta::new("age", DataType::Integer),
        ];
        vec![Table::new("users", columns)]
    }

    fn make_two_tables() -> Vec<Table> {
        vec![
            Table::new("orders", vec![
                ColumnMeta::new("id", DataType::Integer).primary_key(),
                ColumnMeta::new("user_id", DataType::Integer),
                ColumnMeta::new("total", DataType::Double),
            ]),
            Table::new("users", vec![
                ColumnMeta::new("id", DataType::Integer).primary_key(),
                ColumnMeta::new("name", DataType::Text),
            ]),
        ]
    }

    #[test]
    fn test_plan_create_table() {
        let stmts = parse("CREATE TABLE t (id INTEGER)").unwrap();
        let plan = plan(&stmts[0], &[]).unwrap();
        assert!(matches!(plan, LogicalPlan::CreateTable { .. }));
    }

    #[test]
    fn test_plan_drop_table() {
        let stmts = parse("DROP TABLE users").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        assert!(matches!(plan, LogicalPlan::DropTable { .. }));
    }

    #[test]
    fn test_plan_insert() {
        let stmts = parse("INSERT INTO users VALUES (1, 'Alice', 30)").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        match plan {
            LogicalPlan::Insert { rows, .. } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].values.len(), 3);
            }
            _ => panic!("Expected Insert"),
        }
    }

    #[test]
    fn test_plan_select() {
        let stmts = parse("SELECT id, name FROM users WHERE age > 18").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        // Should produce a plan tree with Projection -> Filter -> TableScan
        match plan {
            LogicalPlan::Projection { columns, .. } => {
                assert_eq!(columns.len(), 2);
            }
            _ => panic!("Expected Projection tree, got: {:?}", plan),
        }
    }

    #[test]
    fn test_plan_select_wildcard() {
        let stmts = parse("SELECT * FROM users").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        match plan {
            LogicalPlan::Projection { columns, .. } => {
                assert_eq!(columns.len(), 3);
            }
            _ => panic!("Expected Projection tree, got: {:?}", plan),
        }
    }

    #[test]
    fn test_plan_update() {
        let stmts = parse("UPDATE users SET age = 31 WHERE id = 1").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        match plan {
            LogicalPlan::Update {
                set_indices,
                set_values,
                ..
            } => {
                assert_eq!(set_indices, vec![2]);
                assert_eq!(set_values[0], Datum::Integer(31));
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_plan_delete() {
        let stmts = parse("DELETE FROM users WHERE id = 1").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        assert!(matches!(plan, LogicalPlan::Delete { .. }));
    }

    #[test]
    fn test_plan_create_user() {
        let stmts = parse("CREATE USER alice WITH PASSWORD 'secret'").unwrap();
        let plan = plan(&stmts[0], &[]).unwrap();
        match plan {
            LogicalPlan::CreateUser { username, .. } => assert_eq!(username, "alice"),
            _ => panic!("Expected CreateUser"),
        }
    }

    #[test]
    fn test_plan_drop_user() {
        let stmts = parse("DROP USER alice").unwrap();
        let plan = plan(&stmts[0], &[]).unwrap();
        assert!(matches!(plan, LogicalPlan::DropUser { .. }));
    }

    #[test]
    fn test_plan_grant() {
        let stmts = parse("GRANT SELECT ON users TO alice").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        match plan {
            LogicalPlan::Grant {
                username,
                table,
                permission,
                ..
            } => {
                assert_eq!(username, "alice");
                assert_eq!(table, "users");
                assert_eq!(permission, Permission::Select(vec![]));
            }
            _ => panic!("Expected Grant"),
        }
    }

    #[test]
    fn test_plan_revoke() {
        let stmts = parse("REVOKE ALL ON users FROM alice").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        assert!(matches!(plan, LogicalPlan::Revoke { .. }));
    }

    #[test]
    fn test_plan_table_not_found() {
        let stmts = parse("SELECT * FROM nonexistent").unwrap();
        let result = plan(&stmts[0], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_plan_column_not_found() {
        let stmts = parse("INSERT INTO users (bad_col) VALUES (1)").unwrap();
        let result = plan(&stmts[0], &make_tables());
        assert!(result.is_err());
    }

    #[test]
    fn test_plan_join() {
        let tables = make_two_tables();
        let stmts = parse("SELECT * FROM orders JOIN users ON orders.user_id = users.id").unwrap();
        let plan = plan(&stmts[0], &tables).unwrap();
        // Should produce a tree with Projection -> Join -> (TableScan, TableScan)
        match plan {
            LogicalPlan::Projection { columns, .. } => {
                assert_eq!(columns.len(), 5); // 3 + 2 columns
            }
            _ => panic!("Expected Projection tree for join, got: {:?}", plan),
        }
    }

    #[test]
    fn test_plan_join_select_columns() {
        let tables = make_two_tables();
        let stmts = parse("SELECT orders.id, users.name FROM orders JOIN users ON orders.user_id = users.id").unwrap();
        let plan = plan(&stmts[0], &tables).unwrap();
        match plan {
            LogicalPlan::Projection { columns, .. } => {
                assert_eq!(columns.len(), 2);
            }
            _ => panic!("Expected Projection tree"),
        }
    }
}
