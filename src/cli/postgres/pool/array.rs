use gqlrs_value::ConstValue;

use super::binary_format::{
    format_bytea, format_date, format_inet, format_interval, format_macaddr, format_range,
    format_timestamp, format_timestamptz, format_timetz, format_uuid, parse_pg_numeric,
};

pub(super) fn raw_element_to_const(
    ty: &postgres_types::Type,
    raw: &[u8],
) -> anyhow::Result<ConstValue> {
    use postgres_types::{Kind, Type};

    match *ty {
        Type::BOOL => {
            if raw.is_empty() {
                return Ok(ConstValue::Null);
            }
            Ok(ConstValue::Boolean(raw[0] != 0))
        }
        Type::INT2 => {
            let v = i16::from_be_bytes(raw.try_into()?);
            Ok(ConstValue::Number(v.into()))
        }
        Type::INT4 => {
            let v = i32::from_be_bytes(raw.try_into()?);
            Ok(ConstValue::Number(v.into()))
        }
        Type::INT8 => {
            let v = i64::from_be_bytes(raw.try_into()?);
            Ok(ConstValue::Number(v.into()))
        }
        Type::FLOAT4 => {
            let v = f32::from_be_bytes(raw.try_into()?);
            Ok(serde_json::Number::from_f64(f64::from(v))
                .map_or(ConstValue::Null, ConstValue::Number))
        }
        Type::FLOAT8 => {
            let v = f64::from_be_bytes(raw.try_into()?);
            Ok(serde_json::Number::from_f64(v).map_or(ConstValue::Null, ConstValue::Number))
        }
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            let s = std::str::from_utf8(raw)?;
            Ok(ConstValue::String(s.to_string()))
        }
        Type::UUID => Ok(ConstValue::String(format_uuid(raw)?)),
        Type::BYTEA => Ok(ConstValue::String(format_bytea(raw))),
        Type::NUMERIC => Ok(ConstValue::String(parse_pg_numeric(raw)?)),
        Type::TIMESTAMP => Ok(ConstValue::String(format_timestamp(raw)?)),
        Type::TIMESTAMPTZ => Ok(ConstValue::String(format_timestamptz(raw)?)),
        Type::DATE => Ok(ConstValue::String(format_date(raw)?)),
        Type::TIME => {
            // i64 microseconds since midnight.
            let us = i64::from_be_bytes(raw.try_into()?);
            let total_secs = u32::try_from(us / 1_000_000).unwrap_or(0);
            let micro = u32::try_from(us % 1_000_000).unwrap_or(0);
            let t = chrono::NaiveTime::from_num_seconds_from_midnight_opt(total_secs, micro * 1000)
                .unwrap_or_default();
            Ok(ConstValue::String(t.to_string()))
        }
        Type::TIMETZ => Ok(ConstValue::String(format_timetz(raw)?)),
        Type::INTERVAL => Ok(ConstValue::String(format_interval(raw)?)),
        Type::INET | Type::CIDR => Ok(ConstValue::String(format_inet(raw)?)),
        Type::MACADDR | Type::MACADDR8 => Ok(ConstValue::String(format_macaddr(raw)?)),
        Type::JSON | Type::JSONB => {
            // JSONB binary has a version byte prefix (0x01); JSON does not.
            let data = if *ty == Type::JSONB && !raw.is_empty() {
                &raw[1..]
            } else {
                raw
            };
            let v: serde_json::Value = serde_json::from_slice(data)?;
            Ok(ConstValue::from_json(v).unwrap_or(ConstValue::Null))
        }
        _ => {
            // For range types (e.g., arrays of ranges), format as PostgreSQL
            // text.
            if let Kind::Range(elem_ty) = ty.kind() {
                return Ok(ConstValue::String(format_range(raw, elem_ty)?));
            }
            // For enum and other types, try UTF-8 string.
            match std::str::from_utf8(raw) {
                Ok(s) => Ok(ConstValue::String(s.to_string())),
                Err(_) => Ok(ConstValue::String(format_bytea(raw))),
            }
        }
    }
}

