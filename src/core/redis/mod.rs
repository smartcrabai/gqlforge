pub mod request_template;

use std::pin::Pin;

use async_graphql_value::ConstValue;
use futures_util::Stream;
pub use request_template::RequestTemplate;

use crate::core::config::RedisOperation;
/// How to interpret Redis string payloads (GET results, PUBLISH messages,
/// stream entries).
pub use crate::core::config::RedisPayloadType;
use crate::core::mustache::Mustache;

/// Describes where a `@redis` subscription reads its events from.
#[derive(Clone, Debug)]
pub enum RedisStreamSource {
    /// Listen to a Pub/Sub channel (`SUBSCRIBE`).
    PubSub { channel: Mustache },
    /// Read new entries from a Redis Stream (`XREAD`).
    Stream { key: Mustache, start_id: Mustache },
}

/// Trait for executing Redis commands. Concrete implementations live in the
/// CLI crate (real connection pool) or in test utilities (mock).
#[async_trait::async_trait]
pub trait RedisIO: Send + Sync + 'static {
    /// Execute a Redis command with positional arguments and return the
    /// result as a `ConstValue`.
    async fn execute(&self, command: &str, args: &[String]) -> anyhow::Result<ConstValue>;
}

/// Trait for subscribing to Redis Pub/Sub channels and Streams.
/// Concrete implementations live in the CLI crate or in test utilities (mock
/// listener).
#[async_trait::async_trait]
pub trait RedisListenerIO: Send + Sync + 'static {
    /// Subscribe to a Redis Pub/Sub channel.
    /// Returns a stream that yields one `ConstValue` per message.
    async fn subscribe(
        &self,
        channel: &str,
        payload_type: RedisPayloadType,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<ConstValue, anyhow::Error>> + Send>>>;

    /// Read new entries from a Redis Stream (`XREAD`).
    /// Returns a stream that yields one `ConstValue::Object { "id", "values" }`
    /// per entry.
    async fn read_stream(
        &self,
        key: &str,
        start_id: &str,
        payload_type: RedisPayloadType,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<ConstValue, anyhow::Error>> + Send>>>;
}

/// Decode a raw Redis string payload according to the configured
/// `RedisPayloadType`.
///
/// `JSON`: tries to parse as JSON, falling back to `ConstValue::String` on
/// failure. `TEXT`: always returns `ConstValue::String`.
#[must_use]
pub fn decode_payload(raw: &str, payload_type: &RedisPayloadType) -> ConstValue {
    match payload_type {
        RedisPayloadType::Json => serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| ConstValue::from_json(v).ok())
            .unwrap_or_else(|| ConstValue::String(raw.to_string())),
        RedisPayloadType::Text => ConstValue::String(raw.to_string()),
    }
}

/// Recursively walks a `ConstValue`, applying `decode_payload` to every
/// string leaf. Lists and objects are traversed; other value kinds are
/// returned unchanged.
#[must_use]
pub fn decode_value_leaves(value: ConstValue, payload_type: &RedisPayloadType) -> ConstValue {
    match value {
        ConstValue::String(s) => decode_payload(&s, payload_type),
        ConstValue::List(items) => ConstValue::List(
            items
                .into_iter()
                .map(|v| decode_value_leaves(v, payload_type))
                .collect(),
        ),
        ConstValue::Object(map) => ConstValue::Object(
            map.into_iter()
                .map(|(k, v)| (k, decode_value_leaves(v, payload_type)))
                .collect(),
        ),
        other => other,
    }
}

/// Normalizes a raw Redis command result into the shape expected by the
/// GraphQL type declared on the field, correcting for RESP2/RESP3
/// differences and for wire-level types that don't match the directive's
/// documented (`Boolean`) return type.
///
/// This must run on the raw [`ConstValue`] produced by driver-level
/// conversion (e.g. `redis_value_to_const` in the CLI crate), **before**
/// [`decode_value_leaves`] interprets string leaves as JSON:
///
/// - `HGETALL`: RESP2 (the default protocol) returns hash contents as a flat
///   array of alternating field/value strings rather than a map, so
///   `redis_value_to_const` produces a `ConstValue::List` instead of an
///   `Object`. This folds well-formed pairs (even length, all-string elements)
///   into an `Object`. A `Value::Map` (RESP3, or a test mock) already arrives
///   as `ConstValue::Object` and passes through unchanged.
/// - `EXISTS`: Redis replies with an integer count; normalized to `Boolean`
///   (`true` when the count is greater than zero).
/// - `SET`: Redis replies `+OK` (`ConstValue::String("OK")`) on success, or a
///   null bulk reply (`ConstValue::Null`) when a condition modifier (e.g.
///   `NX`/`XX`, not currently exposed by `@redis`) prevents the write.
///   Normalized to `Boolean` accordingly.
///
/// Every other operation, and any already-normalized shape (e.g. RESP3's
/// native `Boolean`/`Map`), passes through unchanged.
#[must_use]
pub fn normalize_command_result(operation: &RedisOperation, value: ConstValue) -> ConstValue {
    match operation {
        RedisOperation::Hgetall => normalize_hgetall(value),
        RedisOperation::Exists => normalize_exists(value),
        RedisOperation::Set => normalize_set(value),
        _ => value,
    }
}

