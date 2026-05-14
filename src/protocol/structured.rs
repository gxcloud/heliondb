use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{HelionError, Result};
use crate::executor::ops::{execute_as, QueryResult};
use crate::sql::parser::*;
use crate::sql::planner::{JoinAlgorithm, LogicalPlan};
use crate::storage::engine::DatabaseEngine;
use crate::storage::types::{ColumnMeta, DataType, Datum, Row};

// ═══════════════════════════════════════════════════════════
// Structured Query Types (JSON-serializable)
// ═══════════════════════════════════════════════════════════

/// Top-level structured query — tagged by `"op"` in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum StructuredQuery {
    #[serde(rename = "findMany")]
    FindMany(FindManyInput),
    #[serde(rename = "findUnique")]
    FindUnique(FindUniqueInput),
    #[serde(rename = "create")]
    Create(CreateInput),
    #[serde(rename = "update")]
    Update(UpdateInput),
    #[serde(rename = "delete")]
    Delete(DeleteInput),
    #[serde(rename = "upsert")]
    Upsert(UpsertInput),
}

// ── SELECT variants ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindManyInput {
    pub from: String,
    #[serde(default, rename = "where")]
    pub where_clause: Option<serde_json::Value>,
    #[serde(default)]
    pub select: Option<Vec<String>>,
    #[serde(default)]
    pub include: Vec<IncludeInput>,
    #[serde(default, rename = "orderBy")]
    pub order_by: Vec<OrderByInput>,
    pub take: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindUniqueInput {
    pub from: String,
    #[serde(rename = "where")]
    pub where_clause: serde_json::Value,
    #[serde(default)]
    pub select: Option<Vec<String>>,
    #[serde(default)]
    pub include: Vec<IncludeInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeInput {
    pub relation: String,
    #[serde(default, rename = "where")]
    pub where_clause: Option<serde_json::Value>,
    #[serde(default)]
    pub select: Option<Vec<String>>,
    #[serde(default, rename = "orderBy")]
    pub order_by: Vec<OrderByInput>,
    pub take: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderByInput {
    pub field: String,
    #[serde(default)]
    pub direction: String,
}

// ── Mutation variants ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInput {
    pub from: String,
    #[serde(default)]
    pub data: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInput {
    pub from: String,
    #[serde(rename = "where")]
    pub where_clause: serde_json::Value,
    pub data: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteInput {
    pub from: String,
    #[serde(rename = "where")]
    pub where_clause: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertInput {
    pub from: String,
    #[serde(rename = "where")]
    pub where_clause: serde_json::Value,
    pub create: HashMap<String, serde_json::Value>,
    pub update: HashMap<String, serde_json::Value>,
}

// ═══════════════════════════════════════════════════════════
// Execution
// ═══════════════════════════════════════════════════════════

/// Execute a structured query against the engine and return a JSON result string.
pub async fn execute_structured(
    engine: &DatabaseEngine,
    query: &StructuredQuery,
    current_user: Option<&str>,
) -> Result<String> {
    match query {
        StructuredQuery::FindMany(input) => execute_find_many(engine, input, current_user).await,
        StructuredQuery::FindUnique(input) => execute_find_unique(engine, input, current_user).await,
        StructuredQuery::Create(input) => execute_create(engine, input, current_user).await,
        StructuredQuery::Update(input) => execute_update(engine, input, current_user).await,
        StructuredQuery::Delete(input) => execute_delete(engine, input, current_user).await,
        StructuredQuery::Upsert(input) => execute_upsert(engine, input, current_user).await,
    }
}

// ── FindMany ─────────────────────────────────────────────

async fn execute_find_many(
    engine: &DatabaseEngine,
    input: &FindManyInput,
    current_user: Option<&str>,
) -> Result<String> {
    let tables = engine.get_tables().await;
    let plan = build_find_many_plan(input, &tables)?;
    let result = execute_as(engine, &plan, current_user).await?;

    // Build nested JSON response from flat rows
    let data = if input.include.is_empty() {
        // No includes: flat rows → array of objects
        rows_to_json(&result.columns, &result.rows)
    } else {
        // With includes: group rows by parent primary key
        let fk_info = resolve_fk_for_includes(input, &tables);
        build_nested_json(&result, &fk_info)
    };

    let response = serde_json::json!({ "data": data });
    Ok(response.to_string())
}

async fn execute_find_unique(
    engine: &DatabaseEngine,
    input: &FindUniqueInput,
    current_user: Option<&str>,
) -> Result<String> {
    let tables = engine.get_tables().await;
    let plan = build_find_unique_plan(input, &tables)?;
    let result = execute_as(engine, &plan, current_user).await?;

    let data = if result.rows.is_empty() {
        serde_json::Value::Null
    } else if input.include.is_empty() {
        row_to_json(&result.columns, &result.rows[0])
    } else {
        let fk_info = resolve_fk_for_includes_find_unique(input, &tables);
        build_nested_json(&result, &fk_info)
            .into_iter()
            .next()
            .unwrap_or(serde_json::Value::Null)
    };

    let response = serde_json::json!({ "data": data });
    Ok(response.to_string())
}

// ── Create ───────────────────────────────────────────────

async fn execute_create(
    engine: &DatabaseEngine,
    input: &CreateInput,
    current_user: Option<&str>,
) -> Result<String> {
    let tables = engine.get_tables().await;
    let table = find_table_meta(&tables, &input.from)?;

    let mut values = vec![Datum::Null; table.columns.len()];
    for (col_name, json_val) in &input.data {
        let idx = table.column_index(col_name).ok_or_else(|| {
            HelionError::ColumnNotFound(format!("{}.{}", input.from, col_name))
        })?;
        values[idx] = json_value_to_datum(json_val, &table.columns[idx].data_type);
    }

    let plan = LogicalPlan::Insert {
        table_name: input.from.clone(),
        rows: vec![Row::new(values)],
    };
    let result = execute_as(engine, &plan, current_user).await?;

    // Build response with inserted data
    let inserted = if result.rows_affected > 0 {
        // Re-fetch the last inserted row (approximate)
        let re_fetch = execute_as(
            engine,
            &LogicalPlan::Projection {
                input: Box::new(LogicalPlan::TableScan {
                    table: input.from.clone(),
                    table_columns: table.columns.clone(),
                    filter: None,
                    index_name: None,
                    index_keys: None,
                }),
                columns: (0..table.columns.len()).collect(),
                table_map: vec![(input.from.clone(), table.columns.clone())],
            },
            current_user,
        )
        .await?;
        re_fetch.rows.last().map(|r| row_to_json(&re_fetch.columns, r)).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };

    let response = serde_json::json!({ "data": inserted });
    Ok(response.to_string())
}

// ── Update ───────────────────────────────────────────────

async fn execute_update(
    engine: &DatabaseEngine,
    input: &UpdateInput,
    current_user: Option<&str>,
) -> Result<String> {
    let tables = engine.get_tables().await;
    let table = find_table_meta(&tables, &input.from)?;
    let where_expr = parse_where_clause(&input.where_clause)?;

    // Build SET values
    let mut set_indices = Vec::new();
    let mut set_values = Vec::new();
    for (col_name, json_val) in &input.data {
        let idx = table.column_index(col_name).ok_or_else(|| {
            HelionError::ColumnNotFound(format!("{}.{}", input.from, col_name))
        })?;
        set_indices.push(idx);
        set_values.push(json_value_to_datum(json_val, &table.columns[idx].data_type));
    }

    let plan = LogicalPlan::Update {
        table_name: input.from.clone(),
        set_indices,
        set_values,
        where_clause: Some(where_expr),
        table_columns: table.columns.clone(),
    };
    let result = execute_as(engine, &plan, current_user).await?;

    let response = serde_json::json!({ "data": { "rows_affected": result.rows_affected } });
    Ok(response.to_string())
}

// ── Delete ───────────────────────────────────────────────

async fn execute_delete(
    engine: &DatabaseEngine,
    input: &DeleteInput,
    current_user: Option<&str>,
) -> Result<String> {
    let tables = engine.get_tables().await;
    let table = find_table_meta(&tables, &input.from)?;
    let where_expr = parse_where_clause(&input.where_clause)?;

    let plan = LogicalPlan::Delete {
        table_name: input.from.clone(),
        where_clause: Some(where_expr),
        table_columns: table.columns.clone(),
    };
    let result = execute_as(engine, &plan, current_user).await?;

    let response = serde_json::json!({ "data": { "rows_affected": result.rows_affected } });
    Ok(response.to_string())
}

// ── Upsert ───────────────────────────────────────────────

async fn execute_upsert(
    engine: &DatabaseEngine,
    input: &UpsertInput,
    current_user: Option<&str>,
) -> Result<String> {
    // Try UPDATE first
    let update_query = UpdateInput {
        from: input.from.clone(),
        where_clause: input.where_clause.clone(),
        data: input.update.clone(),
    };
    let update_result = execute_update(engine, &update_query, current_user).await?;
    let update_json: serde_json::Value = serde_json::from_str(&update_result)
        .map_err(|e| HelionError::Internal(format!("JSON parse: {}", e)))?;
    let rows_affected = update_json["data"]["rows_affected"].as_u64().unwrap_or(0);

    if rows_affected == 0 {
        // No matching row → INSERT
        let create_query = CreateInput {
            from: input.from.clone(),
            data: input.create.clone(),
        };
        execute_create(engine, &create_query, current_user).await
    } else {
        Ok(update_result)
    }
}

// ═══════════════════════════════════════════════════════════
// Plan Building
// ═══════════════════════════════════════════════════════════

fn build_find_many_plan(input: &FindManyInput, tables: &[crate::storage::table::Table]) -> Result<LogicalPlan> {
    let table = find_table_meta(tables, &input.from)?;

    // 1. Base TableScan
    let mut plan: LogicalPlan = LogicalPlan::TableScan {
        table: input.from.clone(),
        table_columns: table.columns.clone(),
        filter: None,
        index_name: None,
        index_keys: None,
    };

    // 2. Apply WHERE
    let where_filter = parse_where_clause_opt(&input.where_clause)?;
    if let Some(wc) = where_filter {
        plan = LogicalPlan::Filter {
            input: Box::new(plan),
            predicate: wc,
        };
    }

    // 3. Resolve includes → JOINs
    let mut all_columns = table.columns.clone();
    let mut table_map = vec![(input.from.clone(), table.columns.clone())];
    let mut join_column_count = table.columns.len();

    for inc in &input.include {
        if let Some((right_table, fk)) = find_relationship(tables, &input.from, &inc.relation) {
            // Build JOIN plan
            let right_scan = LogicalPlan::TableScan {
                table: inc.relation.clone(),
                table_columns: right_table.columns.clone(),
                filter: None,
                index_name: None,
                index_keys: None,
            };
            // Apply include where on the right table
            let right_input = if let Some(ref inc_where) = inc.where_clause {
                let wc = parse_where_clause(inc_where)?;
                LogicalPlan::Filter {
                    input: Box::new(right_scan),
                    predicate: wc,
                }
            } else {
                right_scan
            };

            // Build ON clause from FK
            let on_expr = Expression::BinaryOp {
                left: Box::new(Expression::QualifiedColumn(input.from.clone(), fk.parent_column.clone())),
                op: BinaryOperator::Eq,
                right: Box::new(Expression::QualifiedColumn(inc.relation.clone(), fk.child_column.clone())),
            };

            plan = LogicalPlan::Join {
                left: Box::new(plan),
                right: Box::new(right_input),
                join_type: JoinType::Left,
                algorithm: JoinAlgorithm::NestedLoop { on_clause: Some(on_expr) },
            };

            all_columns.extend(right_table.columns.iter().cloned());
            table_map.push((inc.relation.clone(), right_table.columns.clone()));
            join_column_count += right_table.columns.len();
        }
    }

    // 4. ORDER BY
    if !input.order_by.is_empty() {
        let order_by_exprs: Vec<OrderByExpr> = input.order_by.iter().map(|o| OrderByExpr {
            expr: Expression::Column(o.field.clone()),
            direction: if o.direction.eq_ignore_ascii_case("desc") { OrderByDesc::Desc } else { OrderByDesc::Asc },
        }).collect();
        plan = LogicalPlan::Sort {
            input: Box::new(plan),
            order_by: order_by_exprs,
        };
    }

    // 5. LIMIT / OFFSET
    if input.take.is_some() || input.skip.unwrap_or(0) > 0 {
        plan = LogicalPlan::Limit {
            input: Box::new(plan),
            limit: input.take.unwrap_or(u64::MAX),
            offset: input.skip.unwrap_or(0),
        };
    }

    // 6. Projection (SELECT columns)
    let proj_indices: Vec<usize> = if let Some(ref select_cols) = input.select {
        select_cols.iter().filter_map(|sc| {
            all_columns.iter().position(|c| c.name.eq_ignore_ascii_case(sc))
        }).collect()
    } else {
        (0..join_column_count).collect()
    };

    plan = LogicalPlan::Projection {
        input: Box::new(plan),
        columns: proj_indices,
        table_map: table_map.clone(),
    };

    Ok(plan)
}

fn build_find_unique_plan(input: &FindUniqueInput, tables: &[crate::storage::table::Table]) -> Result<LogicalPlan> {
    let filter_expr = parse_where_clause(&input.where_clause)?;
    let find_many = FindManyInput {
        from: input.from.clone(),
        where_clause: Some(input.where_clause.clone()),
        select: input.select.clone(),
        include: input.include.clone(),
        order_by: vec![],
        take: Some(1),
        skip: None,
    };
    build_find_many_plan(&find_many, tables)
}

// ═══════════════════════════════════════════════════════════
// WHERE Clause Parsing (Prisma-style JSON → Expression)
// ═══════════════════════════════════════════════════════════

pub fn parse_where_clause(value: &serde_json::Value) -> Result<Expression> {
    match parse_where_opt(value) {
        Ok(Some(expr)) => Ok(expr),
        Ok(None) => Err(HelionError::Parse("Empty WHERE clause".into())),
        Err(e) => Err(e),
    }
}

pub fn parse_where_clause_opt(value: &Option<serde_json::Value>) -> Result<Option<Expression>> {
    match value {
        Some(v) => parse_where_opt(v),
        None => Ok(None),
    }
}

fn parse_where_opt(value: &serde_json::Value) -> Result<Option<Expression>> {
    match value {
        Value::Null => Ok(None),
        Value::Object(map) => {
            // Check logical operators
            if let Some(conds) = map.get("AND").and_then(|v| v.as_array()) {
                let exprs: Vec<Expression> = conds.iter()
                    .filter_map(|c| parse_where_opt(c).transpose())
                    .collect::<Result<Vec<_>>>()?;
                return Ok(combine_and(exprs));
            }
            if let Some(conds) = map.get("OR").and_then(|v| v.as_array()) {
                let exprs: Vec<Expression> = conds.iter()
                    .filter_map(|c| parse_where_opt(c).transpose())
                    .collect::<Result<Vec<_>>>()?;
                return Ok(combine_or(exprs));
            }
            if let Some(cond) = map.get("NOT") {
                let inner = parse_where_opt(cond)?;
                return match inner {
                    Some(e) => Ok(Some(Expression::UnaryOp {
                        op: UnaryOperator::Not,
                        expr: Box::new(e),
                    })),
                    None => Ok(None),
                };
            }
            // Otherwise: { "field": condition }
            if let Some((field, condition)) = map.iter().next() {
                return parse_field_condition(field, condition);
            }
            Ok(None)
        }
        _ => Err(HelionError::Parse("Invalid WHERE clause: expected object".into())),
    }
}

fn parse_field_condition(field: &str, condition: &serde_json::Value) -> Result<Option<Expression>> {
    let col_expr = || Expression::Column(field.to_string());
    match condition {
        Value::Null => Ok(Some(Expression::IsNull(Box::new(col_expr())))),
        Value::Bool(b) => Ok(Some(binary_op(col_expr(), BinaryOperator::Eq, Datum::Boolean(*b)))),
        Value::Number(n) => {
            let datum = if n.is_f64() {
                Datum::Double(n.as_f64().unwrap())
            } else {
                Datum::BigInt(n.as_i64().unwrap_or(0))
            };
            Ok(Some(binary_op(col_expr(), BinaryOperator::Eq, datum)))
        }
        Value::String(s) => Ok(Some(binary_op(col_expr(), BinaryOperator::Eq, Datum::Text(s.clone())))),
        Value::Object(ops) => {
            let mut parts: Vec<Expression> = Vec::new();
            for (op, val) in ops {
                let expr = parse_operator_condition(field, op, val)?;
                parts.push(expr);
            }
            Ok(combine_and(parts))
        }
        Value::Array(items) => {
            // Treat array as IN list
            let values: Vec<Datum> = items.iter().map(json_to_datum).collect();
            Ok(Some(Expression::In {
                expr: Box::new(col_expr()),
                list: values,
            }))
        }
    }
}

fn parse_operator_condition(field: &str, op: &str, val: &serde_json::Value) -> Result<Expression> {
    let col = || Expression::Column(field.to_string());
    let datum = json_to_datum(val);
    match op {
        "eq" => Ok(binary_op(col(), BinaryOperator::Eq, datum)),
        "ne" | "not" => Ok(binary_op(col(), BinaryOperator::Ne, datum)),
        "gt" => Ok(binary_op(col(), BinaryOperator::Gt, datum)),
        "gte" => Ok(binary_op(col(), BinaryOperator::Ge, datum)),
        "lt" => Ok(binary_op(col(), BinaryOperator::Lt, datum)),
        "lte" => Ok(binary_op(col(), BinaryOperator::Le, datum)),
        "contains" => Ok(Expression::Like {
            expr: Box::new(col()),
            pattern: format!("%{}%", string_from_value(val)),
        }),
        "startsWith" => Ok(Expression::Like {
            expr: Box::new(col()),
            pattern: format!("{}%", string_from_value(val)),
        }),
        "endsWith" => Ok(Expression::Like {
            expr: Box::new(col()),
            pattern: format!("%{}", string_from_value(val)),
        }),
        "in" => {
            let items = val.as_array().ok_or_else(|| HelionError::Parse("'in' requires an array".into()))?;
            let values: Vec<Datum> = items.iter().map(json_to_datum).collect();
            Ok(Expression::In { expr: Box::new(col()), list: values })
        }
        _ => Err(HelionError::Parse(format!("Unknown operator: {}", op))),
    }
}

// ═══════════════════════════════════════════════════════════
// Relationship resolution
// ═══════════════════════════════════════════════════════════

pub struct FkRelationship {
    pub child_column: String,
    pub parent_column: String,
}

/// Find a FK relationship between two tables by looking at column metadata.
pub fn find_relationship<'a>(
    tables: &'a [crate::storage::table::Table],
    parent_table: &str,
    child_table: &str,
) -> Option<(&'a crate::storage::table::Table, FkRelationship)> {
    let child = tables.iter().find(|t| t.name == child_table)?;
    // Look for a column in child_table that references parent_table
    for col in &child.columns {
        if let Some(ref fk) = col.references {
            if fk.foreign_table == parent_table {
                let parent = tables.iter().find(|t| t.name == parent_table)?;
                return Some((parent, FkRelationship {
                    child_column: col.name.clone(),
                    parent_column: fk.foreign_column.clone(),
                }));
            }
        }
    }
    // Fallback: convention-based lookup (child_table has a column named parent_table + "_id")
    let convention_col = format!("{}_id", parent_table.trim_end_matches('s'));
    let col = child.columns.iter().find(|c| c.name == convention_col)?;
    // Assume it references parent.id
    let parent = tables.iter().find(|t| t.name == parent_table)?;
    Some((parent, FkRelationship {
        child_column: col.name.clone(),
        parent_column: "id".to_string(),
    }))
}

pub struct FkInfo {
    pub parent_idx: usize,       // index into table_map for parent
    pub child_idx: usize,        // index into table_map for child
    pub parent_col: String,
    pub child_col: String,
    pub parent_table: String,
    pub child_table: String,
}

fn resolve_fk_for_includes(input: &FindManyInput, tables: &[crate::storage::table::Table]) -> Vec<FkInfo> {
    let mut result = Vec::new();
    // Parent offset is always 0 (primary table)
    let mut offset = 0;
    let parent = tables.iter().find(|t| t.name == input.from);
    if parent.is_none() { return result; }
    let parent = parent.unwrap();
    offset += parent.columns.len();

    for inc in &input.include {
        if let Some((_, fk)) = find_relationship(tables, &input.from, &inc.relation) {
            if let Some(child_table) = tables.iter().find(|t| t.name == inc.relation) {
                result.push(FkInfo {
                    parent_idx: 0,
                    child_idx: offset,
                    parent_col: fk.parent_column.clone(),
                    child_col: fk.child_column.clone(),
                    parent_table: input.from.clone(),
                    child_table: inc.relation.clone(),
                });
                offset += child_table.columns.len();
            }
        }
    }
    result
}

fn resolve_fk_for_includes_find_unique(input: &FindUniqueInput, tables: &[crate::storage::table::Table]) -> Vec<FkInfo> {
    let fm = FindManyInput {
        from: input.from.clone(),
        where_clause: None,
        select: None,
        include: input.include.clone(),
        order_by: vec![],
        take: None,
        skip: None,
    };
    resolve_fk_for_includes(&fm, tables)
}

// ═══════════════════════════════════════════════════════════
// JSON Response Building
// ═══════════════════════════════════════════════════════════

fn rows_to_json(columns: &[String], rows: &[Vec<String>]) -> Vec<serde_json::Value> {
    rows.iter().map(|row| {
        let mut obj = serde_json::Map::new();
        for (i, col) in columns.iter().enumerate() {
            if let Some(val) = row.get(i) {
                obj.insert(col.clone(), serde_json::Value::String(val.clone()));
            }
        }
        serde_json::Value::Object(obj)
    }).collect()
}

fn row_to_json(columns: &[String], row: &[String]) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (i, col) in columns.iter().enumerate() {
        if let Some(val) = row.get(i) {
            obj.insert(col.clone(), serde_json::Value::String(val.clone()));
        }
    }
    serde_json::Value::Object(obj)
}

fn build_nested_json(result: &QueryResult, fk_info: &[FkInfo]) -> Vec<serde_json::Value> {
    if fk_info.is_empty() || result.columns.is_empty() {
        return rows_to_json(&result.columns, &result.rows);
    }

    let mut grouped: Vec<serde_json::Value> = Vec::new();

    for row in &result.rows {
        // Extract parent fields (before first child offset)
        let first_child_offset = fk_info.iter().map(|f| f.child_idx).min().unwrap_or(result.columns.len());
        let parent_key: Vec<String> = row.iter().take(first_child_offset).cloned().collect();

        // Find existing parent or create new one
        let parent_idx = grouped.iter().position(|g| {
            g.as_object().and_then(|o| {
                o.get("_key").and_then(|k| k.as_array())
            }).map_or(false, |k| {
                k.iter().zip(&parent_key).all(|(a, b)| a.as_str() == Some(b))
            })
        });

        if let Some(idx) = parent_idx {
            // Add child data to existing parent
            for fk in fk_info {
                let child_start = fk.child_idx;
                let child_end = fk_info.iter()
                    .filter(|f| f.child_idx > child_start)
                    .map(|f| f.child_idx)
                    .min()
                    .unwrap_or(result.columns.len());
                if child_start >= row.len() { continue; }
                let child_data: Vec<&str> = row[child_start..child_end.min(row.len())].iter().map(|s| s.as_str()).collect();
                if child_data.iter().all(|v| *v == "NULL") { continue; }

                let child_obj = &grouped[idx];
                if let Some(obj) = child_obj.as_object() {
                    let relation_key = &fk.child_table;
                    if !obj.contains_key(relation_key) {
                        if let Some(mut obj_clone) = child_obj.as_object().cloned() {
                            let arr = serde_json::Value::Array(vec![]);
                            obj_clone.insert(relation_key.clone(), arr);
                            // Can't modify in place, rebuild
                        }
                    }
                }
                // This approach is getting complex. Let's use a simpler method.
            }
        } else {
            // Create new parent entry
            let mut obj = serde_json::Map::new();
            for (i, col) in result.columns.iter().enumerate() {
                if i < first_child_offset {
                    if let Some(val) = row.get(i) {
                        obj.insert(col.clone(), serde_json::Value::String(val.clone()));
                    }
                }
            }
            // Add empty child arrays
            for fk in fk_info {
                obj.insert(fk.child_table.clone(), serde_json::Value::Array(vec![]));
            }
            obj.insert("_key".to_string(), serde_json::Value::Array(
                parent_key.iter().map(|k| serde_json::Value::String(k.clone())).collect()
            ));
            grouped.push(serde_json::Value::Object(obj));
        }
    }

    // Add child rows to their parent
    for row in &result.rows {
        let first_child_offset = fk_info.iter().map(|f| f.child_idx).min().unwrap_or(result.columns.len());
        let parent_key: Vec<String> = row.iter().take(first_child_offset).cloned().collect();

        if let Some(parent_idx) = grouped.iter().position(|g| {
            g.as_object().and_then(|o| o.get("_key"))
                .and_then(|k| k.as_array())
                .map_or(false, |k| k.iter().zip(&parent_key).all(|(a, b)| a.as_str() == Some(b)))
        }) {
            for fk in &fk_info[..1.min(fk_info.len())] {
                let child_start = fk.child_idx;
                let child_end = result.columns.len();
                if child_start >= row.len() { continue; }
                let child_vals: Vec<&str> = row[child_start..child_end.min(row.len())].iter().map(|s| s.as_str()).collect();
                if child_vals.iter().all(|v| *v == "NULL") { continue; }

                let mut child_obj = serde_json::Map::new();
                for (i, col) in result.columns.iter().enumerate().skip(child_start).take(child_end - child_start) {
                    if let Some(val) = row.get(i) {
                        child_obj.insert(col.clone(), serde_json::Value::String(val.clone()));
                    }
                }

                if let Some(parent_obj) = grouped[parent_idx].as_object() {
                    if let Some(arr) = parent_obj.get(&fk.child_table).and_then(|v| v.as_array()) {
                        // Check if this child already exists in the array
                        let exists = arr.iter().any(|c| {
                            child_obj.iter().all(|(k, v)| {
                                c.get(k).and_then(|cv| cv.as_str()) == v.as_str()
                            })
                        });
                        if !exists {
                            let mut new_arr = arr.clone();
                            new_arr.push(serde_json::Value::Object(child_obj));
                            if let Some(new_parent) = grouped[parent_idx].as_object().map(|o| {
                                let mut m = o.clone();
                                m.insert(fk.child_table.clone(), serde_json::Value::Array(new_arr));
                                m
                            }) {
                                grouped[parent_idx] = serde_json::Value::Object(new_parent);
                            }
                        }
                    }
                }
            }
        }
    }

    // Remove internal _key fields
    for item in &mut grouped {
        if let Some(obj) = item.as_object_mut() {
            obj.remove("_key");
        }
    }

    grouped
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

fn find_table_meta<'a>(tables: &'a [crate::storage::table::Table], name: &str) -> Result<&'a crate::storage::table::Table> {
    tables.iter().find(|t| t.name == name)
        .ok_or_else(|| HelionError::TableNotFound(name.to_string()))
}

fn binary_op(left: Expression, op: BinaryOperator, right: Datum) -> Expression {
    Expression::BinaryOp {
        left: Box::new(left),
        op,
        right: Box::new(Expression::Literal(right)),
    }
}

fn combine_and(exprs: Vec<Expression>) -> Option<Expression> {
    exprs.into_iter().reduce(|a, b| Expression::BinaryOp {
        left: Box::new(a),
        op: BinaryOperator::And,
        right: Box::new(b),
    })
}

fn combine_or(exprs: Vec<Expression>) -> Option<Expression> {
    exprs.into_iter().reduce(|a, b| Expression::BinaryOp {
        left: Box::new(a),
        op: BinaryOperator::Or,
        right: Box::new(b),
    })
}

/// Convert a JSON value to a Datum. For WHERE clause field values.
fn json_to_datum(val: &serde_json::Value) -> Datum {
    match val {
        Value::Null => Datum::Null,
        Value::Bool(b) => Datum::Boolean(*b),
        Value::Number(n) => {
            if n.is_f64() {
                Datum::Double(n.as_f64().unwrap())
            } else {
                Datum::BigInt(n.as_i64().unwrap_or(0))
            }
        }
        Value::String(s) => Datum::Text(s.clone()),
        _ => Datum::Null,
    }
}

/// Convert a JSON value to a Datum, with type coercion for the target column type.
fn json_value_to_datum(val: &serde_json::Value, target_type: &DataType) -> Datum {
    match (val, target_type) {
        (Value::Null, _) => Datum::Null,
        (Value::Bool(b), _) => Datum::Boolean(*b),
        (Value::Number(n), DataType::Integer) => Datum::Integer(n.as_i64().unwrap_or(0) as i32),
        (Value::Number(n), DataType::BigInt) => Datum::BigInt(n.as_i64().unwrap_or(0)),
        (Value::Number(n), DataType::Double) | (Value::Number(n), DataType::Real) => {
            Datum::Double(n.as_f64().unwrap_or(0.0))
        }
        (Value::Number(n), _) => Datum::BigInt(n.as_i64().unwrap_or(0)),
        (Value::String(s), DataType::Text) => Datum::Text(s.clone()),
        (Value::String(s), DataType::VarChar(_)) => Datum::VarChar(s.clone()),
        (Value::String(s), DataType::Char(_)) => Datum::Char(s.clone()),
        (Value::String(s), _) => Datum::Text(s.clone()),
        _ => Datum::Null,
    }
}

fn string_from_value(val: &serde_json::Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => "".to_string(),
    }
}
