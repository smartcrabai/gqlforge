pub(super) fn parse_naive_datetime(
    s: &str,
) -> Result<chrono::NaiveDateTime, Box<dyn std::error::Error + Sync + Send>> {
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ];
    for fmt in &formats {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(dt);
        }
    }
    Err(format!("cannot parse '{s}' as TIMESTAMP").into())
}

pub(super) fn parse_naive_time(
    s: &str,
) -> Result<chrono::NaiveTime, Box<dyn std::error::Error + Sync + Send>> {
    let formats = ["%H:%M:%S%.f", "%H:%M:%S", "%H:%M"];
    for fmt in &formats {
        if let Ok(t) = chrono::NaiveTime::parse_from_str(s, fmt) {
            return Ok(t);
        }
    }
    Err(format!("cannot parse '{s}' as TIME").into())
}

pub(super) fn parse_uuid_to_bytes(
    s: &str,
) -> Result<[u8; 16], Box<dyn std::error::Error + Sync + Send>> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return Err(format!("invalid UUID: '{s}'").into());
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn test_parse_naive_datetime_iso_t() {
        for (input, expected) in [
            ("2024-01-15T10:30:00", Some("2024-01-15 10:30:00")),
            ("2024-01-15 10:30:00", Some("2024-01-15 10:30:00")),
            (
                "2024-01-15T10:30:00.123456",
                Some("2024-01-15 10:30:00.123456"),
            ),
            ("not-a-date", None),
        ] {
            if let Some(expected) = expected {
                let dt = parse_naive_datetime(input).unwrap();
                assert_eq!(dt.to_string(), expected, "input: {input}");
            } else {
                assert!(parse_naive_datetime(input).is_err(), "input: {input}");
            }
        }
    }

    #[test]
    fn test_parse_naive_time_hms() {
        for (input, expected) in [
            ("10:30:00", Some("10:30:00")),
            ("10:30", Some("10:30:00")),
            ("25:00:00", None),
        ] {
            if let Some(expected) = expected {
                let time = parse_naive_time(input).unwrap();
                assert_eq!(time.to_string(), expected, "input: {input}");
            } else {
                assert!(parse_naive_time(input).is_err(), "input: {input}");
            }
        }
    }

    #[test]
    fn test_parse_uuid_to_bytes_valid() {
        let expected_bytes = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        for (input, expected) in [
            ("550e8400-e29b-41d4-a716-446655440000", Some(expected_bytes)),
            ("550e8400e29b41d4a716446655440000", Some(expected_bytes)),
            ("invalid-uuid", None),
        ] {
            if let Some(expected) = expected {
                let bytes = parse_uuid_to_bytes(input).unwrap();
                assert_eq!(bytes, expected, "input: {input}");
            } else {
                assert!(parse_uuid_to_bytes(input).is_err(), "input: {input}");
            }
        }
    }
}
