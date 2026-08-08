use super::datetime::{parse_naive_datetime, parse_naive_time, parse_uuid_to_bytes};

/// Wrapper that accepts any `PostgreSQL` type and reads the raw binary data.
pub(super) struct RawBytes(pub(super) Vec<u8>);

impl<'a> postgres_types::FromSql<'a> for RawBytes {
    fn from_sql(
        _ty: &postgres_types::Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(RawBytes(raw.to_vec()))
    }

    fn accepts(_ty: &postgres_types::Type) -> bool {
        true
    }
}

/// Wrapper that reads `PostgreSQL` enum values as UTF-8 strings.
pub(super) struct EnumText(pub(super) String);

impl<'a> postgres_types::FromSql<'a> for EnumText {
    fn from_sql(
        _ty: &postgres_types::Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        let s = std::str::from_utf8(raw)?;
        Ok(EnumText(s.to_string()))
    }

    fn accepts(ty: &postgres_types::Type) -> bool {
        matches!(ty.kind(), postgres_types::Kind::Enum(_))
    }
}

/// Wrapper that converts a string parameter to the type `PostgreSQL` expects.
#[derive(Debug)]
pub(crate) struct TypedParam(pub(crate) Option<String>);

impl postgres_types::ToSql for TypedParam {
    fn to_sql(
        &self,
        ty: &postgres_types::Type,
        out: &mut bytes::BytesMut,
    ) -> Result<postgres_types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use postgres_types::Type;

        let Some(value) = self.0.as_deref() else {
            return Ok(postgres_types::IsNull::Yes);
        };

