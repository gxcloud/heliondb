use crate::error::{HelionError, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataType {
    Boolean,
    SmallInt,
    UnsignedSmallInt,
    Integer,
    UnsignedInteger,
    BigInt,
    UnsignedBigInt,
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
    UuidV7,
    Null,
}

impl DataType {
    pub fn name(&self) -> &str {
        match self {
            DataType::Boolean => "BOOLEAN",
            DataType::SmallInt => "SMALLINT",
            DataType::UnsignedSmallInt => "U_SMALLINT",
            DataType::Integer => "INTEGER",
            DataType::UnsignedInteger => "U_INTEGER",
            DataType::BigInt => "BIGINT",
            DataType::UnsignedBigInt => "U_BIGINT",
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
            DataType::UuidV7 => "UUIDV7",
            DataType::Null => "NULL",
        }
    }

    pub fn from_sql(dt: sqlparser::ast::DataType) -> Result<Self> {
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
            SqlType::Custom(name, _) => {
                let raw = name.to_string().to_uppercase();
                match raw.as_str() {
                    "U_SMALLINT" => Ok(DataType::UnsignedSmallInt),
                    "U_INTEGER" => Ok(DataType::UnsignedInteger),
                    "U_BIGINT" => Ok(DataType::UnsignedBigInt),
                    "UUIDV7" => Ok(DataType::UuidV7),
                    _ => Err(HelionError::Internal(format!(
                        "Unsupported SQL type: {:?}",
                        raw
                    ))),
                }
            }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Datum {
    Boolean(bool),
    SmallInt(i16),
    UnsignedSmallInt(u16),
    Integer(i32),
    UnsignedInteger(u32),
    BigInt(i64),
    UnsignedBigInt(u64),
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
    Uuid(Uuid),
    UuidV7([u8; 16]),
    Null,
}

impl PartialOrd for Datum {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Datum {
    fn cmp(&self, other: &Self) -> Ordering {
        use Datum::*;

        fn variant_idx(d: &Datum) -> u8 {
            match d {
                Boolean(_) => 0,
                SmallInt(_) => 1,
                UnsignedSmallInt(_) => 2,
                Integer(_) => 3,
                UnsignedInteger(_) => 4,
                BigInt(_) => 5,
                UnsignedBigInt(_) => 6,
                Real(_) => 7,
                Double(_) => 8,
                VarChar(_) => 9,
                Char(_) => 10,
                Text(_) => 11,
                Binary(_) => 12,
                Date(_) => 13,
                Time(_) => 14,
                Timestamp(_) => 15,
                TimestampTz(_) => 16,
                Uuid(_) => 17,
                UuidV7(_) => 18,
                Null => 19,
            }
        }

        let vi = variant_idx(self);
        let vo = variant_idx(other);
        if vi != vo {
            return vi.cmp(&vo);
        }

        match (self, other) {
            (Boolean(a), Boolean(b)) => a.cmp(b),
            (SmallInt(a), SmallInt(b)) => a.cmp(b),
            (UnsignedSmallInt(a), UnsignedSmallInt(b)) => a.cmp(b),
            (Integer(a), Integer(b)) => a.cmp(b),
            (UnsignedInteger(a), UnsignedInteger(b)) => a.cmp(b),
            (BigInt(a), BigInt(b)) => a.cmp(b),
            (UnsignedBigInt(a), UnsignedBigInt(b)) => a.cmp(b),
            (Real(a), Real(b)) => a.total_cmp(b),
            (Double(a), Double(b)) => a.total_cmp(b),
            (VarChar(a), VarChar(b)) => a.cmp(b),
            (Char(a), Char(b)) => a.cmp(b),
            (Text(a), Text(b)) => a.cmp(b),
            (Binary(a), Binary(b)) => a.cmp(b),
            (Date(a), Date(b)) => a.cmp(b),
            (Time(a), Time(b)) => a.cmp(b),
            (Timestamp(a), Timestamp(b)) => a.cmp(b),
            (TimestampTz(a), TimestampTz(b)) => a.cmp(b),
            (Uuid(a), Uuid(b)) => a.cmp(b),
            (UuidV7(a), UuidV7(b)) => a.cmp(b),
            (Null, Null) => Ordering::Equal,
            _ => unreachable!(),
        }
    }
}

impl Datum {
    pub fn data_type(&self) -> DataType {
        match self {
            Datum::Boolean(_) => DataType::Boolean,
            Datum::SmallInt(_) => DataType::SmallInt,
            Datum::UnsignedSmallInt(_) => DataType::UnsignedSmallInt,
            Datum::Integer(_) => DataType::Integer,
            Datum::UnsignedInteger(_) => DataType::UnsignedInteger,
            Datum::BigInt(_) => DataType::BigInt,
            Datum::UnsignedBigInt(_) => DataType::UnsignedBigInt,
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
            Datum::UuidV7(_) => DataType::UuidV7,
            Datum::Null => DataType::Null,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Datum::Null => "NULL".to_string(),
            Datum::Boolean(b) => b.to_string(),
            Datum::SmallInt(i) => i.to_string(),
            Datum::UnsignedSmallInt(i) => i.to_string(),
            Datum::Integer(i) => i.to_string(),
            Datum::UnsignedInteger(i) => i.to_string(),
            Datum::BigInt(i) => i.to_string(),
            Datum::UnsignedBigInt(i) => i.to_string(),
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
            Datum::UuidV7(bytes) => Uuid::from_bytes(*bytes).to_string(),
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
    fn from(b: bool) -> Self {
        Datum::Boolean(b)
    }
}

impl From<i32> for Datum {
    fn from(i: i32) -> Self {
        Datum::Integer(i)
    }
}

impl From<i64> for Datum {
    fn from(i: i64) -> Self {
        Datum::BigInt(i)
    }
}

impl From<f64> for Datum {
    fn from(f: f64) -> Self {
        Datum::Double(f)
    }
}

impl From<String> for Datum {
    fn from(s: String) -> Self {
        Datum::Text(s)
    }
}

impl From<&str> for Datum {
    fn from(s: &str) -> Self {
        Datum::Text(s.to_string())
    }
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

impl From<u16> for Datum {
    fn from(i: u16) -> Self {
        Datum::UnsignedSmallInt(i)
    }
}

impl From<u32> for Datum {
    fn from(i: u32) -> Self {
        Datum::UnsignedInteger(i)
    }
}

impl From<u64> for Datum {
    fn from(i: u64) -> Self {
        Datum::UnsignedBigInt(i)
    }
}

pub fn uuidv7_bytes() -> [u8; 16] {
    Uuid::now_v7().into_bytes()
}

pub fn coerce_datum(value: &Datum, target: &DataType) -> Result<Datum> {
    use Datum::*;

    match (value, target) {
        (Null, _) => Ok(Null),
        (Boolean(v), DataType::Boolean) => Ok(Boolean(*v)),
        (SmallInt(v), DataType::SmallInt) => Ok(SmallInt(*v)),
        (SmallInt(v), DataType::Integer) => Ok(Integer(*v as i32)),
        (SmallInt(v), DataType::BigInt) => Ok(BigInt(*v as i64)),
        (SmallInt(v), DataType::UnsignedSmallInt) if *v >= 0 => Ok(UnsignedSmallInt(*v as u16)),
        (SmallInt(v), DataType::UnsignedInteger) if *v >= 0 => Ok(UnsignedInteger(*v as u32)),
        (SmallInt(v), DataType::UnsignedBigInt) if *v >= 0 => Ok(UnsignedBigInt(*v as u64)),

        (UnsignedSmallInt(v), DataType::UnsignedSmallInt) => Ok(UnsignedSmallInt(*v)),
        (UnsignedSmallInt(v), DataType::UnsignedInteger) => Ok(UnsignedInteger(*v as u32)),
        (UnsignedSmallInt(v), DataType::UnsignedBigInt) => Ok(UnsignedBigInt(*v as u64)),
        (UnsignedSmallInt(v), DataType::Integer) => Ok(Integer(*v as i32)),
        (UnsignedSmallInt(v), DataType::BigInt) => Ok(BigInt(*v as i64)),

        (Integer(v), DataType::Integer) => Ok(Integer(*v)),
        (Integer(v), DataType::BigInt) => Ok(BigInt(*v as i64)),
        (Integer(v), DataType::UnsignedInteger) if *v >= 0 => Ok(UnsignedInteger(*v as u32)),
        (Integer(v), DataType::UnsignedBigInt) if *v >= 0 => Ok(UnsignedBigInt(*v as u64)),

        (UnsignedInteger(v), DataType::UnsignedInteger) => Ok(UnsignedInteger(*v)),
        (UnsignedInteger(v), DataType::UnsignedBigInt) => Ok(UnsignedBigInt(*v as u64)),
        (UnsignedInteger(v), DataType::BigInt) => Ok(BigInt(*v as i64)),
        (UnsignedInteger(v), DataType::Integer) if *v <= i32::MAX as u32 => Ok(Integer(*v as i32)),

        (BigInt(v), DataType::BigInt) => Ok(BigInt(*v)),
        (BigInt(v), DataType::UnsignedBigInt) if *v >= 0 => Ok(UnsignedBigInt(*v as u64)),
        (BigInt(v), DataType::Integer) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
            Ok(Integer(*v as i32))
        }

        (UnsignedBigInt(v), DataType::UnsignedBigInt) => Ok(UnsignedBigInt(*v)),
        (UnsignedBigInt(v), DataType::BigInt) if *v <= i64::MAX as u64 => Ok(BigInt(*v as i64)),
        (UnsignedBigInt(v), DataType::Integer) if *v <= i32::MAX as u64 => Ok(Integer(*v as i32)),

        (Real(v), DataType::Real) => Ok(Real(*v)),
        (Double(v), DataType::Double) => Ok(Double(*v)),

        (Text(v), DataType::Text) => Ok(Text(v.clone())),
        (Text(v), DataType::VarChar(_)) => Ok(VarChar(v.clone())),
        (Text(v), DataType::Char(_)) => Ok(Char(v.clone())),
        (VarChar(v), DataType::Text) => Ok(Text(v.clone())),
        (VarChar(v), DataType::VarChar(_)) => Ok(VarChar(v.clone())),
        (VarChar(v), DataType::Char(_)) => Ok(Char(v.clone())),
        (Char(v), DataType::Text) => Ok(Text(v.clone())),
        (Char(v), DataType::VarChar(_)) => Ok(VarChar(v.clone())),
        (Char(v), DataType::Char(_)) => Ok(Char(v.clone())),

        (Binary(v), DataType::Binary) => Ok(Binary(v.clone())),
        (Date(v), DataType::Date) => Ok(Date(*v)),
        (Time(v), DataType::Time) => Ok(Time(*v)),
        (Timestamp(v), DataType::Timestamp) => Ok(Timestamp(*v)),
        (TimestampTz(v), DataType::TimestampTz) => Ok(TimestampTz(*v)),
        (Uuid(v), DataType::Uuid) => Ok(Uuid(*v)),
        (UuidV7(v), DataType::UuidV7) => Ok(UuidV7(*v)),
        (Uuid(v), DataType::UuidV7) if v.get_version_num() == 7 => {
            Ok(UuidV7(v.as_bytes().to_owned()))
        }

        _ => Err(HelionError::TypeMismatch {
            expected: target.to_string(),
            actual: value.data_type().to_string(),
        }),
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
        assert_eq!(
            Datum::from(3.0f64 + 0.14f64),
            Datum::Double(3.0f64 + 0.14f64)
        );
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
    fn test_data_type_from_sql_custom_unsigned() {
        let ty = sqlparser::ast::DataType::Custom(
            sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                sqlparser::ast::Ident::new("U_INTEGER"),
            )]),
            vec![],
        );
        assert_eq!(DataType::from_sql(ty).unwrap(), DataType::UnsignedInteger);
    }

    #[test]
    fn test_data_type_from_sql_uuidv7() {
        let ty = sqlparser::ast::DataType::Custom(
            sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                sqlparser::ast::Ident::new("UUIDV7"),
            )]),
            vec![],
        );
        assert_eq!(DataType::from_sql(ty).unwrap(), DataType::UuidV7);
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
        assert!(!Datum::Text("".to_string()).is_null());
    }

    #[test]
    fn test_data_type_eq() {
        assert_eq!(DataType::Integer, DataType::Integer);
        assert_ne!(DataType::Integer, DataType::Text);
        assert_eq!(DataType::VarChar(Some(100)), DataType::VarChar(Some(100)));
    }
}
