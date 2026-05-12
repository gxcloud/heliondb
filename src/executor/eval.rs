use crate::error::{HelionError, Result};
use crate::sql::parser::{
    BinaryOperator, Expression, UnaryOperator,
};
use crate::storage::types::{ColumnMeta, Datum};

/// Evaluate an expression against a row of data.
pub fn evaluate(expr: &Expression, row: &[Datum], columns: &[ColumnMeta]) -> Result<Datum> {
    match expr {
        Expression::Literal(d) => Ok(d.clone()),
        Expression::Column(name) => {
            let idx = columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| HelionError::ColumnNotFound(name.clone()))?;
            Ok(row.get(idx).cloned().unwrap_or(Datum::Null))
        }
        Expression::BinaryOp { left, op, right } => {
            let left_val = evaluate(left, row, columns)?;
            let right_val = evaluate(right, row, columns)?;

            if left_val.is_null() || right_val.is_null() {
                return Ok(Datum::Null);
            }

            match op {
                BinaryOperator::Eq => Ok(Datum::Boolean(datum_eq(&left_val, &right_val))),
                BinaryOperator::Ne => Ok(Datum::Boolean(!datum_eq(&left_val, &right_val))),
                BinaryOperator::Lt => Ok(Datum::Boolean(datum_cmp(&left_val, &right_val) == Some(std::cmp::Ordering::Less))),
                BinaryOperator::Le => Ok(Datum::Boolean(
                    datum_cmp(&left_val, &right_val).map(|o| o != std::cmp::Ordering::Greater).unwrap_or(false)
                )),
                BinaryOperator::Gt => Ok(Datum::Boolean(datum_cmp(&left_val, &right_val) == Some(std::cmp::Ordering::Greater))),
                BinaryOperator::Ge => Ok(Datum::Boolean(
                    datum_cmp(&left_val, &right_val).map(|o| o != std::cmp::Ordering::Less).unwrap_or(false)
                )),
                BinaryOperator::And => {
                    let lb = datum_to_bool(&left_val);
                    let rb = datum_to_bool(&right_val);
                    Ok(Datum::Boolean(lb && rb))
                }
                BinaryOperator::Or => {
                    let lb = datum_to_bool(&left_val);
                    let rb = datum_to_bool(&right_val);
                    Ok(Datum::Boolean(lb || rb))
                }
                BinaryOperator::Add => datum_arith(&left_val, &right_val, |a, b| a + b),
                BinaryOperator::Sub => datum_arith(&left_val, &right_val, |a, b| a - b),
                BinaryOperator::Mul => datum_arith(&left_val, &right_val, |a, b| a * b),
                BinaryOperator::Div => datum_arith(&left_val, &right_val, |a, b| a / b),
            }
        }
        Expression::UnaryOp { op, expr: inner } => {
            let val = evaluate(inner, row, columns)?;
            match op {
                UnaryOperator::Not => Ok(Datum::Boolean(!datum_to_bool(&val))),
                UnaryOperator::Neg => negate_datum(&val),
            }
        }
        Expression::IsNull(inner) => {
            let val = evaluate(inner, row, columns)?;
            Ok(Datum::Boolean(val.is_null()))
        }
        Expression::IsNotNull(inner) => {
            let val = evaluate(inner, row, columns)?;
            Ok(Datum::Boolean(!val.is_null()))
        }
        Expression::In { expr: inner, list } => {
            let val = evaluate(inner, row, columns)?;
            Ok(Datum::Boolean(list.iter().any(|d| datum_eq(d, &val))))
        }
        Expression::Between { expr: inner, low, high } => {
            let val = evaluate(inner, row, columns)?;
            let low_val = evaluate(low, row, columns)?;
            let high_val = evaluate(high, row, columns)?;
            if val.is_null() || low_val.is_null() || high_val.is_null() {
                return Ok(Datum::Null);
            }
            let ge_low = datum_cmp(&val, &low_val).map(|o| o != std::cmp::Ordering::Less).unwrap_or(false);
            let le_high = datum_cmp(&val, &high_val).map(|o| o != std::cmp::Ordering::Greater).unwrap_or(false);
            Ok(Datum::Boolean(ge_low && le_high))
        }
        Expression::Like { expr: inner, pattern } => {
            let val = evaluate(inner, row, columns)?;
            let s = datum_to_string(&val);
            // Simple LIKE: % matches any sequence, _ matches single char
            let regex_pattern = like_to_regex(pattern);
            let re = regex_lite(&regex_pattern, &s);
            Ok(Datum::Boolean(re))
        }
        Expression::Function { name, args } => {
            let evaluated_args: Result<Vec<Datum>> = args.iter()
                .map(|a| evaluate(a, row, columns))
                .collect();
            let args = evaluated_args?;
            evaluate_function(name, &args)
        }
    }
}

