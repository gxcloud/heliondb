use crate::error::{HelionError, Result};
use crate::sql::parser::*;
use crate::storage::table::Table;
use crate::storage::types::{ColumnMeta, Datum, Row};

/// A logical plan node representing a database operation.
#[derive(Debug, Clone)]
pub enum LogicalPlan {
    CreateTable {
        name: String,
        columns: Vec<ColumnMeta>,
    },
    DropTable {
        name: String,
        if_exists: bool,
    },
    Insert {
        table_name: String,
        rows: Vec<Row>,
    },
    Select {
        table_name: String,
        columns: Vec<usize>,              // column indices for projection
        wildcard: bool,                    // SELECT *
        where_clause: Option<Expression>,
        order_by: Vec<OrderByExpr>,
        limit: Option<u64>,
        offset: Option<u64>,
        table_columns: Vec<ColumnMeta>,   // resolved column metadata
    },
    Update {
        table_name: String,
        set_indices: Vec<usize>,          // column indices to update
        set_values: Vec<Datum>,
        where_clause: Option<Expression>,
        table_columns: Vec<ColumnMeta>,
    },
    Delete {
        table_name: String,
        where_clause: Option<Expression>,
        table_columns: Vec<ColumnMeta>,
    },
}

/// Plan a parsed statement against the available table schemas.
pub fn plan(
    statement: &HelionStatement,
    tables: &[Table],
) -> Result<LogicalPlan> {
    match statement {
        HelionStatement::CreateTable { name, columns } => {
            Ok(LogicalPlan::CreateTable {
                name: name.clone(),
                columns: columns.clone(),
            })
        }
        HelionStatement::DropTable { name, if_exists } => {
            Ok(LogicalPlan::DropTable {
                name: name.clone(),
                if_exists: *if_exists,
            })
        }
        HelionStatement::Insert { table_name, columns, values } => {
            let table = find_table(tables, table_name)?;
            let col_indices = if columns.is_empty() {
                // Use all columns in order
                (0..table.columns.len()).collect()
            } else {
                columns.iter().map(|c| {
                    table.column_index(c).ok_or_else(|| {
                        HelionError::ColumnNotFound(format!("{}.{}", table_name, c))
                    })
                }).collect::<Result<Vec<_>>>()?
            };

            let mut rows = Vec::new();
            for row_values in values {
                let mut full_row = vec![Datum::Null; table.columns.len()];
                for (i, val) in row_values.iter().enumerate() {
                    if i < col_indices.len() {
                        full_row[col_indices[i]] = val.clone();
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
        HelionStatement::Select { table_name, columns, where_clause, order_by, limit, offset } => {
            let table = find_table(tables, table_name)?;

            let (col_indices, wildcard) = resolve_columns(columns, table)?;

            Ok(LogicalPlan::Select {
                table_name: table_name.clone(),
                columns: col_indices,
                wildcard,
                where_clause: where_clause.clone(),
                order_by: order_by.clone(),
                limit: *limit,
                offset: *offset,
                table_columns: table.columns.clone(),
            })
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
                set_values.push(val.clone());
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
    }
}

fn find_table<'a>(tables: &'a [Table], name: &str) -> Result<&'a Table> {
    tables.iter().find(|t| t.name == name)
        .ok_or_else(|| HelionError::TableNotFound(name.to_string()))
}

fn resolve_columns(
    columns: &[SelectColumn],
    table: &Table,
) -> Result<(Vec<usize>, bool)> {
    let mut indices = Vec::new();
    let mut wildcard = false;

    for col in columns {
        match col {
            SelectColumn::Wildcard => {
                wildcard = true;
            }
            SelectColumn::Qualified { name, .. } | SelectColumn::Expr(Expression::Column(name)) => {
                let idx = table.column_index(name).ok_or_else(|| {
                    HelionError::ColumnNotFound(format!("{}.{}", table.name, name))
                })?;
                indices.push(idx);
            }
            SelectColumn::Expr(_) => {
                // Expression-based columns (like literals or function results)
                // For now, we don't have a good way to represent these in column indices.
                // We'll handle this during execution.
                wildcard = true;
            }
        }
    }

    if wildcard && indices.is_empty() {
        indices = (0..table.columns.len()).collect();
    }

    Ok((indices, wildcard))
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
        match plan {
            LogicalPlan::Select { columns, wildcard, .. } => {
                assert!(!wildcard);
                assert_eq!(columns.len(), 2);
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_plan_select_wildcard() {
        let stmts = parse("SELECT * FROM users").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        match plan {
            LogicalPlan::Select { wildcard, columns, .. } => {
                assert!(wildcard);
                assert_eq!(columns.len(), 3); // all columns
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_plan_update() {
        let stmts = parse("UPDATE users SET age = 31 WHERE id = 1").unwrap();
        let plan = plan(&stmts[0], &make_tables()).unwrap();
        match plan {
            LogicalPlan::Update { set_indices, set_values, .. } => {
                assert_eq!(set_indices, vec![2]); // age is at index 2
                assert_eq!(set_values[0], Datum::BigInt(31));
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
}