/// Folds a flat `[field1, value1, field2, value2, ...]` list (RESP2's
/// `HGETALL` shape) into an `Object`, including the empty list a missing
/// key's `HGETALL` returns (zero pairs unambiguously folds to an empty
/// object). Anything that isn't safely foldable -- already an `Object`, an
/// odd-length list, or a list with non-string elements -- passes through
/// unchanged instead of guessing.
fn normalize_hgetall(value: ConstValue) -> ConstValue {
    match value {
        ConstValue::List(items)
            if items.len() % 2 == 0 && items.iter().all(|v| matches!(v, ConstValue::String(_))) =>
        {
            let mut map = indexmap::IndexMap::with_capacity(items.len() / 2);
            let mut iter = items.into_iter();
            while let (Some(field), Some(val)) = (iter.next(), iter.next()) {
                let ConstValue::String(field) = field else {
                    unreachable!("guarded by the match arm's all-string check above")
                };
                map.insert(async_graphql::Name::new(field), val);
            }
            ConstValue::Object(map)
        }
        other => other,
    }
}

/// `EXISTS` replies with an integer count of the keys that exist; the
/// `@redis` directive documents it as returning `Boolean`.
fn normalize_exists(value: ConstValue) -> ConstValue {
    match value {
        ConstValue::Number(n) => ConstValue::Boolean(n.as_i64().is_some_and(|n| n > 0)),
        other => other,
    }
}