fn datum_eq(a: &Datum, b: &Datum) -> bool {
    match (a, b) {
        (Datum::Null, _) | (_, Datum::Null) => false,
        _ => a == b,
    }
}

fn datum_cmp(a: &Datum, b: &Datum) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Datum::Null, _) | (_, Datum::Null) => None,
        (Datum::Integer(ai), Datum::Integer(bi)) => Some(ai.cmp(bi)),
        (Datum::Integer(ai), Datum::BigInt(bi)) => Some((*ai as i64).cmp(bi)),
        (Datum::BigInt(ai), Datum::Integer(bi)) => Some(ai.cmp(&(*bi as i64))),
        (Datum::BigInt(ai), Datum::BigInt(bi)) => Some(ai.cmp(bi)),
        (Datum::Double(af), Datum::Double(bf)) => af.partial_cmp(bf),
        (Datum::Text(as_), Datum::Text(bs_)) => Some(as_.cmp(bs_)),
        (Datum::VarChar(as_), Datum::Text(bs_)) => Some(as_.cmp(bs_)),
        (Datum::Text(as_), Datum::VarChar(bs_)) => Some(as_.cmp(bs_)),
        (Datum::Boolean(ab), Datum::Boolean(bb)) => Some(ab.cmp(bb)),
        _ => None,
    }
}

fn datum_to_bool(d: &Datum) -> bool {
    match d {
        Datum::Boolean(b) => *b,
        Datum::Null => false,
        Datum::Integer(i) => *i != 0,
        Datum::BigInt(i) => *i != 0,
        Datum::Text(s) => !s.is_empty(),
        _ => true,
    }
}

fn datum_to_string(d: &Datum) -> String {
    match d {
        Datum::Null => String::new(),
        Datum::Text(s) | Datum::VarChar(s) | Datum::Char(s) => s.clone(),
        other => other.display(),
    }
}

fn datum_arith<F>(a: &Datum, b: &Datum, op: F) -> Result<Datum>
where F: Fn(f64, f64) -> f64 {
    match (a, b) {
        (Datum::Integer(ai), Datum::Integer(bi)) => Ok(Datum::Integer(op(*ai as f64, *bi as f64) as i32)),
        (Datum::BigInt(ai), Datum::BigInt(bi)) => Ok(Datum::BigInt(op(*ai as f64, *bi as f64) as i64)),
        (Datum::Double(af), Datum::Double(bf)) => Ok(Datum::Double(op(*af, *bf))),
        (Datum::Integer(ai), Datum::Double(bf)) => Ok(Datum::Double(op(*ai as f64, *bf))),
        (Datum::Double(af), Datum::Integer(bi)) => Ok(Datum::Double(op(*af, *bi as f64))),
        (Datum::BigInt(ai), Datum::Integer(bi)) => Ok(Datum::BigInt(op(*ai as f64, *bi as f64) as i64)),
        (Datum::Integer(ai), Datum::BigInt(bi)) => Ok(Datum::BigInt(op(*ai as f64, *bi as f64) as i64)),
        _ => Err(HelionError::TypeMismatch {
            expected: a.data_type().to_string(),
            actual: b.data_type().to_string(),
        }),
    }
}

fn negate_datum(d: &Datum) -> Result<Datum> {
    match d {
        Datum::Integer(i) => Ok(Datum::Integer(-i)),
        Datum::BigInt(i) => Ok(Datum::BigInt(-i)),
        Datum::Double(f) => Ok(Datum::Double(-f)),
        _ => Err(HelionError::TypeMismatch {
            expected: "numeric".to_string(),
            actual: d.data_type().to_string(),
        }),
    }
}

