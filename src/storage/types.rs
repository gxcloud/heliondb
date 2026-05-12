use serde::{Deserialize, Serialize};
use std::fmt;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataType {
    Boolean,
    SmallInt,
    Integer,
    BigInt,
    Real,
    Double,
    VarChar(Option<usize>),
    Char(Option<usize>),
    Text,
    Binary,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Uuid,
    Null,
}

impl DataType {
    pub fn name(&self) -> &str {
        match self {
            DataType::Boolean => "BOOLEAN",
            DataType::SmallInt => "SMALLINT",
            DataType::Integer => "INTEGER",
            DataType::BigInt => "BIGINT",
            DataType::Real => "REAL",
            DataType::Double => "DOUBLE",
            DataType::VarChar(_) => "VARCHAR",
            DataType::Char(_) => "CHAR",
            DataType::Text => "TEXT",
            DataType::Binary => "BINARY",
            DataType::Date => "DATE",
            DataType::Time => "TIME",
            DataType::Timestamp => "TIMESTAMP",
            DataType::TimestampTz => "TIMESTAMPTZ",
            DataType::Uuid => "UUID",
            DataType::Null => "NULL",
        }
    }

    pub fn from_sql(dt: sqlparser::ast::DataType) -> Result<Self, crate::error::HelionError> {
        use crate::error::HelionError;
        use sqlparser::ast::{CharacterLength, DataType as SqlType};

        let char_len = |cl: CharacterLength| -> Option<usize> {
            match cl {
                CharacterLength::IntegerLength { length, .. } => Some(length as usize),
                CharacterLength::Max => None,
            }
        };

        match dt {
            SqlType::Boolean => Ok(DataType::Boolean),
            SqlType::SmallInt(_) => Ok(DataType::SmallInt),
            SqlType::Int(_) | SqlType::Integer(_) => Ok(DataType::Integer),
            SqlType::BigInt(_) => Ok(DataType::BigInt),
            SqlType::Real => Ok(DataType::Real),
            SqlType::Double(_) | SqlType::Float(_) => Ok(DataType::Double),
            SqlType::Varchar(Some(cl)) => Ok(DataType::VarChar(char_len(cl))),
            SqlType::Varchar(None) => Ok(DataType::VarChar(None)),
            SqlType::Char(Some(cl)) => Ok(DataType::Char(char_len(cl))),
            SqlType::Char(None) => Ok(DataType::Char(None)),
            SqlType::Text => Ok(DataType::Text),
            SqlType::Date => Ok(DataType::Date),
            SqlType::Time(..) => Ok(DataType::Time),
            SqlType::Timestamp(_, _) => Ok(DataType::Timestamp),
            SqlType::Uuid => Ok(DataType::Uuid),
            other => Err(HelionError::Internal(format!(
                "Unsupported SQL type: {:?}",
                other
            ))),
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd)]
pub enum Datum {
    Boolean(bool),
    SmallInt(i16),
    Integer(i32),
    BigInt(i64),
    Real(f32),
    Double(f64),
    VarChar(String),
    Char(String),
    Text(String),
    Binary(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    Timestamp(NaiveDateTime),
    TimestampTz(i64),
    Uuid(uuid::Uuid),
    Null,
}

impl Datum {
    pub fn data_type(&self) -> DataType {
        match self {
            Datum::Boolean(_) => DataType::Boolean,
            Datum::SmallInt(_) => DataType::SmallInt,
            Datum::Integer(_) => DataType::Integer,
            Datum::BigInt(_) => DataType::BigInt,
            Datum::Real(_) => DataType::Real,
            Datum::Double(_) => DataType::Double,
            Datum::VarChar(_) => DataType::VarChar(None),
            Datum::Char(_) => DataType::Char(None),
            Datum::Text(_) => DataType::Text,
            Datum::Binary(_) => DataType::Binary,
            Datum::Date(_) => DataType::Date,
            Datum::Time(_) => DataType::Time,
            Datum::Timestamp(_) => DataType::Timestamp,
            Datum::TimestampTz(_) => DataType::TimestampTz,
            Datum::Uuid(_) => DataType::Uuid,
            Datum::Null => DataType::Null,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Datum::Null => "NULL".to_string(),
            Datum::Boolean(b) => b.to_string(),
            Datum::SmallInt(i) => i.to_string(),
            Datum::Integer(i) => i.to_string(),
            Datum::BigInt(i) => i.to_string(),
            Datum::Real(f) => f.to_string(),
            Datum::Double(f) => f.to_string(),
            Datum::VarChar(s) | Datum::Char(s) | Datum::Text(s) => s.clone(),
            Datum::Binary(b) => hex::encode(b),
            Datum::Date(d) => d.to_string(),
            Datum::Time(t) => t.to_string(),
            Datum::Timestamp(ts) => ts.to_string(),
            Datum::TimestampTz(ts) => chrono::DateTime::from_timestamp(*ts, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| ts.to_string()),
            Datum::Uuid(u) => u.to_string(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Datum::Null)
    }
}

impl fmt::Display for Datum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}

impl From<bool> for Datum {
    fn from(b: bool) -> Self { Datum::Boolean(b) }
}

impl From<i32> for Datum {
    fn from(i: i32) -> Self { Datum::Integer(i) }
}

impl From<i64> for Datum {
    fn from(i: i64) -> Self { Datum::BigInt(i) }
}

impl From<f64> for Datum {
    fn from(f: f64) -> Self { Datum::Double(f) }
}

impl From<String> for Datum {
    fn from(s: String) -> Self { Datum::Text(s) }
}

impl From<&str> for Datum {
    fn from(s: &str) -> Self { Datum::Text(s.to_string()) }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColumnMeta {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_unique: bool,
    pub default: Option<Datum>,
}

impl ColumnMeta {
    pub fn new(name: &str, data_type: DataType) -> Self {
        ColumnMeta {
            name: name.to_string(),
            data_type,
            nullable: true,
            is_primary_key: false,
            is_unique: false,
            default: None,
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn primary_key(mut self) -> Self {
        self.nullable = false;
        self.is_primary_key = true;
        self.is_unique = true;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Datum>,
}

impl Row {
    pub fn new(values: Vec<Datum>) -> Self {
        Row { values }
    }

    pub fn get(&self, idx: usize) -> Option<&Datum> {
        self.values.get(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_type_names() {
        assert_eq!(DataType::Integer.name(), "INTEGER");
        assert_eq!(DataType::Boolean.name(), "BOOLEAN");
        assert_eq!(DataType::Text.name(), "TEXT");
        assert_eq!(DataType::VarChar(None).name(), "VARCHAR");
    }

    #[test]
    fn test_datum_display() {
        assert_eq!(Datum::Integer(42).display(), "42");
        assert_eq!(Datum::Text("hello".into()).display(), "hello");
        assert_eq!(Datum::Null.display(), "NULL");
        assert_eq!(Datum::Boolean(true).display(), "true");
    }

    #[test]
    fn test_datum_data_type() {
        assert_eq!(Datum::Integer(5).data_type(), DataType::Integer);
        assert_eq!(Datum::Null.data_type(), DataType::Null);
    }

    #[test]
    fn test_datum_from_traits() {
        assert_eq!(Datum::from(true), Datum::Boolean(true));
        assert_eq!(Datum::from(42i32), Datum::Integer(42));
        assert_eq!(Datum::from("hello"), Datum::Text("hello".to_string()));
    }

    #[test]
    fn test_column_meta_primary_key() {
        let col = ColumnMeta::new("id", DataType::Integer).primary_key();
        assert!(col.is_primary_key);
        assert!(col.is_unique);
        assert!(!col.nullable);
    }

    #[test]
    fn test_row_get() {
        let row = Row::new(vec![Datum::Integer(1), Datum::Text("a".into())]);
        assert_eq!(row.get(0), Some(&Datum::Integer(1)));
        assert_eq!(row.get(2), None);
    }

    #[test]
    fn test_datum_edge_cases() {
        assert_eq!(Datum::Integer(i32::MAX).display(), "2147483647");
        assert_eq!(Datum::Integer(i32::MIN).display(), "-2147483648");
        assert_eq!(Datum::BigInt(i64::MAX).display(), "9223372036854775807");
        assert_eq!(Datum::Double(f64::INFINITY).display(), "inf");
        assert_eq!(Datum::Double(f64::NEG_INFINITY).display(), "-inf");
    }

    #[test]
    fn test_datum_from_i64() {
        assert_eq!(Datum::from(42i64), Datum::BigInt(42));
        assert_eq!(Datum::from(-1i64), Datum::BigInt(-1));
    }

    #[test]
    fn test_datum_from_f64() {
        assert_eq!(Datum::from(3.0f64 + 0.14f64), Datum::Double(3.0f64 + 0.14f64));
        assert_eq!(Datum::from(-0.0f64), Datum::Double(-0.0));
    }

    #[test]
    fn test_column_meta_defaults() {
        let col = ColumnMeta::new("col", DataType::Text);
        assert!(col.nullable);
        assert!(!col.is_primary_key);
        assert!(!col.is_unique);
        assert!(col.default.is_none());
    }

    #[test]
    fn test_column_meta_not_null() {
        let col = ColumnMeta::new("col", DataType::Integer).not_null();
        assert!(!col.nullable);
    }

    #[test]
    fn test_data_type_from_sql_supported() {
        let result = DataType::from_sql(sqlparser::ast::DataType::Text);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), DataType::Text);
    }

    #[test]
    fn test_data_type_from_sql_null() {
        // The Null type might not parse from SQL but should still exist in our enum
        assert_eq!(DataType::Null.name(), "NULL");
        assert!(Datum::Null.is_null());
    }

    #[test]
    fn test_is_null() {
        assert!(Datum::Null.is_null());
        assert!(!Datum::Integer(0).is_null());
        assert!(!Datum::Boolean(false).is_null());
        assert!(!Datum::Text("".into()).is_null());
    }

    #[test]
    fn test_data_type_eq() {
        assert_eq!(DataType::Integer, DataType::Integer);
        assert_ne!(DataType::Integer, DataType::Text);
        assert_eq!(DataType::VarChar(Some(100)), DataType::VarChar(Some(100)));
    }
}