fn read_elements(
    raw: &[u8],
    pos: &mut usize,
    dims: &[usize],
    dim_idx: usize,
    elem_type: &postgres_types::Type,
) -> anyhow::Result<ConstValue> {
    if dim_idx == dims.len() - 1 {
        // Leaf dimension: read actual elements.
        let mut items = Vec::with_capacity(dims[dim_idx]);
        for _ in 0..dims[dim_idx] {
            if *pos + 4 > raw.len() {
                anyhow::bail!("array binary truncated");
            }
            let elem_len = i64::from(i32::from_be_bytes(raw[*pos..*pos + 4].try_into()?));
            *pos += 4;
            if elem_len < 0 {
                // NULL element.
                items.push(ConstValue::Null);
            } else {
                let len = usize::try_from(elem_len).unwrap_or(0);
                if *pos + len > raw.len() {
                    anyhow::bail!("array element data truncated");
                }
                let elem_data = &raw[*pos..*pos + len];
                *pos += len;
                items.push(raw_element_to_const(elem_type, elem_data)?);
            }
        }
        Ok(ConstValue::List(items))
    } else {
        // Nested dimension.
        let mut items = Vec::with_capacity(dims[dim_idx]);
        for _ in 0..dims[dim_idx] {
            items.push(read_elements(raw, pos, dims, dim_idx + 1, elem_type)?);
        }
        Ok(ConstValue::List(items))
    }
}

