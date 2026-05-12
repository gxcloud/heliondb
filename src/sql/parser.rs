use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser as SqlParser;
use crate::error::Result;

pub fn parse_sql(sql: &str) -> Result<Vec<sqlparser::ast::Statement>> {
    let dialect = PostgreSqlDialect {};
    let statements = SqlParser::parse_sql(&dialect, sql)?;
    Ok(statements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_select() {
        let result = parse_sql("SELECT 1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_create_table() {
        let result = parse_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_insert() {
        let result = parse_sql("INSERT INTO users VALUES (1, 'Alice')");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_invalid_sql() {
        let result = parse_sql("CREAT TABLE");
        assert!(result.is_err());
    }
}