fn evaluate_function(name: &str, args: &[Datum]) -> Result<Datum> {
    let fn_name = name.to_lowercase();
    match fn_name.as_str() {
        "count" => {
            if args.is_empty() {
                Ok(Datum::BigInt(0))
            } else {
                Ok(Datum::BigInt(args.iter().filter(|a| !a.is_null()).count() as i64))
            }
        }
        "sum" => {
            let total: f64 = args.iter().filter_map(|a| match a {
                Datum::Integer(i) => Some(*i as f64),
                Datum::BigInt(i) => Some(*i as f64),
                Datum::Double(f) => Some(*f),
                _ => None,
            }).sum();
            Ok(Datum::Double(total))
        }
        "avg" => {
            let nums: Vec<f64> = args.iter().filter_map(|a| match a {
                Datum::Integer(i) => Some(*i as f64),
                Datum::BigInt(i) => Some(*i as f64),
                Datum::Double(f) => Some(*f),
                _ => None,
            }).collect();
            if nums.is_empty() {
                Ok(Datum::Null)
            } else {
                Ok(Datum::Double(nums.iter().sum::<f64>() / nums.len() as f64))
            }
        }
        "min" => {
            args.iter().cloned().min_by(|a, b| datum_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal))
                .ok_or_else(|| HelionError::Internal("MIN of empty set".into()))
        }
        "max" => {
            args.iter().cloned().max_by(|a, b| datum_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal))
                .ok_or_else(|| HelionError::Internal("MAX of empty set".into()))
        }
        "lower" | "lcase" => {
            let s = args.first().map(datum_to_string).unwrap_or_default();
            Ok(Datum::Text(s.to_lowercase()))
        }
        "upper" | "ucase" => {
            let s = args.first().map(datum_to_string).unwrap_or_default();
            Ok(Datum::Text(s.to_uppercase()))
        }
        "length" | "len" => {
            let s = args.first().map(datum_to_string).unwrap_or_default();
            Ok(Datum::Integer(s.len() as i32))
        }
        "coalesce" => {
            Ok(args.iter().find(|a| !a.is_null()).cloned().unwrap_or(Datum::Null))
        }
        "ifnull" => {
            Ok(args.first().map(|a| if a.is_null() { args.get(1).cloned().unwrap_or(Datum::Null) } else { a.clone() })
                .unwrap_or(Datum::Null))
        }
        "abs" => {
            let v = args.first().ok_or_else(|| HelionError::Internal("ABS requires 1 argument".into()))?;
            match v {
                Datum::Integer(i) => Ok(Datum::Integer(i.abs())),
                Datum::BigInt(i) => Ok(Datum::BigInt(i.abs())),
                Datum::Double(f) => Ok(Datum::Double(f.abs())),
                _ => Err(HelionError::TypeMismatch {
                    expected: "numeric".to_string(),
                    actual: v.data_type().to_string(),
                }),
            }
        }
        "round" => {
            let v = args.first().ok_or_else(|| HelionError::Internal("ROUND requires 1 argument".into()))?;
            let decimals = args.get(1).map(|d| match d {
                Datum::Integer(i) => *i,
                _ => 0,
            }).unwrap_or(0);
            match v {
                Datum::Double(f) => {
                    let multiplier = 10f64.powi(decimals);
                    Ok(Datum::Double((f * multiplier).round() / multiplier))
                }
                _ => Ok(v.clone()),
            }
        }
        _ => Err(HelionError::Internal(format!("Unknown function: {}", name))),
    }
}

/// Convert a LIKE pattern to a simple wildcard matcher pattern.
fn like_to_regex(pattern: &str) -> String {
    let mut regex = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '%' => regex.push_str(".*"),
            '_' => regex.push('.'),
            '.' | '*' | '+' | '?' | '^' | '$' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' => {
                regex.push('\\');
                regex.push(c);
            }
            other => regex.push(other),
        }
    }
    regex
}

/// Simple wildcard matching for LIKE patterns.
fn regex_lite(pattern: &str, s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let pat_chars: Vec<char> = pattern.chars().collect();
    simple_match(&pat_chars, &chars, 0, 0)
}

