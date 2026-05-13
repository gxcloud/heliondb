use crate::executor::ops::QueryResult;

/// Print a query result to stdout.
pub fn print_result(result: &QueryResult, expanded: bool) {
    if result.columns.is_empty() && result.rows.is_empty() {
        println!("(empty)");
        return;
    }

    if result.columns.is_empty() {
        // DML result — show rows_affected if > 0
        if result.rows_affected > 0 || !result.rows.is_empty() {
            let count = if result.rows_affected > 0 {
                result.rows_affected
            } else {
                result.rows.len() as u64
            };
            println!("INSERT 0 {}", count);
        }
        return;
    }

    if expanded {
        print_expanded(result);
    } else {
        print_table(result);
    }

    let row_count = result.rows.len();
    let noun = if row_count == 1 { "row" } else { "rows" };
    println!("({} {})", row_count, noun);
}

fn print_table(result: &QueryResult) {
    let col_count = result.columns.len();
    if col_count == 0 {
        return;
    }

    // Calculate column widths
    let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();

    for row in &result.rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(val.len());
            }
        }
    }

    // Build separator line
    let sep: String = widths
        .iter()
        .map(|w| "─".repeat(*w + 2))
        .collect::<Vec<_>>()
        .join("┼");

    // Print header
    let header: String = result
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!(" {} ", pad(c, widths[i])))
        .collect::<Vec<_>>()
        .join("│");
    println!(" {}", header);
    println!("─{}─", sep);

    // Print rows
    for row in &result.rows {
        let line: String = (0..col_count)
            .map(|i| {
                let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
                format!(" {} ", pad(val, widths[i]))
            })
            .collect::<Vec<_>>()
            .join("│");
        println!(" {}", line);
    }
}

fn print_expanded(result: &QueryResult) {
    for (row_idx, row) in result.rows.iter().enumerate() {
        if row_idx > 0 {
            println!();
        }
        println!("─[ RECORD {} ]─", row_idx + 1);
        for (col_idx, col_name) in result.columns.iter().enumerate() {
            let val = row.get(col_idx).map(|s| s.as_str()).unwrap_or("NULL");
            println!("{} | {}", col_name, val);
        }
    }
}

fn pad(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        let mut result = String::with_capacity(width);
        result.push_str(s);
        result.push_str(&" ".repeat(width - s.len()));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(columns: Vec<&str>, rows: Vec<Vec<&str>>) -> QueryResult {
        QueryResult {
            columns: columns.into_iter().map(String::from).collect(),
            column_types: vec![],
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(String::from).collect())
                .collect(),
            rows_affected: 0,
        }
    }

    #[test]
    fn test_dml_result() {
        let r = QueryResult {
            columns: vec![],
            column_types: vec![],
            rows: vec![],
            rows_affected: 3,
        };
        print_result(&r, false);
        // Should print "INSERT 0 3"
    }

    #[test]
    fn test_table_output() {
        let r = make_result(
            vec!["id", "name"],
            vec![vec!["1", "Alice"], vec!["2", "Bob"]],
        );
        print_result(&r, false);
    }

    #[test]
    fn test_expanded_output() {
        let r = make_result(vec!["id", "name"], vec![vec!["1", "Alice"]]);
        print_result(&r, true);
    }
}