        match *ty {
            Type::BOOL => {
                let b = match value.to_lowercase().as_str() {
                    "true" | "t" | "1" | "yes" => true,
                    "false" | "f" | "0" | "no" => false,
                    other => {
                        return Err(format!("cannot convert '{other}' to bool").into());
                    }
                };
                b.to_sql(&Type::BOOL, out)
            }
            Type::INT2 => {
                let v: i16 = value.parse()?;
                v.to_sql(&Type::INT2, out)
            }
            Type::INT4 => {
                let v: i32 = value.parse()?;
                v.to_sql(&Type::INT4, out)
            }
            Type::INT8 => {
                let v: i64 = value.parse()?;
                v.to_sql(&Type::INT8, out)
            }
            Type::FLOAT4 => {
                let v: f32 = value.parse()?;
                v.to_sql(&Type::FLOAT4, out)
            }
            Type::FLOAT8 => {
                let v: f64 = value.parse()?;
                v.to_sql(&Type::FLOAT8, out)
            }
            Type::TIMESTAMP => {
                let dt = parse_naive_datetime(value)?;
                dt.to_sql(&Type::TIMESTAMP, out)
            }
            Type::TIMESTAMPTZ => {
                let dt = chrono::DateTime::parse_from_rfc3339(value)
                    .map(|d| d.with_timezone(&chrono::Utc))?;
                dt.to_sql(&Type::TIMESTAMPTZ, out)
            }
            Type::DATE => {
                let d = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
                d.to_sql(&Type::DATE, out)
            }
            Type::TIME => {
                let t = parse_naive_time(value)?;
                t.to_sql(&Type::TIME, out)
            }
            Type::UUID => {
                let bytes = parse_uuid_to_bytes(value)?;
                out.extend_from_slice(&bytes);
                Ok(postgres_types::IsNull::No)
            }
            _ => {
                // TEXT, VARCHAR, enum, and everything else: send as raw UTF-8.
                out.extend_from_slice(value.as_bytes());
                Ok(postgres_types::IsNull::No)
            }
        }
    }

    fn encode_format(&self, ty: &postgres_types::Type) -> postgres_types::Format {
        use postgres_types::Type;
        match *ty {
            Type::BOOL
            | Type::INT2
            | Type::INT4
            | Type::INT8
            | Type::FLOAT4
            | Type::FLOAT8
            | Type::TIMESTAMP
            | Type::TIMESTAMPTZ
            | Type::DATE
            | Type::TIME
            | Type::UUID => postgres_types::Format::Binary,
            _ => postgres_types::Format::Text,
        }
    }

    fn accepts(_ty: &postgres_types::Type) -> bool {
        true
    }

    postgres_types::to_sql_checked!();
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    fn typed_param_to_bytes(
        value: &str,
        ty: &postgres_types::Type,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Sync + Send>> {
        use bytes::BytesMut;
        use postgres_types::ToSql;

        let param = TypedParam(Some(value.to_string()));
        let mut buf = BytesMut::new();
        param.to_sql(ty, &mut buf)?;
        Ok(buf.to_vec())
    }

    #[test]
    fn test_typed_param_bool_true() {
        let bytes = typed_param_to_bytes("true", &postgres_types::Type::BOOL).unwrap();
        assert_eq!(bytes, vec![1]);
    }

    #[test]
    fn test_typed_param_bool_false() {
        let bytes = typed_param_to_bytes("false", &postgres_types::Type::BOOL).unwrap();
        assert_eq!(bytes, vec![0]);
    }

    #[test]
    fn test_typed_param_bool_t() {
        let bytes = typed_param_to_bytes("t", &postgres_types::Type::BOOL).unwrap();
        assert_eq!(bytes, vec![1]);
    }

    #[test]
    fn test_typed_param_bool_f() {
        let bytes = typed_param_to_bytes("f", &postgres_types::Type::BOOL).unwrap();
        assert_eq!(bytes, vec![0]);
    }

    #[test]
    fn test_typed_param_bool_1() {
        let bytes = typed_param_to_bytes("1", &postgres_types::Type::BOOL).unwrap();
        assert_eq!(bytes, vec![1]);
    }

    #[test]
    fn test_typed_param_bool_0() {
        let bytes = typed_param_to_bytes("0", &postgres_types::Type::BOOL).unwrap();
        assert_eq!(bytes, vec![0]);
    }

    #[test]
    fn test_typed_param_bool_invalid() {
        assert!(typed_param_to_bytes("maybe", &postgres_types::Type::BOOL).is_err());
    }

    #[test]
    fn test_typed_param_int4() {
        let bytes = typed_param_to_bytes("42", &postgres_types::Type::INT4).unwrap();
        assert_eq!(bytes, 42i32.to_be_bytes().to_vec());
    }

    #[test]
    fn test_typed_param_int4_neg() {
        let bytes = typed_param_to_bytes("-1", &postgres_types::Type::INT4).unwrap();
        assert_eq!(bytes, (-1i32).to_be_bytes().to_vec());
    }

    #[test]
    fn test_typed_param_int8() {
        let bytes = typed_param_to_bytes("9999999999", &postgres_types::Type::INT8).unwrap();
        assert_eq!(bytes, 9_999_999_999_i64.to_be_bytes().to_vec());
    }

    #[test]
    fn test_typed_param_float8() {
        let bytes = typed_param_to_bytes("1.23", &postgres_types::Type::FLOAT8).unwrap();
        assert_eq!(bytes, 1.23_f64.to_be_bytes().to_vec());
    }

    #[test]
    fn test_typed_param_uuid() {
        let bytes = typed_param_to_bytes(
            "550e8400-e29b-41d4-a716-446655440000",
            &postgres_types::Type::UUID,
        )
        .unwrap();
        assert_eq!(bytes.len(), 16);
        assert_eq!(
            bytes,
            vec![
                0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
                0x00, 0x00
            ]
        );
    }

    #[test]
    fn test_typed_param_text() {
        let bytes = typed_param_to_bytes("hello", &postgres_types::Type::TEXT).unwrap();
        assert_eq!(bytes, b"hello".to_vec());
    }

    #[test]
    fn test_typed_param_text_default() {
        // Unknown type falls back to text encoding.
        let bytes = typed_param_to_bytes("hello", &postgres_types::Type::NAME).unwrap();
        assert_eq!(bytes, b"hello".to_vec());
    }

    #[test]
    fn test_typed_param_int4_invalid() {
        assert!(typed_param_to_bytes("abc", &postgres_types::Type::INT4).is_err());
    }

    #[test]
    fn test_typed_param_int2() {
        let bytes = typed_param_to_bytes("123", &postgres_types::Type::INT2).unwrap();
        assert_eq!(bytes, 123i16.to_be_bytes().to_vec());
    }

    #[test]
    fn test_typed_param_float4() {
        let bytes = typed_param_to_bytes("1.5", &postgres_types::Type::FLOAT4).unwrap();
        assert_eq!(bytes, 1.5f32.to_be_bytes().to_vec());
    }

    #[test]
    fn test_typed_param_float8_invalid() {
        assert!(typed_param_to_bytes("xyz", &postgres_types::Type::FLOAT8).is_err());
    }

    #[test]
    fn test_typed_param_timestamp() {
        let result = typed_param_to_bytes("2024-01-15T10:30:00", &postgres_types::Type::TIMESTAMP);
        assert!(result.is_ok());
    }

    #[test]
    fn test_typed_param_date() {
        let bytes = typed_param_to_bytes("2024-01-15", &postgres_types::Type::DATE).unwrap();
        assert!(!bytes.is_empty(), "expected non-empty bytes");
    }

    #[test]
    fn test_typed_param_date_invalid() {
        assert!(typed_param_to_bytes("2024-13-01", &postgres_types::Type::DATE).is_err());
    }

    #[test]
    fn test_typed_param_time() {
        let bytes = typed_param_to_bytes("10:30:00", &postgres_types::Type::TIME).unwrap();
        assert!(!bytes.is_empty(), "expected non-empty bytes");
    }

    #[test]
    fn test_typed_param_uuid_invalid() {
        assert!(typed_param_to_bytes("invalid-uuid", &postgres_types::Type::UUID).is_err());
    }

    #[test]
    fn typed_param_null_encodes_sql_null() {
        use bytes::BytesMut;
        use postgres_types::ToSql;

        let param = TypedParam(None);
        let mut buffer = BytesMut::new();
        assert!(matches!(
            param
                .to_sql(&postgres_types::Type::TEXT, &mut buffer)
                .unwrap(),
            postgres_types::IsNull::Yes
        ));
        assert!(buffer.is_empty(), "expected empty buffer");
    }
}