fn simple_match(pat: &[char], s: &[char], pi: usize, si: usize) -> bool {
    if pi >= pat.len() {
        return si >= s.len();
    }
    if si >= s.len() {
        // Allow trailing .* to match empty
        return pi + 1 < pat.len() 
            && pat[pi] == '.' 
            && pat[pi + 1] == '*'
            && simple_match(pat, s, pi + 2, si);
    }

    match pat[pi] {
        '.' if pi + 1 < pat.len() && pat[pi + 1] == '*' => {
            // .* matches any sequence (including empty)
            simple_match(pat, s, pi + 2, si) || simple_match(pat, s, pi, si + 1)
        }
        '.' => {
            // . matches any single char (LIKE _)
            simple_match(pat, s, pi + 1, si + 1)
        }
        '\\' if pi + 1 < pat.len() => {
            if pat[pi + 1] == s[si] {
                simple_match(pat, s, pi + 2, si + 1)
            } else {
                false
            }
        }
        c if c == s[si] => simple_match(pat, s, pi + 1, si + 1),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::DataType;
    use crate::sql::parser::*;

    fn test_columns() -> Vec<ColumnMeta> {
        vec![
            ColumnMeta::new("id", DataType::Integer),
            ColumnMeta::new("name", DataType::Text),
            ColumnMeta::new("age", DataType::Integer),
        ]
    }

    #[test]
    fn test_evaluate_literal() {
        let expr = Expression::Literal(Datum::Integer(42));
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Integer(42));
    }

    #[test]
    fn test_evaluate_column() {
        let expr = Expression::Column("name".to_string());
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Text("Alice".into()));
    }

    #[test]
    fn test_evaluate_binary_eq() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Column("id".to_string())),
            op: BinaryOperator::Eq,
            right: Box::new(Expression::Literal(Datum::Integer(1))),
        };
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_binary_gt() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Column("age".to_string())),
            op: BinaryOperator::Gt,
            right: Box::new(Expression::Literal(Datum::Integer(18))),
        };
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_and() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::BinaryOp {
                left: Box::new(Expression::Column("age".to_string())),
                op: BinaryOperator::Gt,
                right: Box::new(Expression::Literal(Datum::Integer(18))),
            }),
            op: BinaryOperator::And,
            right: Box::new(Expression::BinaryOp {
                left: Box::new(Expression::Column("name".to_string())),
                op: BinaryOperator::Eq,
                right: Box::new(Expression::Literal(Datum::Text("Alice".into()))),
            }),
        };
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_is_null() {
        let expr = Expression::IsNull(Box::new(Expression::Column("name".to_string())));
        let row = [Datum::Integer(1), Datum::Null, Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_like() {
        let expr = Expression::Like {
            expr: Box::new(Expression::Column("name".to_string())),
            pattern: "A%".to_string(),
        };
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_like_to_regex() {
        assert_eq!(like_to_regex("A%"), "A.*");
        assert_eq!(like_to_regex("_"), ".");
        assert_eq!(like_to_regex("Hello"), "Hello");
    }

    #[test]
    fn test_function_lower() {
        let result = evaluate_function("lower", &[Datum::Text("HELLO".into())]).unwrap();
        assert_eq!(result, Datum::Text("hello".into()));
    }

    #[test]
    fn test_function_coalesce() {
        let result = evaluate_function("coalesce", &[Datum::Null, Datum::Integer(42)]).unwrap();
        assert_eq!(result, Datum::Integer(42));
    }

    #[test]
    fn test_function_count() {
        let result = evaluate_function("count", &[Datum::Integer(1), Datum::Integer(2)]).unwrap();
        assert_eq!(result, Datum::BigInt(2));
    }

    #[test]
    fn test_evaluate_in() {
        let expr = Expression::In {
            expr: Box::new(Expression::Column("age".to_string())),
            list: vec![Datum::Integer(20), Datum::Integer(30), Datum::Integer(40)],
        };
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_between() {
        let expr = Expression::Between {
            expr: Box::new(Expression::Column("age".to_string())),
            low: Box::new(Expression::Literal(Datum::Integer(20))),
            high: Box::new(Expression::Literal(Datum::Integer(40))),
        };
        let row = [Datum::Integer(1), Datum::Text("Alice".into()), Datum::Integer(30)];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_column_not_found() {
        let expr = Expression::Column("nonexistent".to_string());
        let result = evaluate(&expr, &[], &test_columns());
        assert!(result.is_err());
    }
}
