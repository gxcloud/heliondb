use std::cmp::Ordering;

use crate::error::{HelionError, Result};
use crate::sql::parser::{BinaryOperator, Expression, UnaryOperator};
use crate::storage::types::{uuidv7_bytes, ColumnMeta, Datum};

/// Evaluate an expression against a row of data.
pub fn evaluate(expr: &Expression, row: &[Datum], columns: &[ColumnMeta]) -> Result<Datum> {
    match expr {
        Expression::Literal(d) => Ok(d.clone()),
        Expression::Column(name) => {
            let idx = columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(name))
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
                BinaryOperator::Lt => Ok(Datum::Boolean(
                    compare_datums(&left_val, &right_val) == Some(Ordering::Less),
                )),
                BinaryOperator::Le => Ok(Datum::Boolean(
                    compare_datums(&left_val, &right_val)
                        .map(|o| o != Ordering::Greater)
                        .unwrap_or(false),
                )),
                BinaryOperator::Gt => Ok(Datum::Boolean(
                    compare_datums(&left_val, &right_val) == Some(Ordering::Greater),
                )),
                BinaryOperator::Ge => Ok(Datum::Boolean(
                    compare_datums(&left_val, &right_val)
                        .map(|o| o != Ordering::Less)
                        .unwrap_or(false),
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
        Expression::Between {
            expr: inner,
            low,
            high,
        } => {
            let val = evaluate(inner, row, columns)?;
            let low_val = evaluate(low, row, columns)?;
            let high_val = evaluate(high, row, columns)?;
            if val.is_null() || low_val.is_null() || high_val.is_null() {
                return Ok(Datum::Null);
            }
            let ge_low = compare_datums(&val, &low_val)
                .map(|o| o != std::cmp::Ordering::Less)
                .unwrap_or(false);
            let le_high = compare_datums(&val, &high_val)
                .map(|o| o != std::cmp::Ordering::Greater)
                .unwrap_or(false);
            Ok(Datum::Boolean(ge_low && le_high))
        }
        Expression::Like {
            expr: inner,
            pattern,
        } => {
            let val = evaluate(inner, row, columns)?;
            let s = datum_to_string(&val);
            // Simple LIKE: % matches any sequence, _ matches single char
            let regex_pattern = like_to_regex(pattern);
            let re = regex_lite(&regex_pattern, &s);
            Ok(Datum::Boolean(re))
        }
        Expression::Function { name, args } => {
            let evaluated_args: Result<Vec<Datum>> =
                args.iter().map(|a| evaluate(a, row, columns)).collect();
            let args = evaluated_args?;
            evaluate_function(name, &args)
        }
    }
}

fn datum_eq(a: &Datum, b: &Datum) -> bool {
    compare_datums(a, b) == Some(Ordering::Equal)
}

pub fn compare_datums(a: &Datum, b: &Datum) -> Option<Ordering> {
    match (a, b) {
        (Datum::Null, _) | (_, Datum::Null) => None,
        (Datum::SmallInt(ai), Datum::SmallInt(bi)) => Some(ai.cmp(bi)),
        (Datum::UnsignedSmallInt(ai), Datum::UnsignedSmallInt(bi)) => Some(ai.cmp(bi)),
        (Datum::Integer(ai), Datum::Integer(bi)) => Some(ai.cmp(bi)),
        (Datum::UnsignedInteger(ai), Datum::UnsignedInteger(bi)) => Some(ai.cmp(bi)),
        (Datum::BigInt(ai), Datum::BigInt(bi)) => Some(ai.cmp(bi)),
        (Datum::UnsignedBigInt(ai), Datum::UnsignedBigInt(bi)) => Some(ai.cmp(bi)),
        (Datum::Real(af), Datum::Real(bf)) => af.partial_cmp(bf),
        (Datum::Double(af), Datum::Double(bf)) => af.partial_cmp(bf),
        (Datum::Real(af), Datum::Double(bf)) => (*af as f64).partial_cmp(bf),
        (Datum::Double(af), Datum::Real(bf)) => af.partial_cmp(&(*bf as f64)),
        (Datum::Text(as_), Datum::Text(bs_)) => Some(as_.cmp(bs_)),
        (Datum::VarChar(as_), Datum::Text(bs_)) => Some(as_.cmp(bs_)),
        (Datum::Text(as_), Datum::VarChar(bs_)) => Some(as_.cmp(bs_)),
        (Datum::Boolean(ab), Datum::Boolean(bb)) => Some(ab.cmp(bb)),
        (Datum::Uuid(a), Datum::Uuid(b)) => Some(a.as_bytes().cmp(b.as_bytes())),
        (Datum::UuidV7(a), Datum::UuidV7(b)) => Some(a.cmp(b)),
        (a, b) if is_numeric(a) && is_numeric(b) => compare_numeric(a, b),
        _ => None,
    }
}

fn datum_to_bool(d: &Datum) -> bool {
    match d {
        Datum::Boolean(b) => *b,
        Datum::Null => false,
        Datum::SmallInt(i) => *i != 0,
        Datum::UnsignedSmallInt(i) => *i != 0,
        Datum::Integer(i) => *i != 0,
        Datum::UnsignedInteger(i) => *i != 0,
        Datum::BigInt(i) => *i != 0,
        Datum::UnsignedBigInt(i) => *i != 0,
        Datum::Real(f) => *f != 0.0,
        Datum::Double(f) => *f != 0.0,
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
where
    F: Fn(f64, f64) -> f64,
{
    if !is_numeric(a) || !is_numeric(b) {
        return Err(HelionError::TypeMismatch {
            expected: a.data_type().to_string(),
            actual: b.data_type().to_string(),
        });
    }

    let result = op(numeric_to_f64(a), numeric_to_f64(b));
    match (a, b) {
        (Datum::SmallInt(_), Datum::SmallInt(_)) => Ok(Datum::SmallInt(result as i16)),
        (Datum::UnsignedSmallInt(_), Datum::UnsignedSmallInt(_)) => {
            Ok(Datum::UnsignedSmallInt(result as u16))
        }
        (Datum::Integer(_), Datum::Integer(_)) => Ok(Datum::Integer(result as i32)),
        (Datum::UnsignedInteger(_), Datum::UnsignedInteger(_)) => {
            Ok(Datum::UnsignedInteger(result as u32))
        }
        (Datum::BigInt(_), Datum::BigInt(_)) => Ok(Datum::BigInt(result as i64)),
        (Datum::UnsignedBigInt(_), Datum::UnsignedBigInt(_)) => {
            Ok(Datum::UnsignedBigInt(result as u64))
        }
        (Datum::Real(_), Datum::Real(_)) | (Datum::Double(_), Datum::Double(_)) => {
            Ok(Datum::Double(result))
        }
        _ if result.is_sign_negative() => Ok(Datum::BigInt(result as i64)),
        _ => Ok(Datum::UnsignedBigInt(result as u64)),
    }
}

fn negate_datum(d: &Datum) -> Result<Datum> {
    match d {
        Datum::SmallInt(i) => Ok(Datum::SmallInt(-i)),
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
                Ok(Datum::BigInt(
                    args.iter().filter(|a| !a.is_null()).count() as i64
                ))
            }
        }
        "uuidv7" | "uuid_v7" => {
            if !args.is_empty() {
                return Err(HelionError::Internal("UUIDV7 takes no arguments".into()));
            }
            Ok(Datum::UuidV7(uuidv7_bytes()))
        }
        "sum" => {
            let total: f64 = args
                .iter()
                .filter_map(|a| match a {
                    Datum::SmallInt(i) => Some(*i as f64),
                    Datum::UnsignedSmallInt(i) => Some(*i as f64),
                    Datum::Integer(i) => Some(*i as f64),
                    Datum::UnsignedInteger(i) => Some(*i as f64),
                    Datum::BigInt(i) => Some(*i as f64),
                    Datum::UnsignedBigInt(i) => Some(*i as f64),
                    Datum::Real(f) => Some(*f as f64),
                    Datum::Double(f) => Some(*f),
                    _ => None,
                })
                .sum();
            Ok(Datum::Double(total))
        }
        "avg" => {
            let nums: Vec<f64> = args
                .iter()
                .filter_map(|a| match a {
                    Datum::SmallInt(i) => Some(*i as f64),
                    Datum::UnsignedSmallInt(i) => Some(*i as f64),
                    Datum::Integer(i) => Some(*i as f64),
                    Datum::UnsignedInteger(i) => Some(*i as f64),
                    Datum::BigInt(i) => Some(*i as f64),
                    Datum::UnsignedBigInt(i) => Some(*i as f64),
                    Datum::Real(f) => Some(*f as f64),
                    Datum::Double(f) => Some(*f),
                    _ => None,
                })
                .collect();
            if nums.is_empty() {
                Ok(Datum::Null)
            } else {
                Ok(Datum::Double(nums.iter().sum::<f64>() / nums.len() as f64))
            }
        }
        "min" => args
            .iter()
            .cloned()
            .min_by(|a, b| compare_datums(a, b).unwrap_or(std::cmp::Ordering::Equal))
            .ok_or_else(|| HelionError::Internal("MIN of empty set".into())),
        "max" => args
            .iter()
            .cloned()
            .max_by(|a, b| compare_datums(a, b).unwrap_or(std::cmp::Ordering::Equal))
            .ok_or_else(|| HelionError::Internal("MAX of empty set".into())),
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
        "coalesce" => Ok(args
            .iter()
            .find(|a| !a.is_null())
            .cloned()
            .unwrap_or(Datum::Null)),
        "ifnull" => Ok(args
            .first()
            .map(|a| {
                if a.is_null() {
                    args.get(1).cloned().unwrap_or(Datum::Null)
                } else {
                    a.clone()
                }
            })
            .unwrap_or(Datum::Null)),
        "abs" => {
            let v = args
                .first()
                .ok_or_else(|| HelionError::Internal("ABS requires 1 argument".into()))?;
            match v {
                Datum::SmallInt(i) => Ok(Datum::SmallInt(i.abs())),
                Datum::Integer(i) => Ok(Datum::Integer(i.abs())),
                Datum::BigInt(i) => Ok(Datum::BigInt(i.abs())),
                Datum::UnsignedSmallInt(i) => Ok(Datum::UnsignedSmallInt(*i)),
                Datum::UnsignedInteger(i) => Ok(Datum::UnsignedInteger(*i)),
                Datum::UnsignedBigInt(i) => Ok(Datum::UnsignedBigInt(*i)),
                Datum::Real(f) => Ok(Datum::Double(f.abs() as f64)),
                Datum::Double(f) => Ok(Datum::Double(f.abs())),
                _ => Err(HelionError::TypeMismatch {
                    expected: "numeric".to_string(),
                    actual: v.data_type().to_string(),
                }),
            }
        }
        "round" => {
            let v = args
                .first()
                .ok_or_else(|| HelionError::Internal("ROUND requires 1 argument".into()))?;
            let decimals = args
                .get(1)
                .map(|d| match d {
                    Datum::Integer(i) => *i,
                    _ => 0,
                })
                .unwrap_or(0);
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

fn is_numeric(d: &Datum) -> bool {
    matches!(
        d,
        Datum::SmallInt(_)
            | Datum::UnsignedSmallInt(_)
            | Datum::Integer(_)
            | Datum::UnsignedInteger(_)
            | Datum::BigInt(_)
            | Datum::UnsignedBigInt(_)
            | Datum::Real(_)
            | Datum::Double(_)
    )
}

fn numeric_to_f64(d: &Datum) -> f64 {
    match d {
        Datum::SmallInt(i) => *i as f64,
        Datum::UnsignedSmallInt(i) => *i as f64,
        Datum::Integer(i) => *i as f64,
        Datum::UnsignedInteger(i) => *i as f64,
        Datum::BigInt(i) => *i as f64,
        Datum::UnsignedBigInt(i) => *i as f64,
        Datum::Real(f) => *f as f64,
        Datum::Double(f) => *f,
        _ => 0.0,
    }
}

fn compare_numeric(a: &Datum, b: &Datum) -> Option<Ordering> {
    let left = numeric_rank(a)?;
    let right = numeric_rank(b)?;
    match (left, right) {
        (NumericRank::Float(a), NumericRank::Float(b)) => a.partial_cmp(&b),
        (NumericRank::Float(a), other) => a.partial_cmp(&other.as_f64()),
        (other, NumericRank::Float(b)) => other.as_f64().partial_cmp(&b),
        (NumericRank::Signed(a), NumericRank::Signed(b)) => Some(a.cmp(&b)),
        (NumericRank::Unsigned(a), NumericRank::Unsigned(b)) => Some(a.cmp(&b)),
        (NumericRank::Signed(a), NumericRank::Unsigned(b)) => {
            if a < 0 {
                Some(Ordering::Less)
            } else {
                Some((a as u128).cmp(&b))
            }
        }
        (NumericRank::Unsigned(a), NumericRank::Signed(b)) => {
            if b < 0 {
                Some(Ordering::Greater)
            } else {
                Some(a.cmp(&(b as u128)))
            }
        }
    }
}

#[derive(Copy, Clone)]
enum NumericRank {
    Signed(i128),
    Unsigned(u128),
    Float(f64),
}

impl NumericRank {
    fn as_f64(self) -> f64 {
        match self {
            NumericRank::Signed(v) => v as f64,
            NumericRank::Unsigned(v) => v as f64,
            NumericRank::Float(v) => v,
        }
    }
}

fn numeric_rank(d: &Datum) -> Option<NumericRank> {
    match d {
        Datum::SmallInt(v) => Some(NumericRank::Signed(*v as i128)),
        Datum::Integer(v) => Some(NumericRank::Signed(*v as i128)),
        Datum::BigInt(v) => Some(NumericRank::Signed(*v as i128)),
        Datum::UnsignedSmallInt(v) => Some(NumericRank::Unsigned(*v as u128)),
        Datum::UnsignedInteger(v) => Some(NumericRank::Unsigned(*v as u128)),
        Datum::UnsignedBigInt(v) => Some(NumericRank::Unsigned(*v as u128)),
        Datum::Real(v) => Some(NumericRank::Float(*v as f64)),
        Datum::Double(v) => Some(NumericRank::Float(*v)),
        _ => None,
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
        '\\' if pi + 1 < pat.len() => pat[pi + 1] == s[si] && simple_match(pat, s, pi + 2, si + 1),
        c if c == s[si] => simple_match(pat, s, pi + 1, si + 1),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::*;
    use crate::storage::types::DataType;

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
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
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
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
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
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
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
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
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
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
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
    fn test_function_uuidv7() {
        let result = evaluate_function("uuidv7", &[]).unwrap();
        assert!(matches!(result, Datum::UuidV7(_)));
    }

    #[test]
    fn test_compare_unsigned() {
        assert_eq!(
            compare_datums(&Datum::UnsignedInteger(2), &Datum::Integer(3)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            compare_datums(&Datum::UnsignedBigInt(5), &Datum::UnsignedBigInt(5)),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[test]
    fn test_evaluate_in() {
        let expr = Expression::In {
            expr: Box::new(Expression::Column("age".to_string())),
            list: vec![Datum::Integer(20), Datum::Integer(30), Datum::Integer(40)],
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
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
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_column_not_found() {
        let expr = Expression::Column("nonexistent".to_string());
        let result = evaluate(&expr, &[], &test_columns());
        assert!(result.is_err());
    }

    #[test]
    fn test_evaluate_arithmetic_add() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Literal(Datum::Integer(10))),
            op: BinaryOperator::Add,
            right: Box::new(Expression::Literal(Datum::Integer(20))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Integer(30));
    }

    #[test]
    fn test_evaluate_arithmetic_sub() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Literal(Datum::Integer(100))),
            op: BinaryOperator::Sub,
            right: Box::new(Expression::Literal(Datum::Integer(30))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Integer(70));
    }

    #[test]
    fn test_evaluate_arithmetic_mul() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Literal(Datum::Integer(7))),
            op: BinaryOperator::Mul,
            right: Box::new(Expression::Literal(Datum::Integer(6))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Integer(42));
    }

    #[test]
    fn test_evaluate_arithmetic_div() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Literal(Datum::Integer(10))),
            op: BinaryOperator::Div,
            right: Box::new(Expression::Literal(Datum::Integer(3))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Integer(3));
    }

    #[test]
    fn test_evaluate_not() {
        let expr = Expression::UnaryOp {
            op: UnaryOperator::Not,
            expr: Box::new(Expression::Literal(Datum::Boolean(false))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_neg() {
        let expr = Expression::UnaryOp {
            op: UnaryOperator::Neg,
            expr: Box::new(Expression::Literal(Datum::Integer(42))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Integer(-42));
    }

    #[test]
    fn test_evaluate_is_not_null() {
        let expr = Expression::IsNotNull(Box::new(Expression::Column("name".to_string())));
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_in_empty_list() {
        let expr = Expression::In {
            expr: Box::new(Expression::Column("age".to_string())),
            list: vec![],
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(false));
    }

    #[test]
    fn test_evaluate_between_not_matching() {
        let expr = Expression::Between {
            expr: Box::new(Expression::Column("age".to_string())),
            low: Box::new(Expression::Literal(Datum::Integer(40))),
            high: Box::new(Expression::Literal(Datum::Integer(50))),
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(false));
    }

    #[test]
    fn test_evaluate_like_no_wildcard() {
        let expr = Expression::Like {
            expr: Box::new(Expression::Column("name".to_string())),
            pattern: "Alice".to_string(),
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_like_ends_with() {
        let expr = Expression::Like {
            expr: Box::new(Expression::Column("name".to_string())),
            pattern: "%ice".to_string(),
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_like_underscore() {
        let expr = Expression::Like {
            expr: Box::new(Expression::Column("name".to_string())),
            pattern: "A____".to_string(),
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_function_upper() {
        let result = evaluate_function("upper", &[Datum::Text("hello".into())]).unwrap();
        assert_eq!(result, Datum::Text("HELLO".into()));
    }

    #[test]
    fn test_function_length() {
        let result = evaluate_function("length", &[Datum::Text("hello".into())]).unwrap();
        assert_eq!(result, Datum::Integer(5));
    }

    #[test]
    fn test_function_ifnull() {
        let r = evaluate_function("ifnull", &[Datum::Null, Datum::Integer(42)]).unwrap();
        assert_eq!(r, Datum::Integer(42));
        let r = evaluate_function("ifnull", &[Datum::Integer(1), Datum::Integer(2)]).unwrap();
        assert_eq!(r, Datum::Integer(1));
    }

    #[test]
    fn test_function_abs() {
        let r = evaluate_function("abs", &[Datum::Integer(-5)]).unwrap();
        assert_eq!(r, Datum::Integer(5));
    }

    #[test]
    fn test_function_round() {
        let r =
            evaluate_function("round", &[Datum::Double(3.0 + 0.14159), Datum::Integer(2)]).unwrap();
        assert_eq!(r, Datum::Double(3.0 + 0.14));
    }

    #[test]
    fn test_function_unknown() {
        let r = evaluate_function("nonexistent", &[]);
        assert!(r.is_err());
    }

    #[test]
    fn test_evaluate_binary_or() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Literal(Datum::Boolean(false))),
            op: BinaryOperator::Or,
            right: Box::new(Expression::Literal(Datum::Boolean(true))),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }

    #[test]
    fn test_evaluate_null_equals_null() {
        let expr = Expression::BinaryOp {
            left: Box::new(Expression::Literal(Datum::Null)),
            op: BinaryOperator::Eq,
            right: Box::new(Expression::Literal(Datum::Null)),
        };
        let result = evaluate(&expr, &[], &[]).unwrap();
        // NULL = NULL should be NULL (null propagation)
        assert_eq!(result, Datum::Null);
    }

    #[test]
    fn test_evaluate_like_not_matching() {
        let expr = Expression::Like {
            expr: Box::new(Expression::Column("name".to_string())),
            pattern: "B%".to_string(),
        };
        let row = [
            Datum::Integer(1),
            Datum::Text("Alice".into()),
            Datum::Integer(30),
        ];
        let result = evaluate(&expr, &row, &test_columns()).unwrap();
        assert_eq!(result, Datum::Boolean(false));
    }
}
