use gqlrs::Name;
use gqlrs_value::ConstValue;
use indexmap::IndexMap;

use super::array::parse_pg_array;
use super::binary_format::{
    format_bytea, format_inet, format_interval, format_macaddr, format_range, format_timetz,
    format_uuid, parse_pg_numeric,
};
use super::types::{EnumText, RawBytes};

pub(crate) fn sanitize_graphql_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            result.push(c);
        } else {
            result.push('_');
        }
    }
    if result.starts_with(|c: char| c.is_ascii_digit()) {
        result.insert(0, '_');
    }
    if result.is_empty() {
        result.push_str("_unnamed");
    }
    if result.starts_with("__") {
        result.insert(0, 'x');
    }
    result
}

#[expect(
    clippy::too_many_lines,
    reason = "handles all Postgres type conversions"
)]
pub(crate) fn row_value_to_const(
    row: &tokio_postgres::Row,
    idx: usize,
    col: &tokio_postgres::Column,
) -> anyhow::Result<ConstValue> {
    use postgres_types::Kind;
    use tokio_postgres::types::Type;

    let ty = col.type_();

    match *ty {
        Type::BOOL => {
            let v: Option<bool> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, ConstValue::Boolean))
        }
        Type::INT2 => {
            let v: Option<i16> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |n| ConstValue::Number(n.into())))
        }
        Type::INT4 => {
            let v: Option<i32> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |n| ConstValue::Number(n.into())))
        }
        Type::INT8 => {
            let v: Option<i64> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |n| ConstValue::String(n.to_string())))
        }
        Type::FLOAT4 => {
            let v: Option<f32> = row.try_get(idx)?;
            Ok(v.and_then(|n| serde_json::Number::from_f64(f64::from(n)))
                .map_or(ConstValue::Null, ConstValue::Number))
        }
        Type::FLOAT8 => {
            let v: Option<f64> = row.try_get(idx)?;
            Ok(v.and_then(serde_json::Number::from_f64)
                .map_or(ConstValue::Null, ConstValue::Number))
        }
        Type::JSON | Type::JSONB => {
            let v: Option<serde_json::Value> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |j| {
                ConstValue::from_json(j).unwrap_or(ConstValue::Null)
            }))
        }
        // --- chrono-based datetime types ---
        Type::TIMESTAMP => {
            let v: Option<chrono::NaiveDateTime> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |dt| {
                ConstValue::String(dt.and_utc().to_rfc3339())
            }))
        }
        Type::TIMESTAMPTZ => {
            let v: Option<chrono::DateTime<chrono::Utc>> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |dt| ConstValue::String(dt.to_rfc3339())))
        }
        Type::DATE => {
            let v: Option<chrono::NaiveDate> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |date| {
                ConstValue::String(date.and_time(chrono::NaiveTime::MIN).and_utc().to_rfc3339())
            }))
        }
        Type::TIME => {
            let v: Option<chrono::NaiveTime> = row.try_get(idx)?;
            Ok(v.map_or(ConstValue::Null, |t| ConstValue::String(t.to_string())))
        }
        // --- RawBytes-based types ---
        Type::TIMETZ => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(format_timetz(&raw.0)?)),
                None => Ok(ConstValue::Null),
            }
        }
        Type::INTERVAL => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(format_interval(&raw.0)?)),
                None => Ok(ConstValue::Null),
            }
        }
        Type::NUMERIC => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(parse_pg_numeric(&raw.0)?)),
                None => Ok(ConstValue::Null),
            }
        }
        Type::UUID => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(format_uuid(&raw.0)?)),
                None => Ok(ConstValue::Null),
            }
        }
        Type::BYTEA => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(format_bytea(&raw.0))),
                None => Ok(ConstValue::Null),
            }
        }
        Type::INET | Type::CIDR => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(format_inet(&raw.0)?)),
                None => Ok(ConstValue::Null),
            }
        }
        Type::MACADDR | Type::MACADDR8 => {
            let v: Option<RawBytes> = row.try_get(idx)?;
            match v {
                Some(raw) => Ok(ConstValue::String(format_macaddr(&raw.0)?)),
                None => Ok(ConstValue::Null),
            }
        }
        _ => {
            // Check for enum types.
            if matches!(ty.kind(), Kind::Enum(_)) {
                let v: Option<EnumText> = row.try_get(idx)?;
                return Ok(v.map_or(ConstValue::Null, |e| ConstValue::String(e.0)));
            }

            // Check for array types.
            if let Kind::Array(elem_ty) = ty.kind() {
                let v: Option<RawBytes> = row.try_get(idx)?;
                return match v {
                    Some(raw) => Ok(parse_pg_array(&raw.0, elem_ty)?),
                    None => Ok(ConstValue::Null),
                };
            }

            if let Kind::Range(elem_ty) = ty.kind() {
                let v: Option<RawBytes> = row.try_get(idx)?;
                return match v {
                    Some(raw) => Ok(ConstValue::String(format_range(&raw.0, elem_ty)?)),
                    None => Ok(ConstValue::Null),
                };
            }

            // Fallback: try to get as String (works for TEXT, VARCHAR, BPCHAR, NAME,
            // UNKNOWN).
            if let Ok(v) = row.try_get::<_, Option<String>>(idx) {
                Ok(v.map_or(ConstValue::Null, ConstValue::String))
            } else {
                tracing::warn!(
                    column = col.name(),
                    pg_type = %ty,
                    "unsupported PostgreSQL type, returning null"
                );
                Ok(ConstValue::Null)
            }
        }
    }
}

pub(crate) fn rows_to_const_value(rows: &[tokio_postgres::Row]) -> anyhow::Result<ConstValue> {
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = IndexMap::new();
        for (i, col) in row.columns().iter().enumerate() {
            let value = row_value_to_const(row, i, col)?;
            obj.insert(Name::new(sanitize_graphql_name(col.name())), value);
        }
        result.push(ConstValue::Object(obj));
    }
    Ok(ConstValue::List(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_graphql_name_normal() {
        assert_eq!(sanitize_graphql_name("user_name"), "user_name");
    }

    #[test]
    fn test_sanitize_graphql_name_special_chars() {
        assert_eq!(sanitize_graphql_name("user-name"), "user_name");
    }

    #[test]
    fn test_sanitize_graphql_name_starts_with_digit() {
        assert_eq!(sanitize_graphql_name("1col"), "_1col");
    }

    #[test]
    fn test_sanitize_graphql_name_empty() {
        assert_eq!(sanitize_graphql_name(""), "_unnamed");
    }

    #[test]
    fn test_sanitize_graphql_name_double_underscore() {
        assert_eq!(sanitize_graphql_name("__type"), "x__type");
    }
}
