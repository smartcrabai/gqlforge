use gqlforge_macros::{DirectiveDefinition, InputDefinition};
use serde::{Deserialize, Serialize};

use crate::core::is_default;

/// The command to run against Redis for a `@redis` directive.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum_macros::Display,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RedisOperation {
    /// GET the value of a key.
    #[default]
    Get,
    /// SET a key to a value, optionally with an expiry.
    Set,
    /// DEL a key.
    Del,
    /// EXISTS checks whether a key is present.
    Exists,
    /// INCR atomically increments a key.
    Incr,
    /// HGET reads a field from a hash.
    Hget,
    /// HSET writes a field in a hash.
    Hset,
    /// HGETALL reads all fields of a hash.
    Hgetall,
    /// LPUSH prepends a value to a list.
    Lpush,
    /// RPUSH appends a value to a list.
    Rpush,
    /// LRANGE reads a range of elements from a list.
    Lrange,
    /// SADD adds a value to a set.
    Sadd,
    /// SMEMBERS reads all values of a set.
    Smembers,
    /// PUBLISH sends a message on a channel.
    Publish,
    /// XADD appends an entry to a stream.
    Xadd,
    /// SUBSCRIBE listens to a Pub/Sub channel (Subscription fields only).
    Subscribe,
    /// XREAD streams new entries from a Redis Stream (Subscription fields
    /// only).
    Xread,
}

/// How to interpret Redis string payloads (e.g. GET results, PUBLISH
/// messages, stream entries).
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum_macros::Display,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RedisPayloadType {
    /// Parse the payload as JSON.
    #[default]
    Json,
    /// Treat the payload as a raw string, wrapped in `ConstValue::String`.
    Text,
}

/// The `@redis` directive maps a GraphQL field to a Redis command.
///
/// Supports key-value, hash, list, set, Pub/Sub, and Stream operations with
/// Mustache-templated arguments.
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    schemars::JsonSchema,
    InputDefinition,
    DirectiveDefinition,
)]
#[directive_definition(repeatable, locations = "FieldDefinition")]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Redis {
    /// The `@link(type: Redis)` id to use. Optional when only one Redis link
    /// is defined.
    #[serde(default, skip_serializing_if = "is_default")]
    pub db: Option<String>,

    /// The Redis command to run.
    #[serde(default, skip_serializing_if = "is_default")]
    pub operation: RedisOperation,

    /// The key to operate on. Supports Mustache templates.
    #[serde(default, skip_serializing_if = "is_default")]
    pub key: Option<String>,

    /// The hash field to operate on (HGET/HSET). Supports Mustache templates.
    #[serde(default, skip_serializing_if = "is_default")]
    pub field: Option<String>,

    /// The value to write (SET/HSET/LPUSH/RPUSH/SADD/PUBLISH/XADD). Supports
    /// Mustache templates.
    #[serde(default, skip_serializing_if = "is_default")]
    pub value: Option<String>,

    /// Expiry in seconds for SET (`EX <ttl>`). Supports Mustache templates.
    #[serde(default, skip_serializing_if = "is_default")]
    pub ttl: Option<String>,

    /// Start index for LRANGE. Defaults to "0". Supports Mustache templates.
    #[serde(default, skip_serializing_if = "is_default")]
    pub start: Option<String>,

    /// Stop index for LRANGE. Defaults to "-1". Supports Mustache templates.
    #[serde(default, skip_serializing_if = "is_default")]
    pub stop: Option<String>,

    /// The Pub/Sub channel to publish to or subscribe on.
    #[serde(default, skip_serializing_if = "is_default")]
    pub channel: Option<String>,

    /// The Redis Stream id to start reading from (XREAD). Defaults to "$".
    #[serde(default, skip_serializing_if = "is_default")]
    pub start_id: Option<String>,

    /// How to interpret string payloads. Defaults to `JSON`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub payload_type: RedisPayloadType,

    /// Enables deduplication of identical IO operations.
    #[serde(default, skip_serializing_if = "is_default")]
    pub dedupe: Option<bool>,
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn deserialize_get_operation_default() {
        let json = serde_json::json!({});
        let redis: Redis = serde_json::from_value(json).unwrap();
        assert_eq!(redis.operation, RedisOperation::Get);
        assert_eq!(redis.payload_type, RedisPayloadType::Json);
    }

    #[test]
    fn deserialize_hget_operation() {
        let json = serde_json::json!({
            "operation": "HGET",
            "key": "user:1",
            "field": "name"
        });
        let redis: Redis = serde_json::from_value(json).unwrap();
        assert_eq!(redis.operation, RedisOperation::Hget);
        assert_eq!(redis.field.as_deref(), Some("name"));
    }

    #[test]
    fn deserialize_xread_with_start_id_camel_case() {
        let json = serde_json::json!({
            "operation": "XREAD",
            "key": "events",
            "startId": "$"
        });
        let redis: Redis = serde_json::from_value(json).unwrap();
        assert_eq!(redis.operation, RedisOperation::Xread);
        assert_eq!(redis.start_id.as_deref(), Some("$"));
    }

    #[test]
    fn deserialize_payload_type_text() {
        let json = serde_json::json!({
            "operation": "SUBSCRIBE",
            "channel": "events",
            "payloadType": "TEXT"
        });
        let redis: Redis = serde_json::from_value(json).unwrap();
        assert_eq!(redis.payload_type, RedisPayloadType::Text);
    }

    #[test]
    fn deserialize_payload_type_lowercase_fails() {
        let json = serde_json::json!({
            "operation": "GET",
            "key": "k",
            "payloadType": "json"
        });
        let result: Result<Redis, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_unknown_field_fails() {
        let json = serde_json::json!({
            "operation": "GET",
            "key": "k",
            "bogus": true
        });
        let result: Result<Redis, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }
}
