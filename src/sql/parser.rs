use sqlparser::ast::{self, Statement as SqlStatement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser as SqlParser;

use crate::error::{HelionError, Result};
use crate::storage::types::{ColumnMeta, DataType, Datum};

#[derive(Debug, Clone, PartialEq)]
pub enum HelionStatement {
    CreateTable {
        name: String,
        columns: Vec<ColumnMeta>,
        engine: Option<String>,
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
        columns: Vec<String>,
        values: Vec<Vec<Datum>>,
    },
    Select {
        table_name: String,
        columns: Vec<SelectColumn>,
        where_clause: Option<Expression>,
        order_by: Vec<OrderByExpr>,
        limit: Option<u64>,
        offset: Option<u64>,
    },
    Update {
        table_name: String,
        assignments: Vec<(String, Datum)>,
        where_clause: Option<Expression>,
    },
    Delete {
        table_name: String,
        where_clause: Option<Expression>,
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
        permission_type: GrantPermissionType,
    },
    Revoke {
        username: String,
        table: String,
        columns: Vec<String>,
        permission_type: GrantPermissionType,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum GrantPermissionType {
    Select,
    Insert,
    Update,
    Delete,
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectColumn {
    Wildcard,
    Qualified { name: String, alias: Option<String> },
    Expr(Expression),
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrderByDesc {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderByExpr {
    pub expr: Expression,
    pub direction: OrderByDesc,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Column(String),
    Literal(Datum),
    BinaryOp {
        left: Box<Expression>,
        op: BinaryOperator,
        right: Box<Expression>,
    },
    UnaryOp {
        op: UnaryOperator,
        expr: Box<Expression>,
    },
    IsNull(Box<Expression>),
    IsNotNull(Box<Expression>),
    In {
        expr: Box<Expression>,
        list: Vec<Datum>,
    },
    Between {
        expr: Box<Expression>,
        low: Box<Expression>,
        high: Box<Expression>,
    },
    Like {
        expr: Box<Expression>,
        pattern: String,
    },
    Function {
        name: String,
        args: Vec<Expression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOperator {
    Eq, Ne, Lt, Le, Gt, Ge, And, Or, Add, Sub, Mul, Div,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOperator {
    Not, Neg,
}

/// Parse SQL string into a vector of HelionStatement.
pub fn parse(sql: &str) -> Result<Vec<HelionStatement>> {
    let trimmed = sql.trim();

    if looks_like_create_table_with_engine(trimmed) {
        return parse_create_table_with_engine(trimmed);
    }

    if looks_like_alter_table_engine(trimmed) {
        return parse_alter_table_engine(trimmed);
    }

    // First try sqlparser
    let dialect = PostgreSqlDialect {};
    if let Ok(statements) = SqlParser::parse_sql(&dialect, sql) {
        let mut result = Vec::new();
        for stmt in statements {
            match convert_statement(stmt) {
                Ok(s) => result.push(s),
                Err(_e) => {
                    return parse_custom(sql);
                }
            }
        }
        return Ok(result);
    }

    // Fallback: custom parser for non-standard statements
    parse_custom(sql)
}

fn parse_custom(sql: &str) -> Result<Vec<HelionStatement>> {
    let sql = sql.trim().trim_end_matches(';');
    let upper = sql.to_uppercase().trim().to_string();

    if upper.starts_with("CREATE TABLE ") {
        return parse_create_table_with_engine(sql);
    }

    if upper.starts_with("ALTER TABLE ") {
        return parse_alter_table_engine(sql);
    }

    if upper.starts_with("CREATE USER ") || upper.starts_with("CREATE USER IF NOT EXISTS ") {
        let exists = upper.contains("IF NOT EXISTS");
        let stripped = if exists {
            sql.strip_prefix("CREATE USER IF NOT EXISTS").or_else(|| sql.strip_prefix("create user if not exists"))
        } else {
            sql.strip_prefix("CREATE USER").or_else(|| sql.strip_prefix("create user"))
        }.unwrap_or("").trim();

        let (username, password) = parse_user_with_password(stripped)?;
        if exists && find_user_in_stmt(&username) {
            return Ok(vec![]); // no-op
        }
        return Ok(vec![HelionStatement::CreateUser { username, password }]);
    }

    if upper.starts_with("DROP USER ") || upper.starts_with("DROP USER IF EXISTS ") {
        let if_exists = upper.contains("IF EXISTS");
        let stripped = if if_exists {
            sql.strip_prefix("DROP USER IF EXISTS").or_else(|| sql.strip_prefix("drop user if exists"))
        } else {
            sql.strip_prefix("DROP USER").or_else(|| sql.strip_prefix("drop user"))
        }.unwrap_or("").trim().trim_end_matches(';').trim();
        let username = stripped.to_string();
        return Ok(vec![HelionStatement::DropUser { username, if_exists }]);
    }

    if upper.starts_with("ALTER USER ") {
        let stripped = sql.strip_prefix("ALTER USER").or_else(|| sql.strip_prefix("alter user"))
            .unwrap_or("").trim();
        let (username, password) = parse_user_with_password(stripped)?;
        return Ok(vec![HelionStatement::AlterUser { username, password }]);
    }

    if upper.starts_with("GRANT ") {
        return parse_grant_revoke(sql, true);
    }

    if upper.starts_with("REVOKE ") {
        return parse_grant_revoke(sql, false);
    }

    Err(HelionError::Parse(format!("Unsupported SQL: {}", sql)))
}

fn parse_user_with_password(s: &str) -> Result<(String, String)> {
    let s = s.trim().trim_end_matches(';').trim();
    let upper = s.to_uppercase();

    // Try "WITH PASSWORD" first (more specific)
    let keywords = [" WITH PASSWORD ", " PASSWORD "];
    for kw in &keywords {
        if let Some(pos) = upper.find(kw) {
            let username = s[..pos].trim().to_string();
            let rest = s[pos + kw.len()..].trim().trim_end_matches(';').trim();
            let password = if rest.starts_with('\'') {
                let end = rest[1..].find('\'').map(|i| i + 1).unwrap_or(rest.len());
                rest[1..end].to_string()
            } else {
                rest.split_whitespace().next().unwrap_or(rest).to_string()
            };
            return Ok((username, password));
        }
    }

    Err(HelionError::Parse("Expected PASSWORD clause in user statement".into()))
}

fn find_user_in_stmt(_username: &str) -> bool {
    // Simplified: we don't have user store access from the parser
    false
}

fn parse_grant_revoke(sql: &str, is_grant: bool) -> Result<Vec<HelionStatement>> {
    let action = if is_grant { "GRANT" } else { "REVOKE" };
    let upper = sql.to_uppercase();
    let action_upper = if is_grant { "GRANT" } else { "REVOKE" };

    // Find where the action keyword ends, case-insensitively
    let rest = if let Some(pos) = upper.find(action_upper) {
        sql[pos + action_upper.len()..].trim()
    } else {
        return Err(HelionError::Parse(format!("Expected {} keyword", action)));
    };
    let upper_rest = rest.to_uppercase();

    let (perm_type, rest) = if upper_rest.starts_with("ALL") {
        (GrantPermissionType::All, rest[3..].trim())
    } else if upper_rest.starts_with("SELECT") {
        (GrantPermissionType::Select, rest[6..].trim())
    } else if upper_rest.starts_with("INSERT") {
        (GrantPermissionType::Insert, rest[6..].trim())
    } else if upper_rest.starts_with("UPDATE") {
        (GrantPermissionType::Update, rest[6..].trim())
    } else if upper_rest.starts_with("DELETE") {
        (GrantPermissionType::Delete, rest[6..].trim())
    } else {
        return Err(HelionError::Parse(format!("Unknown permission type in {}: {}", action, rest)));
    };

    // Extract column list if present: SELECT(col1, col2)
    let (columns, rest) = if rest.starts_with('(') {
        if let Some(end) = rest.find(')') {
            let cols: Vec<String> = rest[1..end].split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            (cols, rest[end + 1..].trim())
        } else {
            (vec![], rest)
        }
    } else {
        (vec![], rest)
    };

    // Expect "ON table"
    let rest = if rest.to_uppercase().starts_with("ON ") || rest.to_uppercase().starts_with("ON ") {
        if rest.len() > 3 { &rest[3..] } else { "" }
    } else {
        return Err(HelionError::Parse(format!("Expected ON in {} statement", action)));
    };
    let rest = rest.trim();

    // Extract table name (stop at "TO" or "FROM")
    let (table_name, rest) = if is_grant {
        let target = " TO ";
        if let Some(pos) = rest.to_uppercase().find(target) {
            (&rest[..pos], &rest[pos + 4..])
        } else if let Some(pos) = rest.to_uppercase().find(" TO") {
            (&rest[..pos], &rest[pos + 3..])
        } else {
            (rest.trim(), "")
        }
    } else {
        let target = " FROM ";
        if let Some(pos) = rest.to_uppercase().find(target) {
            (&rest[..pos], &rest[pos + 5..])
        } else if let Some(pos) = rest.to_uppercase().find(" FROM") {
            (&rest[..pos], &rest[pos + 5..])
        } else {
            (rest.trim(), "")
        }
    };
    let table_name = table_name.trim().trim_end_matches(';').to_string();

    // Extract username
    let username = rest.trim().trim_end_matches(';').trim().to_string();

    if username.is_empty() {
        return Err(HelionError::Parse(format!("Expected username in {} statement", action)));
    }

    if is_grant {
        Ok(vec![HelionStatement::Grant {
            username,
            table: table_name,
            columns,
            permission_type: perm_type,
        }])
    } else {
        Ok(vec![HelionStatement::Revoke {
            username,
            table: table_name,
            columns,
            permission_type: perm_type,
        }])
    }
}

fn convert_statement(stmt: SqlStatement) -> Result<HelionStatement> {
    match stmt {
        SqlStatement::CreateTable(ct) => {
            let table_name = ct.name.to_string();
            let mut cols = Vec::new();
            for col in ct.columns {
                let col_name = col.name.to_string();
                let data_type = DataType::from_sql(col.data_type)?;
                let mut meta = ColumnMeta::new(&col_name, data_type);

                for opt_def in &col.options {
                    match &opt_def.option {
                        ast::ColumnOption::NotNull => meta.nullable = false,
                        ast::ColumnOption::Null => meta.nullable = true,
                        ast::ColumnOption::Unique { is_primary, .. } => {
                            if *is_primary {
                                meta.is_primary_key = true;
                                meta.is_unique = true;
                                meta.nullable = false;
                            } else {
                                meta.is_unique = true;
                            }
                        }
                        ast::ColumnOption::Default(expr) => {
                            if let Some(d) = sql_literal_to_datum(expr) {
                                meta.default = Some(d);
                            }
                        }
                        _ => {}
                    }
                }
                cols.push(meta);
            }
            Ok(HelionStatement::CreateTable {
                name: table_name,
                columns: cols,
                engine: None,
            })
        }
        SqlStatement::Drop { object_type, if_exists, names, .. } => {
            if matches!(object_type, ast::ObjectType::Table) {
                let name = names.first()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                Ok(HelionStatement::DropTable { name, if_exists })
            } else {
                Err(HelionError::Parse("Only DROP TABLE is supported".into()))
            }
        }
        SqlStatement::Insert(insert) => {
            let table = match &insert.table {
                ast::TableObject::TableName(name) => name.to_string(),
                _ => return Err(HelionError::Parse("Only named tables supported in INSERT".into())),
            };
            let cols: Vec<String> = insert.columns.iter().map(|c| c.to_string()).collect();

            let mut all_values = Vec::new();
            if let Some(source) = &insert.source {
                if let ast::SetExpr::Values(values) = &*source.body {
                    for row in &values.rows {
                        let mut row_data = Vec::new();
                        for expr_val in row {
                            let datum = sql_expr_to_datum(expr_val)?;
                            row_data.push(datum);
                        }
                        all_values.push(row_data);
                    }
                }
            }
            // Also handle INSERT ... SET assignments
            if !insert.assignments.is_empty() {
                let mut row_data = Vec::new();
                for assignment in &insert.assignments {
                    let val = sql_expr_to_datum(&assignment.value)?;
                    row_data.push(val);
                }
                all_values.push(row_data);
            }

            Ok(HelionStatement::Insert {
                table_name: table,
                columns: cols,
                values: all_values,
            })
        }
        SqlStatement::Query(query) => convert_query(*query),
        SqlStatement::Update { table, assignments, selection, .. } => {
            let table_name = table.relation.to_string();
            let mut assigns = Vec::new();
            for a in assignments {
                let col_name = match &a.target {
                    ast::AssignmentTarget::ColumnName(name) => name.to_string(),
                    _ => return Err(HelionError::Parse("Only simple column assignments supported".into())),
                };
                let datum = sql_expr_to_datum(&a.value)?;
                assigns.push((col_name, datum));
            }
            let where_clause = selection.as_ref().map(|e| sql_expr_to_expression(e));

            Ok(HelionStatement::Update {
                table_name,
                assignments: assigns,
                where_clause,
            })
        }
        SqlStatement::Delete(delete) => {
            let tables = match &delete.from {
                ast::FromTable::WithFromKeyword(tables) => tables,
                ast::FromTable::WithoutKeyword(tables) => tables,
            };
            let table_name = tables.first()
                .map(|t| t.relation.to_string())
                .unwrap_or_default();
            Ok(HelionStatement::Delete {
                table_name,
                where_clause: delete.selection.as_ref().map(|e| sql_expr_to_expression(e)),
            })
        }
        other => Err(HelionError::Parse(format!(
            "Unsupported statement: {:?}",
            other
        ))),
    }
}

fn looks_like_create_table_with_engine(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    upper.starts_with("CREATE TABLE ") && upper.contains(" ENGINE")
}

fn looks_like_alter_table_engine(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    upper.starts_with("ALTER TABLE ") && upper.contains(" ENGINE")
}

fn parse_create_table_with_engine(sql: &str) -> Result<Vec<HelionStatement>> {
    let sql = sql.trim().trim_end_matches(';').trim();
    let upper = sql.to_uppercase();
    let engine_pos = upper.rfind(" ENGINE").ok_or_else(|| {
        HelionError::Parse("Expected ENGINE clause in CREATE TABLE statement".into())
    })?;

    let create_sql = sql[..engine_pos].trim();
    let engine_clause = sql[engine_pos..].trim();
    let engine = parse_engine_clause(engine_clause)?;

    let dialect = PostgreSqlDialect {};
    let statements = SqlParser::parse_sql(&dialect, create_sql)
        .map_err(|e| HelionError::Parse(e.to_string()))?;
    let mut result = Vec::new();
    for stmt in statements {
        let mut converted = convert_statement(stmt)?;
        if let HelionStatement::CreateTable { engine: table_engine, .. } = &mut converted {
            *table_engine = Some(engine.clone());
        }
        result.push(converted);
    }
    Ok(result)
}

fn parse_alter_table_engine(sql: &str) -> Result<Vec<HelionStatement>> {
    let sql = sql.trim().trim_end_matches(';').trim();
    let upper = sql.to_uppercase();
    let prefix = "ALTER TABLE ";
    let _after_prefix = sql.get(prefix.len()..).ok_or_else(|| {
        HelionError::Parse("Expected ALTER TABLE statement".into())
    })?.trim();

    let engine_pos = upper.rfind(" ENGINE").ok_or_else(|| {
        HelionError::Parse("Expected ENGINE clause in ALTER TABLE statement".into())
    })?;
    let before_engine = sql[prefix.len()..engine_pos].trim();
    let mut parts = before_engine.split_whitespace();
    let name = parts.next().ok_or_else(|| {
        HelionError::Parse("Expected table name in ALTER TABLE statement".into())
    })?;

    if parts.next().is_some() {
        return Err(HelionError::Parse("Unsupported ALTER TABLE syntax".into()));
    }

    let engine_clause = sql[engine_pos..].trim();
    let engine = parse_engine_clause(engine_clause)?;

    Ok(vec![HelionStatement::AlterTableEngine {
        name: name.trim_matches('`').to_string(),
        engine,
    }])
}

fn parse_engine_clause(clause: &str) -> Result<String> {
    let clause = clause.trim().trim_end_matches(';').trim();
    let upper = clause.to_uppercase();
    if !upper.starts_with("ENGINE") {
        return Err(HelionError::Parse("Expected ENGINE clause".into()));
    }

    let rhs = clause["ENGINE".len()..].trim();
    let rhs = rhs.strip_prefix('=').unwrap_or(rhs).trim();
    let engine = rhs.split_whitespace().next().ok_or_else(|| {
        HelionError::Parse("Expected engine name".into())
    })?;
    Ok(engine.trim_matches('`').to_string())
}

fn convert_query(query: ast::Query) -> Result<HelionStatement> {
    let body = &*query.body;
    match body {
        ast::SetExpr::Select(select) => {
            let from = select.from.first().ok_or_else(|| {
                HelionError::Parse("SELECT requires a FROM clause".into())
            })?;

            let table_name = match &from.relation {
                ast::TableFactor::Table { name, .. } => name.to_string(),
                other => return Err(HelionError::Parse(format!(
                    "Unsupported table expression: {:?}", other
                ))),
            };

            let columns: Vec<SelectColumn> = select.projection.iter().map(|p| {
                match p {
                    ast::SelectItem::Wildcard(_) => SelectColumn::Wildcard,
                    ast::SelectItem::UnnamedExpr(expr) => {
                        SelectColumn::Expr(sql_expr_to_expression(expr))
                    }
                    ast::SelectItem::ExprWithAlias { expr, alias } => {
                        SelectColumn::Qualified {
                            name: expr.to_string(),
                            alias: Some(alias.to_string()),
                        }
                    }
                    ast::SelectItem::QualifiedWildcard(_, _) => SelectColumn::Wildcard,
                }
            }).collect();

            let where_clause = select.selection.as_ref().map(|e| sql_expr_to_expression(e));

            let order_by: Vec<OrderByExpr> = match &query.order_by {
                None => vec![],
                Some(order_by) => {
                    match &order_by.kind {
                        ast::OrderByKind::Expressions(exprs) => {
                            exprs.iter().map(|o| {
                                OrderByExpr {
                                    expr: sql_expr_to_expression(&o.expr),
                                    direction: match o.options.asc {
                                        Some(false) => OrderByDesc::Desc,
                                        _ => OrderByDesc::Asc,
                                    },
                                }
                            }).collect()
                        }
                        _ => vec![],
                    }
                }
            };

            let limit = query.limit.as_ref().and_then(|e| {
                let inner = match e {
                    ast::Expr::Value(vws) => &vws.value,
                    _ => return None,
                };
                match inner {
                    ast::Value::Number(n, _) => n.parse::<u64>().ok(),
                    _ => None,
                }
            });

            let offset = query.offset.as_ref().and_then(|o| {
                let inner = match &o.value {
                    ast::Expr::Value(vws) => &vws.value,
                    _ => return None,
                };
                match inner {
                    ast::Value::Number(n, _) => n.parse::<u64>().ok(),
                    _ => None,
                }
            });

            Ok(HelionStatement::Select {
                table_name,
                columns,
                where_clause,
                order_by,
                limit,
                offset,
            })
        }
        _ => Err(HelionError::Parse("Only SELECT queries are supported".into())),
    }
}

fn sql_expr_to_expression(expr: &ast::Expr) -> Expression {
    match expr {
        ast::Expr::Identifier(id) => Expression::Column(id.to_string()),
        ast::Expr::CompoundIdentifier(parts) => {
            Expression::Column(parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("."))
        }
        ast::Expr::Value(vws) => Expression::Literal(sql_value_to_datum(&vws.value)),
        ast::Expr::BinaryOp { left, op, right } => {
            let bin_op = match op {
                ast::BinaryOperator::Eq => BinaryOperator::Eq,
                ast::BinaryOperator::NotEq => BinaryOperator::Ne,
                ast::BinaryOperator::Lt => BinaryOperator::Lt,
                ast::BinaryOperator::LtEq => BinaryOperator::Le,
                ast::BinaryOperator::Gt => BinaryOperator::Gt,
                ast::BinaryOperator::GtEq => BinaryOperator::Ge,
                ast::BinaryOperator::And => BinaryOperator::And,
                ast::BinaryOperator::Or => BinaryOperator::Or,
                ast::BinaryOperator::Plus => BinaryOperator::Add,
                ast::BinaryOperator::Minus => BinaryOperator::Sub,
                ast::BinaryOperator::Multiply => BinaryOperator::Mul,
                ast::BinaryOperator::Divide => BinaryOperator::Div,
                _ => return Expression::Literal(Datum::Null),
            };
            Expression::BinaryOp {
                left: Box::new(sql_expr_to_expression(left)),
                op: bin_op,
                right: Box::new(sql_expr_to_expression(right)),
            }
        }
        ast::Expr::UnaryOp { op, expr: inner } => {
            let un_op = match op {
                ast::UnaryOperator::Not => UnaryOperator::Not,
                ast::UnaryOperator::Minus => UnaryOperator::Neg,
                _ => return Expression::Literal(Datum::Null),
            };
            Expression::UnaryOp {
                op: un_op,
                expr: Box::new(sql_expr_to_expression(inner)),
            }
        }
        ast::Expr::IsNull(inner) => Expression::IsNull(Box::new(sql_expr_to_expression(inner))),
        ast::Expr::IsNotNull(inner) => Expression::IsNotNull(Box::new(sql_expr_to_expression(inner))),
        ast::Expr::InList { expr: inner, list, .. } => {
            let datums: Vec<Datum> = list.iter().filter_map(|e| sql_expr_to_datum_opt(e)).collect();
            Expression::In {
                expr: Box::new(sql_expr_to_expression(inner)),
                list: datums,
            }
        }
        ast::Expr::Between { expr: inner, low, high, .. } => {
            Expression::Between {
                expr: Box::new(sql_expr_to_expression(inner)),
                low: Box::new(sql_expr_to_expression(low)),
                high: Box::new(sql_expr_to_expression(high)),
            }
        }
        ast::Expr::Like { expr: inner, pattern, .. } => {
            let pat_str = match pattern.as_ref() {
                ast::Expr::Value(vws) => match &vws.value {
                    ast::Value::SingleQuotedString(s) => s.clone(),
                    _ => pattern.to_string(),
                },
                _ => pattern.to_string(),
            };
            Expression::Like {
                expr: Box::new(sql_expr_to_expression(inner)),
                pattern: pat_str,
            }
        }
        ast::Expr::Function(func) => {
            let name = func.name.to_string();
            let args: Vec<Expression> = match &func.args {
                ast::FunctionArguments::List(list) => {
                    list.args.iter().filter_map(|a| {
                        match a {
                            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(e)) => {
                                Some(sql_expr_to_expression(e))
                            }
                            _ => None,
                        }
                    }).collect()
                }
                _ => vec![],
            };
            Expression::Function { name, args }
        }
        ast::Expr::Nested(inner) => sql_expr_to_expression(inner),
        ast::Expr::Cast { expr: inner, .. } => sql_expr_to_expression(inner),
        _ => Expression::Literal(Datum::Null),
    }
}

fn sql_expr_to_datum(expr: &ast::Expr) -> Result<Datum> {
    match expr {
        ast::Expr::Value(vws) => Ok(sql_value_to_datum(&vws.value)),
        ast::Expr::UnaryOp { op: ast::UnaryOperator::Minus, expr: inner } => {
            match inner.as_ref() {
                ast::Expr::Value(vws) => {
                    let n = match &vws.value {
                        ast::Value::Number(n, _) => n,
                        _ => return Err(HelionError::Parse("Invalid negative expression".into())),
                    };
                    if n.contains('.') {
                        Ok(Datum::Double(-n.parse::<f64>().map_err(|_| {
                            HelionError::Parse(format!("Invalid number: {}", n))
                        })?))
                    } else {
                        Ok(Datum::Integer(-n.parse::<i64>().map_err(|_| {
                            HelionError::Parse(format!("Invalid integer: {}", n))
                        })? as i32))
                    }
                }
                _ => Err(HelionError::Parse("Invalid negative expression".into())),
            }
        }
        other => Err(HelionError::Parse(format!(
            "Expected literal value, got: {:?}",
            other
        ))),
    }
}

fn sql_expr_to_datum_opt(expr: &ast::Expr) -> Option<Datum> {
    match expr {
        ast::Expr::Value(vws) => Some(sql_value_to_datum(&vws.value)),
        _ => None,
    }
}

fn sql_value_to_datum(value: &ast::Value) -> Datum {
    match value {
        ast::Value::Number(n, _) => {
            if n.contains('.') {
                n.parse::<f64>().map(Datum::Double).unwrap_or(Datum::Null)
            } else {
                n.parse::<i64>().map(Datum::BigInt).unwrap_or(Datum::Null)
            }
        }
        ast::Value::SingleQuotedString(s) | ast::Value::NationalStringLiteral(s) => {
            Datum::Text(s.clone())
        }
        ast::Value::Boolean(b) => Datum::Boolean(*b),
        ast::Value::Null => Datum::Null,
        _ => Datum::Null,
    }
}

fn sql_literal_to_datum(expr: &ast::Expr) -> Option<Datum> {
    match expr {
        ast::Expr::Value(vws) => Some(sql_value_to_datum(&vws.value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_create_table() {
        let sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)";
        let stmts = parse(sql).unwrap();
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            HelionStatement::CreateTable { name, columns, engine } => {
                assert_eq!(name, "users");
                assert_eq!(columns.len(), 3);
                assert!(columns[0].is_primary_key);
                assert!(!columns[0].nullable);
                assert!(!columns[1].nullable);
                assert!(columns[2].nullable);
                assert_eq!(columns[0].data_type, DataType::Integer);
                assert_eq!(columns[1].data_type, DataType::Text);
                assert!(engine.is_none());
            }
            _ => panic!("Expected CreateTable"),
        }
    }

    #[test]
    fn test_parse_drop_table() {
        let sql = "DROP TABLE users";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::DropTable { name, if_exists } => {
                assert_eq!(name, "users");
                assert!(!if_exists);
            }
            _ => panic!("Expected DropTable"),
        }
    }

    #[test]
    fn test_parse_drop_table_if_exists() {
        let sql = "DROP TABLE IF EXISTS users";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::DropTable { name, if_exists } => {
                assert_eq!(name, "users");
                assert!(if_exists);
            }
            _ => panic!("Expected DropTable"),
        }
    }

    #[test]
    fn test_parse_insert() {
        let sql = "INSERT INTO users (id, name) VALUES (1, 'Alice')";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Insert { table_name, columns, values } => {
                assert_eq!(table_name, "users");
                assert_eq!(columns.len(), 2);
                assert_eq!(values.len(), 1);
                assert_eq!(values[0][0], Datum::BigInt(1));
                assert_eq!(values[0][1], Datum::Text("Alice".to_string()));
            }
            _ => panic!("Expected Insert"),
        }
    }

    #[test]
    fn test_parse_select() {
        let sql = "SELECT id, name FROM users WHERE id > 1 ORDER BY name DESC LIMIT 10 OFFSET 5";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Select { table_name, columns, where_clause, order_by, limit, offset } => {
                assert_eq!(table_name, "users");
                assert_eq!(columns.len(), 2);
                assert!(where_clause.is_some());
                assert_eq!(order_by.len(), 1);
                assert_eq!(*limit, Some(10));
                assert_eq!(*offset, Some(5));
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_wildcard() {
        let sql = "SELECT * FROM users";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Select { columns, .. } => {
                assert_eq!(columns.len(), 1);
                assert_eq!(columns[0], SelectColumn::Wildcard);
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_update() {
        let sql = "UPDATE users SET name = 'Bob' WHERE id = 1";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Update { table_name, assignments, where_clause } => {
                assert_eq!(table_name, "users");
                assert_eq!(assignments.len(), 1);
                assert_eq!(assignments[0].0, "name");
                assert!(where_clause.is_some());
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_parse_delete() {
        let sql = "DELETE FROM users WHERE id = 1";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Delete { table_name, where_clause } => {
                assert_eq!(table_name, "users");
                assert!(where_clause.is_some());
            }
            _ => panic!("Expected Delete"),
        }
    }

    #[test]
    fn test_parse_invalid_sql() {
        let result = parse("CREAT TABLE");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_multiple_statements() {
        let sql = "CREATE TABLE t (id INTEGER); DROP TABLE t;";
        let stmts = parse(sql).unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0], HelionStatement::CreateTable { .. }));
        assert!(matches!(stmts[1], HelionStatement::DropTable { .. }));
    }

    #[test]
    fn test_parse_insert_without_columns() {
        let sql = "INSERT INTO users VALUES (1, 'Alice')";
        let stmts = parse(sql).unwrap();
        assert!(matches!(stmts[0], HelionStatement::Insert { .. }));
    }

    #[test]
    fn test_parse_delete_all() {
        let sql = "DELETE FROM users";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Delete { where_clause, .. } => {
                assert!(where_clause.is_none());
            }
            _ => panic!("Expected Delete"),
        }
    }

    #[test]
    fn test_parsed_expression_binary_op() {
        let sql = "SELECT * FROM t WHERE age > 18";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Select { where_clause: Some(expr), .. } => {
                match expr {
                    Expression::BinaryOp { op: BinaryOperator::Gt, .. } => {}
                    _ => panic!("Expected Gt binary op"),
                }
            }
            _ => panic!("Expected Select with WHERE"),
        }
    }

    #[test]
    fn test_parse_insert_with_different_types() {
        let sql = "INSERT INTO t VALUES (42, 3.14, 'hello', TRUE, NULL)";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Insert { values, .. } => {
                assert_eq!(values[0][0], Datum::BigInt(42));
                assert_eq!(values[0][1], Datum::Double(3.14));
                assert_eq!(values[0][2], Datum::Text("hello".to_string()));
                assert_eq!(values[0][3], Datum::Boolean(true));
                assert_eq!(values[0][4], Datum::Null);
            }
            _ => panic!("Expected Insert"),
        }
    }

    // ── User/Permission Statement Tests ─────────────────────────────────

    #[test]
    fn test_parse_create_user() {
        let sql = "CREATE USER alice WITH PASSWORD 'secret123'";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::CreateUser { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "secret123");
            }
            _ => panic!("Expected CreateUser"),
        }
    }

    #[test]
    fn test_parse_drop_user() {
        let sql = "DROP USER alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::DropUser { username, if_exists } => {
                assert_eq!(username, "alice");
                assert!(!if_exists);
            }
            _ => panic!("Expected DropUser"),
        }
    }

    #[test]
    fn test_parse_drop_user_if_exists() {
        let sql = "DROP USER IF EXISTS alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::DropUser { username, if_exists } => {
                assert_eq!(username, "alice");
                assert!(if_exists);
            }
            _ => panic!("Expected DropUser"),
        }
    }

    #[test]
    fn test_parse_alter_user() {
        let sql = "ALTER USER alice WITH PASSWORD 'newpass'";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::AlterUser { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "newpass");
            }
            _ => panic!("Expected AlterUser"),
        }
    }

    #[test]
    fn test_parse_grant_select() {
        let sql = "GRANT SELECT ON users TO alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Grant { username, table, columns, permission_type } => {
                assert_eq!(username, "alice");
                assert_eq!(table, "users");
                assert!(columns.is_empty());
                assert_eq!(*permission_type, GrantPermissionType::Select);
            }
            _ => panic!("Expected Grant"),
        }
    }

    #[test]
    fn test_parse_grant_select_columns() {
        let sql = "GRANT SELECT(id, name) ON users TO alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Grant { username, table, columns, permission_type } => {
                assert_eq!(username, "alice");
                assert_eq!(table, "users");
                assert_eq!(columns, &["id", "name"]);
                assert_eq!(*permission_type, GrantPermissionType::Select);
            }
            _ => panic!("Expected Grant with columns"),
        }
    }

    #[test]
    fn test_parse_grant_insert() {
        let sql = "GRANT INSERT ON users TO alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Grant { permission_type, .. } => {
                assert_eq!(*permission_type, GrantPermissionType::Insert);
            }
            _ => panic!("Expected Grant"),
        }
    }

    #[test]
    fn test_parse_grant_update() {
        let sql = "GRANT UPDATE(email) ON users TO alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Grant { permission_type, columns, .. } => {
                assert_eq!(*permission_type, GrantPermissionType::Update);
                assert_eq!(columns, &["email"]);
            }
            _ => panic!("Expected Grant"),
        }
    }

    #[test]
    fn test_parse_grant_delete() {
        let sql = "GRANT DELETE ON users TO alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Grant { permission_type, .. } => {
                assert_eq!(*permission_type, GrantPermissionType::Delete);
            }
            _ => panic!("Expected Grant"),
        }
    }

    #[test]
    fn test_parse_grant_all() {
        let sql = "GRANT ALL ON users TO alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Grant { permission_type, .. } => {
                assert_eq!(*permission_type, GrantPermissionType::All);
            }
            _ => panic!("Expected Grant All"),
        }
    }

    #[test]
    fn test_parse_revoke_select() {
        let sql = "REVOKE SELECT ON users FROM alice";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::Revoke { username, table, .. } => {
                assert_eq!(username, "alice");
                assert_eq!(table, "users");
            }
            _ => panic!("Expected Revoke"),
        }
    }

    #[test]
    fn test_parse_create_user_password() {
        let sql = "CREATE USER bob PASSWORD 'test123'";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::CreateUser { username, password } => {
                assert_eq!(username, "bob");
                assert_eq!(password, "test123");
            }
            _ => panic!("Expected CreateUser"),
            }
        }
    }

    #[test]
    fn test_parse_create_table_with_engine() {
        let sql = "CREATE TABLE users (id INTEGER) ENGINE = disk";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::CreateTable { engine, .. } => {
                assert_eq!(engine.as_deref(), Some("disk"));
            }
            _ => panic!("Expected CreateTable"),
        }
    }

    #[test]
    fn test_parse_alter_table_engine() {
        let sql = "ALTER TABLE users ENGINE = memory";
        let stmts = parse(sql).unwrap();
        match &stmts[0] {
            HelionStatement::AlterTableEngine { name, engine } => {
                assert_eq!(name, "users");
                assert_eq!(engine, "memory");
            }
            _ => panic!("Expected AlterTableEngine"),
        }
    }