/// `SET` replies `+OK` on success or a null bulk reply when a condition
/// modifier suppresses the write; the `@redis` directive documents it as
/// returning `Boolean`.
fn normalize_set(value: ConstValue) -> ConstValue {
    match value {
        ConstValue::String(s) if s == "OK" => ConstValue::Boolean(true),
        ConstValue::Null => ConstValue::Boolean(false),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_payload_json_parses_object() {
        let value = decode_payload(r#"{"a":1}"#, &RedisPayloadType::Json);
        match value {
            ConstValue::Object(map) => {
                assert_eq!(map.get("a"), Some(&ConstValue::Number(1.into())));
            }
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn decode_payload_json_falls_back_to_string() {
        let value = decode_payload("not json", &RedisPayloadType::Json);
        assert_eq!(value, ConstValue::String("not json".to_string()));
    }

    #[test]
    fn decode_payload_text_always_string() {
        let value = decode_payload(r#"{"a":1}"#, &RedisPayloadType::Text);
        assert_eq!(value, ConstValue::String(r#"{"a":1}"#.to_string()));
    }

    #[test]
    fn decode_value_leaves_recurses_into_list() {
        let value = ConstValue::List(vec![
            ConstValue::String(r#"{"a":1}"#.to_string()),
            ConstValue::String("plain".to_string()),
        ]);
        let decoded = decode_value_leaves(value, &RedisPayloadType::Json);
        match decoded {
            ConstValue::List(items) => {
                assert!(matches!(items[0], ConstValue::Object(_)));
                assert_eq!(items[1], ConstValue::String("plain".to_string()));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn decode_value_leaves_recurses_into_object() {
        let mut map = indexmap::IndexMap::new();
        map.insert(
            async_graphql::Name::new("field"),
            ConstValue::String("42".to_string()),
        );
        let decoded = decode_value_leaves(ConstValue::Object(map), &RedisPayloadType::Json);
        match decoded {
            ConstValue::Object(map) => {
                assert_eq!(map.get("field"), Some(&ConstValue::Number(42.into())));
            }
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn decode_value_leaves_text_leaves_strings_unchanged() {
        let value = ConstValue::String(r#"{"a":1}"#.to_string());
        let decoded = decode_value_leaves(value, &RedisPayloadType::Text);
        assert_eq!(decoded, ConstValue::String(r#"{"a":1}"#.to_string()));
    }

    // -- normalize_command_result -------------------------------------

    #[test]
    fn normalize_hgetall_folds_resp2_flat_pairs_into_object() {
        let value = ConstValue::List(vec![
            ConstValue::String("name".to_string()),
            ConstValue::String("Alice".to_string()),
            ConstValue::String("age".to_string()),
            ConstValue::String("30".to_string()),
        ]);
        let normalized = normalize_command_result(&RedisOperation::Hgetall, value);
        match normalized {
            ConstValue::Object(map) => {
                assert_eq!(
                    map.get("name"),
                    Some(&ConstValue::String("Alice".to_string()))
                );
                assert_eq!(map.get("age"), Some(&ConstValue::String("30".to_string())));
            }
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn normalize_hgetall_passes_through_resp3_object() {
        let mut map = indexmap::IndexMap::new();
        map.insert(
            async_graphql::Name::new("name"),
            ConstValue::String("Alice".to_string()),
        );
        let value = ConstValue::Object(map.clone());
        let normalized = normalize_command_result(&RedisOperation::Hgetall, value);
        assert_eq!(normalized, ConstValue::Object(map));
    }

    #[test]
    fn normalize_hgetall_passes_through_odd_length_list() {
        // Malformed/unexpected shape: folding could silently drop the
        // trailing element, so leave it untouched rather than guess.
        let value = ConstValue::List(vec![
            ConstValue::String("name".to_string()),
            ConstValue::String("Alice".to_string()),
            ConstValue::String("orphan".to_string()),
        ]);
        let normalized = normalize_command_result(&RedisOperation::Hgetall, value.clone());
        assert_eq!(normalized, value);
    }

    #[test]
    fn normalize_hgetall_passes_through_non_string_elements() {
        // A list with non-string elements isn't the RESP2 HGETALL shape;
        // leave it alone rather than folding nonsense pairs.
        let value = ConstValue::List(vec![
            ConstValue::String("count".to_string()),
            ConstValue::Number(1.into()),
        ]);
        let normalized = normalize_command_result(&RedisOperation::Hgetall, value.clone());
        assert_eq!(normalized, value);
    }

    #[test]
    fn normalize_hgetall_folds_empty_list_into_empty_object() {
        // Redis returns an empty array for a missing key's HGETALL; zero
        // pairs unambiguously means an empty hash, so this folds safely
        // (unlike an odd-length list, which is truly malformed).
        let normalized =
            normalize_command_result(&RedisOperation::Hgetall, ConstValue::List(vec![]));
        assert_eq!(normalized, ConstValue::Object(indexmap::IndexMap::new()));
    }

    #[test]
    fn normalize_exists_converts_positive_count_to_true() {
        let normalized =
            normalize_command_result(&RedisOperation::Exists, ConstValue::Number(1.into()));
        assert_eq!(normalized, ConstValue::Boolean(true));
    }

    #[test]
    fn normalize_exists_converts_zero_count_to_false() {
        let normalized =
            normalize_command_result(&RedisOperation::Exists, ConstValue::Number(0.into()));
        assert_eq!(normalized, ConstValue::Boolean(false));
    }

    #[test]
    fn normalize_exists_converts_count_greater_than_one_to_true() {
        // EXISTS with multiple key arguments can return a count > 1.
        let normalized =
            normalize_command_result(&RedisOperation::Exists, ConstValue::Number(3.into()));
        assert_eq!(normalized, ConstValue::Boolean(true));
    }

    #[test]
    fn normalize_exists_passes_through_resp3_boolean() {
        let normalized =
            normalize_command_result(&RedisOperation::Exists, ConstValue::Boolean(true));
        assert_eq!(normalized, ConstValue::Boolean(true));
    }

    #[test]
    fn normalize_set_converts_ok_to_true() {
        let normalized =
            normalize_command_result(&RedisOperation::Set, ConstValue::String("OK".to_string()));
        assert_eq!(normalized, ConstValue::Boolean(true));
    }

    #[test]
    fn normalize_set_converts_null_to_false() {
        let normalized = normalize_command_result(&RedisOperation::Set, ConstValue::Null);
        assert_eq!(normalized, ConstValue::Boolean(false));
    }

    #[test]
    fn normalize_set_passes_through_resp3_boolean() {
        let normalized = normalize_command_result(&RedisOperation::Set, ConstValue::Boolean(true));
        assert_eq!(normalized, ConstValue::Boolean(true));
    }

    #[test]
    fn normalize_command_result_passes_through_unrelated_operations() {
        let value = ConstValue::String(r#"{"a":1}"#.to_string());
        let normalized = normalize_command_result(&RedisOperation::Get, value.clone());
        assert_eq!(normalized, value);

        let list = ConstValue::List(vec![ConstValue::String("a".to_string())]);
        let normalized = normalize_command_result(&RedisOperation::Lrange, list.clone());
        assert_eq!(normalized, list);
    }
}
