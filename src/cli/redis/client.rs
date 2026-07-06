use std::time::Duration;

use async_graphql_value::ConstValue;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use tokio::sync::OnceCell;

use crate::core::redis::RedisIO;

/// Response timeout for regular (non-`BLOCK`ing) Redis commands issued
/// through the shared `ConnectionManager`. The `redis` crate's own default
/// (~500ms) is tuned for low-latency commands and is too tight for
/// occasional slow commands or a momentarily busy server; 30s gives ample
/// headroom while still failing fast on a genuinely dead connection.
const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// A Redis client backed by `redis`'s auto-reconnecting `ConnectionManager`.
///
/// `redis::Client::open` builds a lightweight, unconnected handle, so
/// construction is synchronous. The actual TCP/TLS connection is established
/// lazily on the first `execute()` call and shared afterwards; `ConnectionManager`
/// is cheap to clone and safe to use concurrently from multiple callers.
pub struct RedisClientPool {
    client: redis::Client,
    manager: OnceCell<ConnectionManager>,
}

impl RedisClientPool {
    /// Create a new pool from a Redis connection URL (`redis://` or
    /// `rediss://`). This does not connect; the connection is established
    /// lazily on first use.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn new(connection_url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(connection_url)
            .map_err(|e| anyhow::anyhow!("Failed to create Redis client: {e}"))?;

        Ok(Self { client, manager: OnceCell::new() })
    }

    /// Returns a cloned handle to the shared `ConnectionManager`, connecting
    /// on first call.
    async fn manager(&self) -> anyhow::Result<ConnectionManager> {
        let manager = self
            .manager
            .get_or_try_init(|| async {
                let config = ConnectionManagerConfig::new()
                    .set_response_timeout(Some(COMMAND_RESPONSE_TIMEOUT));
                self.client
                    .get_connection_manager_with_config(config)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to connect to Redis: {e}"))
            })
            .await?;
        Ok(manager.clone())
    }
}

#[async_trait::async_trait]
impl RedisIO for RedisClientPool {
    async fn execute(&self, command: &str, args: &[String]) -> anyhow::Result<ConstValue> {
        let mut manager = self.manager().await?;

        let mut cmd = redis::cmd(command);
        for arg in args {
            cmd.arg(arg);
        }

        let value: redis::Value = cmd
            .query_async(&mut manager)
            .await
            .map_err(|e| anyhow::anyhow!("Redis command '{command}' failed: {e}"))?;

        redis_value_to_const(value)
    }
}

/// Convert a raw RESP `redis::Value` into a `ConstValue`.
///
/// This is a purely structural conversion. Interpreting string leaves as
/// JSON per the field's `RedisPayloadType` happens afterwards, in the core
/// layer's `decode_value_leaves`.
///
/// # Errors
///
/// Returns an error if `value` is a `ServerError`. In practice this should
/// already have been surfaced as an `Err` by `query_async` before a `Value`
/// is produced, but it is handled defensively here too.
pub(crate) fn redis_value_to_const(value: redis::Value) -> anyhow::Result<ConstValue> {
    use redis::Value;

    Ok(match value {
        Value::Int(n) => ConstValue::Number(n.into()),
        Value::Double(f) => {
            serde_json::Number::from_f64(f).map_or(ConstValue::Null, ConstValue::Number)
        }
        Value::Boolean(b) => ConstValue::Boolean(b),
        Value::Okay => ConstValue::String("OK".to_string()),
        Value::SimpleString(s) => ConstValue::String(s),
        Value::BulkString(bytes) => {
            ConstValue::String(String::from_utf8_lossy(&bytes).into_owned())
        }
        Value::Array(items) | Value::Set(items) => {
            let mut list = Vec::with_capacity(items.len());
            for item in items {
                list.push(redis_value_to_const(item)?);
            }
            ConstValue::List(list)
        }
        Value::Map(pairs) => {
            let mut map = indexmap::IndexMap::with_capacity(pairs.len());
            for (k, v) in pairs {
                let key = async_graphql::Name::new(redis_value_to_key(&k));
                map.insert(key, redis_value_to_const(v)?);
            }
            ConstValue::Object(map)
        }
        Value::BigNumber(bytes) => ConstValue::String(String::from_utf8_lossy(&bytes).into_owned()),
        Value::VerbatimString { text, .. } => ConstValue::String(text),
        // RESP3 attributes carry out-of-band metadata that `ConstValue` has
        // no room for; keep the payload and drop the attributes.
        Value::Attribute { data, .. } => redis_value_to_const(*data)?,
        // Out-of-band push messages (e.g. invalidation, RESP3 pub/sub) are
        // not expected on a command-execution connection, but degrade to a
        // plain list rather than failing the whole response.
        Value::Push { data, .. } => {
            let mut list = Vec::with_capacity(data.len());
            for item in data {
                list.push(redis_value_to_const(item)?);
            }
            ConstValue::List(list)
        }
        Value::ServerError(e) => anyhow::bail!("Redis server error: {e}"),
        // Covers `Value::Nil` as well as any variant added to this
        // `#[non_exhaustive]` enum in the future; fail safe rather than
        // breaking the whole response.
        _ => ConstValue::Null,
    })
}