pub(super) fn parse_pg_array(
    raw: &[u8],
    elem_type: &postgres_types::Type,
) -> anyhow::Result<ConstValue> {
    if raw.len() < 12 {
        anyhow::bail!("array binary too short");
    }

    let ndim = i32::from_be_bytes(raw[0..4].try_into()?);
    // has_null flag at raw[4..8], elem_oid at raw[8..12] - we don't need them.

    if ndim < 0 {
        anyhow::bail!("array ndim is negative: {ndim}");
    }

    if ndim == 0 {
        return Ok(ConstValue::List(vec![]));
    }

    // Read dimension sizes and lower bounds.
    let ndim_usize = usize::try_from(ndim).unwrap_or(0);
    let header_end = 12 + ndim_usize * 8;
    if raw.len() < header_end {
        anyhow::bail!("array binary too short for dimensions");
    }

    let mut dims = Vec::with_capacity(ndim_usize);
    for i in 0..ndim_usize {
        let offset = 12 + i * 8;
        let dim_size_i32 = i32::from_be_bytes(raw[offset..offset + 4].try_into()?);
        if dim_size_i32 < 0 {
            anyhow::bail!("array dim_size is negative: {dim_size_i32}");
        }
        let dim_size = usize::try_from(dim_size_i32).unwrap_or(0);
        // lower_bound at offset+4..offset+8 - not needed for value parsing.
        dims.push(dim_size);
    }

    let mut pos = header_end;

    read_elements(raw, &mut pos, &dims, 0, elem_type)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    fn build_pg_array(elem_oid: i32, elements: &[Option<&[u8]>]) -> Vec<u8> {
        let mut buf = Vec::new();
        // ndim=1
        buf.extend_from_slice(&1i32.to_be_bytes());
        // has_null
        let has_null = i32::from(elements.iter().any(std::option::Option::is_none));
        buf.extend_from_slice(&has_null.to_be_bytes());
        // elem_oid
        buf.extend_from_slice(&elem_oid.to_be_bytes());
        // dim_size
        buf.extend_from_slice(&(i32::try_from(elements.len()).unwrap_or(i32::MAX)).to_be_bytes());
        // lower_bound
        buf.extend_from_slice(&1i32.to_be_bytes());
        // elements
        for elem in elements {
            match elem {
                None => {
                    buf.extend_from_slice(&(-1i32).to_be_bytes());
                }
                Some(data) => {
                    buf.extend_from_slice(
                        &(i32::try_from(data.len()).unwrap_or(i32::MAX)).to_be_bytes(),
                    );
                    buf.extend_from_slice(data);
                }
            }
        }
        buf
    }

    fn build_empty_pg_array() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0i32.to_be_bytes()); // ndim=0
        buf.extend_from_slice(&0i32.to_be_bytes()); // has_null
        buf.extend_from_slice(&0i32.to_be_bytes()); // elem_oid
        buf
    }

    #[test]
    fn test_parse_pg_array_empty() {
        let raw = build_empty_pg_array();
        let result = parse_pg_array(&raw, &postgres_types::Type::INT4).unwrap();
        assert_eq!(result, ConstValue::List(vec![]));
    }

    #[test]
    fn test_parse_pg_array_int4() {
        let b1 = 1i32.to_be_bytes();
        let b2 = 2i32.to_be_bytes();
        let b3 = 3i32.to_be_bytes();
        let elems: Vec<Option<&[u8]>> = vec![Some(&b1), Some(&b2), Some(&b3)];
        // INT4 OID = 23
        let raw = build_pg_array(23, &elems);
        let result = parse_pg_array(&raw, &postgres_types::Type::INT4).unwrap();
        assert_eq!(
            result,
            ConstValue::List(vec![
                ConstValue::Number(1.into()),
                ConstValue::Number(2.into()),
                ConstValue::Number(3.into()),
            ])
        );
    }

    #[test]
    fn test_parse_pg_array_text() {
        let elems: Vec<Option<&[u8]>> = vec![Some(b"a"), Some(b"b")];
        // TEXT OID = 25
        let raw = build_pg_array(25, &elems);
        let result = parse_pg_array(&raw, &postgres_types::Type::TEXT).unwrap();
        assert_eq!(
            result,
            ConstValue::List(vec![
                ConstValue::String("a".to_string()),
                ConstValue::String("b".to_string()),
            ])
        );
    }

    #[test]
    fn test_parse_pg_array_with_null() {
        let b1 = 1i32.to_be_bytes();
        let b3 = 3i32.to_be_bytes();
        let elems: Vec<Option<&[u8]>> = vec![Some(&b1), None, Some(&b3)];
        let raw = build_pg_array(23, &elems);
        let result = parse_pg_array(&raw, &postgres_types::Type::INT4).unwrap();
        assert_eq!(
            result,
            ConstValue::List(vec![
                ConstValue::Number(1.into()),
                ConstValue::Null,
                ConstValue::Number(3.into()),
            ])
        );
    }

    #[test]
    fn test_parse_pg_array_bool() {
        let elems: Vec<Option<&[u8]>> = vec![Some(&[1u8]), Some(&[0u8])];
        // BOOL OID = 16
        let raw = build_pg_array(16, &elems);
        let result = parse_pg_array(&raw, &postgres_types::Type::BOOL).unwrap();
        assert_eq!(
            result,
            ConstValue::List(vec![ConstValue::Boolean(true), ConstValue::Boolean(false),])
        );
    }

    #[test]
    fn test_raw_element_bool() {
        let result = raw_element_to_const(&postgres_types::Type::BOOL, &[1]).unwrap();
        assert_eq!(result, ConstValue::Boolean(true));

        let result = raw_element_to_const(&postgres_types::Type::BOOL, &[0]).unwrap();
        assert_eq!(result, ConstValue::Boolean(false));
    }

    #[test]
    fn test_raw_element_int4() {
        let result =
            raw_element_to_const(&postgres_types::Type::INT4, &42i32.to_be_bytes()).unwrap();
        assert_eq!(result, ConstValue::Number(42.into()));
    }

    #[test]
    fn test_raw_element_float8() {
        let result =
            raw_element_to_const(&postgres_types::Type::FLOAT8, &1.23_f64.to_be_bytes()).unwrap();
        match result {
            ConstValue::Number(n) => {
                let v: f64 = n.as_f64().unwrap();
                assert!((v - 1.23_f64).abs() < 1e-10);
            }
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn test_raw_element_text() {
        let result = raw_element_to_const(&postgres_types::Type::TEXT, b"hello").unwrap();
        assert_eq!(result, ConstValue::String("hello".to_string()));
    }

    #[test]
    fn test_raw_element_uuid() {
        let bytes: [u8; 16] = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        let result = raw_element_to_const(&postgres_types::Type::UUID, &bytes).unwrap();
        assert_eq!(
            result,
            ConstValue::String("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn test_raw_element_int2() {
        let result =
            raw_element_to_const(&postgres_types::Type::INT2, &42i16.to_be_bytes()).unwrap();
        assert_eq!(result, ConstValue::Number(42.into()));
    }

    #[test]
    fn test_raw_element_int8() {
        let result = raw_element_to_const(
            &postgres_types::Type::INT8,
            &9_999_999_999_i64.to_be_bytes(),
        )
        .unwrap();
        assert_eq!(result, ConstValue::Number(9_999_999_999_i64.into()));
    }

    #[test]
    fn test_raw_element_float4() {
        let result =
            raw_element_to_const(&postgres_types::Type::FLOAT4, &1.5f32.to_be_bytes()).unwrap();
        match result {
            ConstValue::Number(n) => {
                let v = n.as_f64().unwrap();
                assert!((v - 1.5).abs() < 1e-6);
            }
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn test_raw_element_float4_nan() {
        let result =
            raw_element_to_const(&postgres_types::Type::FLOAT4, &f32::NAN.to_be_bytes()).unwrap();
        assert_eq!(result, ConstValue::Null);
    }

    #[test]
    fn test_raw_element_varchar() {
        let result = raw_element_to_const(&postgres_types::Type::VARCHAR, b"test string").unwrap();
        assert_eq!(result, ConstValue::String("test string".to_string()));
    }

    #[test]
    fn test_raw_element_timestamp() {
        // 2024-01-15T10:30:00 is 8780 days + 37800 seconds after
        // 2000-01-01T00:00:00 = (8780 * 86400 + 37800) * 1_000_000 us =
        // 758_629_800_000_000
        let us: i64 = 758_629_800_000_000;
        let result =
            raw_element_to_const(&postgres_types::Type::TIMESTAMP, &us.to_be_bytes()).unwrap();
        match result {
            ConstValue::String(s) => {
                assert!(s.starts_with("2024-01-15T10:30:00"));
            }
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_raw_element_timestamptz() {
        // Same us value; TIMESTAMPTZ returns RFC3339 with +00:00
        let us: i64 = 758_629_800_000_000;
        let result =
            raw_element_to_const(&postgres_types::Type::TIMESTAMPTZ, &us.to_be_bytes()).unwrap();
        match result {
            ConstValue::String(s) => {
                assert!(s.contains("2024-01-15"));
                assert!(s.contains("+00:00"));
            }
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_raw_element_date() {
        // 2024-01-15 is 8780 days after 2000-01-01
        let days: i32 = 8780;
        let result =
            raw_element_to_const(&postgres_types::Type::DATE, &days.to_be_bytes()).unwrap();
        assert_eq!(result, ConstValue::String("2024-01-15".to_string()));
    }

    #[test]
    fn test_raw_element_time() {
        // 10:30:00 = (10*3600 + 30*60) * 1_000_000 us = 37_800_000_000
        let us: i64 = 37_800_000_000;
        let result = raw_element_to_const(&postgres_types::Type::TIME, &us.to_be_bytes()).unwrap();
        assert_eq!(result, ConstValue::String("10:30:00".to_string()));
    }

    #[test]
    fn test_raw_element_json() {
        let json_bytes = br#"{"key":"value"}"#;
        let result = raw_element_to_const(&postgres_types::Type::JSON, json_bytes).unwrap();
        match result {
            ConstValue::Object(obj) => {
                assert_eq!(
                    obj.get("key"),
                    Some(&ConstValue::String("value".to_string()))
                );
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn test_raw_element_jsonb() {
        // JSONB has a 0x01 version byte prefix
        let mut jsonb_bytes = vec![0x01u8];
        jsonb_bytes.extend_from_slice(br#"{"key":"value"}"#);
        let result = raw_element_to_const(&postgres_types::Type::JSONB, &jsonb_bytes).unwrap();
        match result {
            ConstValue::Object(obj) => {
                assert_eq!(
                    obj.get("key"),
                    Some(&ConstValue::String("value".to_string()))
                );
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn test_raw_element_unknown_utf8_fallback() {
        let result = raw_element_to_const(&postgres_types::Type::REGCLASS, b"some_table").unwrap();
        assert_eq!(result, ConstValue::String("some_table".to_string()));
    }

    #[test]
    fn test_parse_pg_array_negative_ndim() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(-1i32).to_be_bytes()); // ndim = -1
        raw.extend_from_slice(&0i32.to_be_bytes()); // has_null
        raw.extend_from_slice(&23i32.to_be_bytes()); // elem_oid
        assert!(parse_pg_array(&raw, &postgres_types::Type::INT4).is_err());
    }
}
