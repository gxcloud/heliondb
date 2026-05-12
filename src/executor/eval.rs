use crate::storage::types::Datum;

pub fn evaluate_expression(expr: &str, _row: &[Datum]) -> Result<Datum, String> {
    match expr {
        "true" => Ok(Datum::Boolean(true)),
        "false" => Ok(Datum::Boolean(false)),
        _ => Err(format!("Unsupported expression: {}", expr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_true() {
        let result = evaluate_expression("true", &[]).unwrap();
        assert_eq!(result, Datum::Boolean(true));
    }
}