/// Stringify a `redis::Value` used as a RESP3 Map key. Keys are almost
/// always strings or integers in practice; anything else falls back to its
/// debug representation rather than failing the conversion.
fn redis_value_to_key(value: &redis::Value) -> String {
    match value {
        redis::Value::SimpleString(s) => s.clone(),
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        redis::Value::Int(n) => n.to_string(),
        redis::Value::Okay => "OK".to_string(),
        redis::Value::Nil => "null".to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use redis::Value;

    use super::*;

    #[test]
    fn new_with_invalid_url_fails() {
        let result = RedisClientPool::new("not a url");
        assert!(result.is_err());
    }

    #[test]
    fn new_with_valid_url_succeeds() {
        let result = RedisClientPool::new("redis://127.0.0.1:6379");
        assert!(result.is_ok());
    }

    #[test]
    fn convert_nil() {
        assert_eq!(redis_value_to_const(Value::Nil).unwrap(), ConstValue::Null);
    }

    #[test]
    fn convert_int() {
        assert_eq!(
            redis_value_to_const(Value::Int(42)).unwrap(),
            ConstValue::Number(42.into())
        );
    }

    #[test]
    fn convert_double() {
        assert_eq!(
            redis_value_to_const(Value::Double(1.5)).unwrap(),
            ConstValue::Number(serde_json::Number::from_f64(1.5).unwrap())
        );
    }

    #[test]
    fn convert_double_nan_falls_back_to_null() {
        assert_eq!(
            redis_value_to_const(Value::Double(f64::NAN)).unwrap(),
            ConstValue::Null
        );
    }

    #[test]
    fn convert_boolean() {
        assert_eq!(
            redis_value_to_const(Value::Boolean(true)).unwrap(),
            ConstValue::Boolean(true)
        );
    }

    #[test]
    fn convert_okay() {
        assert_eq!(
            redis_value_to_const(Value::Okay).unwrap(),
            ConstValue::String("OK".to_string())
        );
    }

    #[test]
    fn convert_simple_string() {
        assert_eq!(
            redis_value_to_const(Value::SimpleString("hello".to_string())).unwrap(),
            ConstValue::String("hello".to_string())
        );
    }

    #[test]
    fn convert_bulk_string_valid_utf8() {
        assert_eq!(
            redis_value_to_const(Value::BulkString(b"hello".to_vec())).unwrap(),
            ConstValue::String("hello".to_string())
        );
    }

    #[test]
    fn convert_bulk_string_invalid_utf8_is_lossy() {
        let value = redis_value_to_const(Value::BulkString(vec![0xFF, 0xFE])).unwrap();
        match value {
            ConstValue::String(s) => assert!(s.contains('\u{FFFD}')),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn convert_array() {
        let value = redis_value_to_const(Value::Array(vec![Value::Int(1), Value::Int(2)])).unwrap();
        assert_eq!(
            value,
            ConstValue::List(vec![
                ConstValue::Number(1.into()),
                ConstValue::Number(2.into())
            ])
        );
    }

    #[test]
    fn convert_set() {
        let value = redis_value_to_const(Value::Set(vec![Value::Int(1), Value::Int(2)])).unwrap();
        assert_eq!(
            value,
            ConstValue::List(vec![
                ConstValue::Number(1.into()),
                ConstValue::Number(2.into())
            ])
        );
    }

    #[test]
    fn convert_map() {
        let value = redis_value_to_const(Value::Map(vec![(
            Value::BulkString(b"field".to_vec()),
            Value::BulkString(b"value".to_vec()),
        )]))
        .unwrap();
        match value {
            ConstValue::Object(map) => {
                assert_eq!(
                    map.get("field"),
                    Some(&ConstValue::String("value".to_string()))
                );
            }
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn convert_big_number() {
        assert_eq!(
            redis_value_to_const(Value::BigNumber(b"123456789012345678901234567890".to_vec()))
                .unwrap(),
            ConstValue::String("123456789012345678901234567890".to_string())
        );
    }

    #[test]
    fn convert_verbatim_string() {
        let value =
            Value::VerbatimString { format: redis::VerbatimFormat::Text, text: "hi".to_string() };
        assert_eq!(
            redis_value_to_const(value).unwrap(),
            ConstValue::String("hi".to_string())
        );
    }

    #[test]
    fn convert_attribute_unwraps_data() {
        let value = Value::Attribute {
            data: Box::new(Value::Int(7)),
            attributes: vec![(Value::SimpleString("ttl".to_string()), Value::Int(60))],
        };
        assert_eq!(
            redis_value_to_const(value).unwrap(),
            ConstValue::Number(7.into())
        );
    }

    #[test]
    fn convert_push_becomes_list() {
        let value = Value::Push {
            kind: redis::PushKind::Message,
            data: vec![Value::Int(1), Value::Int(2)],
        };
        assert_eq!(
            redis_value_to_const(value).unwrap(),
            ConstValue::List(vec![
                ConstValue::Number(1.into()),
                ConstValue::Number(2.into())
            ])
        );
    }

    #[test]
    fn convert_server_error_fails() {
        // Build a genuine `ServerError` the same way `redis`'s own tests do:
        // parse a real RESP error line and extract it, since `ServerError`
        // has no public constructor.
        let parsed = redis::parse_redis_value(b"-ERR boom\r\n").unwrap();
        let redis_error = parsed.extract_error().unwrap_err();
        let server_error = redis::ServerError::try_from(redis_error).unwrap();

        let err = redis_value_to_const(Value::ServerError(server_error)).unwrap_err();
        assert!(
            err.to_string().contains("Redis server error"),
            "unexpected error: {err}"
        );
    }
}
