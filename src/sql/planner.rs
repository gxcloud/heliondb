use crate::error::Result;

pub struct LogicalPlan {
    pub description: String,
}

pub fn plan(statements: &[sqlparser::ast::Statement]) -> Result<Vec<LogicalPlan>> {
    let plans = statements
        .iter()
        .map(|stmt| LogicalPlan {
            description: format!("{:?}", stmt),
        })
        .collect();
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::parse_sql;

    #[test]
    fn test_plan_create_table() {
        let stmts = parse_sql("CREATE TABLE t (id INTEGER)").unwrap();
        let plans = plan(&stmts).unwrap();
        assert_eq!(plans.len(), 1);
    }
}
