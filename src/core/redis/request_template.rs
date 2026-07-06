use std::hash::{Hash, Hasher};

use gqlforge_hasher::GqlforgeHasher;

use crate::core::config::{RedisOperation, RedisPayloadType};
use crate::core::has_headers::HasHeaders;
use crate::core::ir::model::IoId;
use crate::core::mustache::Mustache;
use crate::core::path::PathString;

/// Template describing how to build a Redis command for a `@redis` field.
#[derive(Debug, Clone)]
pub struct RequestTemplate {
    pub operation: RedisOperation,
    pub key: Option<Mustache>,
    pub field: Option<Mustache>,
    pub value: Option<Mustache>,
    pub ttl: Option<Mustache>,
    pub start: Option<Mustache>,
    pub stop: Option<Mustache>,
    pub channel: Option<Mustache>,
    pub payload_type: RedisPayloadType,
}

/// A rendered, ready-to-execute Redis command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderedCommand {
    pub command: String,
    pub args: Vec<String>,
}

impl RequestTemplate {
    /// Render the template against the given context to produce a Redis
    /// command name and its positional arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn render<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedCommand> {
        match self.operation {
            RedisOperation::Get => self.render_key_only("GET", ctx),
            RedisOperation::Del => self.render_key_only("DEL", ctx),
            RedisOperation::Exists => self.render_key_only("EXISTS", ctx),
            RedisOperation::Incr => self.render_key_only("INCR", ctx),
            RedisOperation::Hgetall => self.render_key_only("HGETALL", ctx),
            RedisOperation::Smembers => self.render_key_only("SMEMBERS", ctx),
            RedisOperation::Set => self.render_set(ctx),
            RedisOperation::Hget => self.render_hget(ctx),
            RedisOperation::Hset => self.render_hset(ctx),
            RedisOperation::Lpush => self.render_key_value("LPUSH", ctx),
            RedisOperation::Rpush => self.render_key_value("RPUSH", ctx),
            RedisOperation::Sadd => self.render_key_value("SADD", ctx),
            RedisOperation::Lrange => self.render_lrange(ctx),
            RedisOperation::Publish => self.render_publish(ctx),
            RedisOperation::Xadd => self.render_xadd(ctx),
            RedisOperation::Subscribe | RedisOperation::Xread => {
                anyhow::bail!(
                    "SUBSCRIBE/XREAD are subscription-only operations and must not be rendered as a command"
                );
            }
        }
    }

    fn require<C: PathString + HasHeaders>(
        field: Option<&Mustache>,
        name: &str,
        ctx: &C,
    ) -> anyhow::Result<String> {
        field
            .map(|m| m.render(ctx))
            .ok_or_else(|| anyhow::anyhow!("Redis operation requires '{name}'"))
    }

    fn render_key_only<C: PathString + HasHeaders>(
        &self,
        command: &str,
        ctx: &C,
    ) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        Ok(RenderedCommand { command: command.to_string(), args: vec![key] })
    }

    fn render_key_value<C: PathString + HasHeaders>(
        &self,
        command: &str,
        ctx: &C,
    ) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        let value = Self::require(self.value.as_ref(), "value", ctx)?;
        Ok(RenderedCommand { command: command.to_string(), args: vec![key, value] })
    }

    fn render_set<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        let value = Self::require(self.value.as_ref(), "value", ctx)?;
        let mut args = vec![key, value];

        if let Some(ttl) = &self.ttl {
            let rendered = ttl.render(ctx);
            if !rendered.is_empty() {
                args.push("EX".to_string());
                args.push(rendered);
            }
        }

        Ok(RenderedCommand { command: "SET".to_string(), args })
    }

    fn render_hget<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        let field = Self::require(self.field.as_ref(), "field", ctx)?;
        Ok(RenderedCommand { command: "HGET".to_string(), args: vec![key, field] })
    }

    fn render_hset<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        let field = Self::require(self.field.as_ref(), "field", ctx)?;
        let value = Self::require(self.value.as_ref(), "value", ctx)?;
        Ok(RenderedCommand { command: "HSET".to_string(), args: vec![key, field, value] })
    }

    fn render_lrange<C: PathString + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        let start = self
            .start
            .as_ref()
            .map(|m| m.render(ctx))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0".to_string());
        let stop = self
            .stop
            .as_ref()
            .map(|m| m.render(ctx))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-1".to_string());
        Ok(RenderedCommand { command: "LRANGE".to_string(), args: vec![key, start, stop] })
    }

    fn render_publish<C: PathString + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedCommand> {
        let channel = Self::require(self.channel.as_ref(), "channel", ctx)?;
        let value = Self::require(self.value.as_ref(), "value", ctx)?;
        Ok(RenderedCommand { command: "PUBLISH".to_string(), args: vec![channel, value] })
    }

    /// XADD expands a JSON object value into alternating field/value
    /// arguments. Non-object values fall back to a single "payload" field
    /// holding the raw rendered string.
    fn render_xadd<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedCommand> {
        let key = Self::require(self.key.as_ref(), "key", ctx)?;
        let value = Self::require(self.value.as_ref(), "value", ctx)?;

        let mut args = vec![key, "*".to_string()];

        if let Ok(serde_json::Value::Object(obj)) =
            serde_json::from_str::<serde_json::Value>(&value)
        {
            for (field, val) in obj {
                let val_str = match val {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                args.push(field);
                args.push(val_str);
            }
        } else {
            args.push("payload".to_string());
            args.push(value);
        }

        Ok(RenderedCommand { command: "XADD".to_string(), args })
    }
}

impl RequestTemplate {
    /// Builds a cache key from the rendered command *and* the connection it
    /// will run against.
    ///
    /// This is an inherent method rather than an `impl CacheKey<Ctx> for
    /// RequestTemplate` (unlike the other `RequestTemplate` types in this
    /// crate) because the generic `CacheKey::cache_key(&self, ctx)` has no
    /// room for the extra `connection_id` input: two `@redis(dedupe: true)`
    /// fields that render the identical command/args against *different*
    /// connections (e.g. `db: "cache"` vs `db: "sessions"`) must not hash
    /// to the same `IoId`, or one connection's cached/deduped result could
    /// be served in place of the other's.
    pub fn cache_key_with_connection<Ctx: PathString + HasHeaders>(
        &self,
        ctx: &Ctx,
        connection_id: &str,
    ) -> Option<IoId> {
        let rendered = self.render(ctx).ok()?;
        let mut hasher = GqlforgeHasher::default();
        rendered.hash(&mut hasher);
        connection_id.hash(&mut hasher);
        Some(IoId::new(hasher.finish()))
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::borrow::Cow;

    use http::HeaderMap;

    use super::*;

    struct Ctx {
        value: serde_json::Value,
    }

    impl PathString for Ctx {
        fn path_string<'a, T: AsRef<str>>(&'a self, parts: &'a [T]) -> Option<Cow<'a, str>> {
            self.value.path_string(parts)
        }
    }

    impl HasHeaders for Ctx {
        fn headers(&self) -> &HeaderMap {
            static EMPTY: std::sync::LazyLock<HeaderMap> = std::sync::LazyLock::new(HeaderMap::new);
            &EMPTY
        }
    }

    fn ctx() -> Ctx {
        Ctx { value: serde_json::Value::Null }
    }

    #[test]
    fn render_get() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Get,
            key: Some(Mustache::parse("user:1")),
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "GET");
        assert_eq!(rendered.args, vec!["user:1".to_string()]);
    }

    #[test]
    fn render_set_with_ttl() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Set,
            key: Some(Mustache::parse("user:1")),
            field: None,
            value: Some(Mustache::parse("Alice")),
            ttl: Some(Mustache::parse("60")),
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "SET");
        assert_eq!(
            rendered.args,
            vec![
                "user:1".to_string(),
                "Alice".to_string(),
                "EX".to_string(),
                "60".to_string()
            ]
        );
    }

    #[test]
    fn render_set_without_ttl() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Set,
            key: Some(Mustache::parse("user:1")),
            field: None,
            value: Some(Mustache::parse("Alice")),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(
            rendered.args,
            vec!["user:1".to_string(), "Alice".to_string()]
        );
    }

    /// Boundary case: `ttl` is `Some(Mustache)` (the directive did set a
    /// template), but it renders to an empty string because the
    /// referenced argument is absent from the context. This must behave
    /// exactly like `ttl: None` -- no `EX` clause -- rather than emitting
    /// `EX ""`.
    #[test]
    fn render_set_with_empty_rendered_ttl_omits_ex() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Set,
            key: Some(Mustache::parse("user:1")),
            field: None,
            value: Some(Mustache::parse("Alice")),
            ttl: Some(Mustache::parse("{{.args.ttl}}")),
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let ctx = Ctx { value: serde_json::json!({"args": {}}) };
        let rendered = tmpl.render(&ctx).unwrap();
        assert_eq!(rendered.command, "SET");
        assert_eq!(
            rendered.args,
            vec!["user:1".to_string(), "Alice".to_string()]
        );
    }

    #[test]
    fn render_hget() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Hget,
            key: Some(Mustache::parse("user:1")),
            field: Some(Mustache::parse("name")),
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "HGET");
        assert_eq!(
            rendered.args,
            vec!["user:1".to_string(), "name".to_string()]
        );
    }

    #[test]
    fn render_hset() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Hset,
            key: Some(Mustache::parse("user:1")),
            field: Some(Mustache::parse("name")),
            value: Some(Mustache::parse("Alice")),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "HSET");
        assert_eq!(
            rendered.args,
            vec![
                "user:1".to_string(),
                "name".to_string(),
                "Alice".to_string()
            ]
        );
    }

    #[test]
    fn render_lpush() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Lpush,
            key: Some(Mustache::parse("mylist")),
            field: None,
            value: Some(Mustache::parse("first")),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "LPUSH");
        assert_eq!(
            rendered.args,
            vec!["mylist".to_string(), "first".to_string()]
        );
    }

    #[test]
    fn render_rpush() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Rpush,
            key: Some(Mustache::parse("mylist")),
            field: None,
            value: Some(Mustache::parse("last")),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "RPUSH");
        assert_eq!(
            rendered.args,
            vec!["mylist".to_string(), "last".to_string()]
        );
    }

    #[test]
    fn render_sadd() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Sadd,
            key: Some(Mustache::parse("myset")),
            field: None,
            value: Some(Mustache::parse("member")),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "SADD");
        assert_eq!(
            rendered.args,
            vec!["myset".to_string(), "member".to_string()]
        );
    }

    #[test]
    fn render_lrange_defaults() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Lrange,
            key: Some(Mustache::parse("mylist")),
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "LRANGE");
        assert_eq!(
            rendered.args,
            vec!["mylist".to_string(), "0".to_string(), "-1".to_string()]
        );
    }

    #[test]
    fn render_lrange_custom_bounds() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Lrange,
            key: Some(Mustache::parse("mylist")),
            field: None,
            value: None,
            ttl: None,
            start: Some(Mustache::parse("2")),
            stop: Some(Mustache::parse("5")),
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(
            rendered.args,
            vec!["mylist".to_string(), "2".to_string(), "5".to_string()]
        );
    }

    /// Boundary case: `start`/`stop` are `Some(Mustache)` templates, but
    /// they render to empty strings because the referenced arguments are
    /// absent from the context. This must fall back to the same defaults
    /// as `start: None`/`stop: None` ("0"/"-1") rather than sending
    /// `LRANGE key "" ""`.
    #[test]
    fn render_lrange_empty_rendered_bounds_fall_back_to_defaults() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Lrange,
            key: Some(Mustache::parse("mylist")),
            field: None,
            value: None,
            ttl: None,
            start: Some(Mustache::parse("{{.args.start}}")),
            stop: Some(Mustache::parse("{{.args.stop}}")),
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let ctx = Ctx { value: serde_json::json!({"args": {}}) };
        let rendered = tmpl.render(&ctx).unwrap();
        assert_eq!(rendered.command, "LRANGE");
        assert_eq!(
            rendered.args,
            vec!["mylist".to_string(), "0".to_string(), "-1".to_string()]
        );
    }

    #[test]
    fn render_publish() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Publish,
            key: None,
            field: None,
            value: Some(Mustache::parse("hello")),
            ttl: None,
            start: None,
            stop: None,
            channel: Some(Mustache::parse("events")),
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "PUBLISH");
        assert_eq!(
            rendered.args,
            vec!["events".to_string(), "hello".to_string()]
        );
    }

    #[test]
    fn render_xadd_expands_json_object() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Xadd,
            key: Some(Mustache::parse("mystream")),
            field: None,
            value: Some(Mustache::parse(r#"{"name": "Alice", "age": 30}"#)),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(rendered.command, "XADD");
        assert_eq!(rendered.args[0], "mystream");
        assert_eq!(rendered.args[1], "*");
        // Remaining args are field/value pairs (order follows the JSON object).
        assert_eq!(rendered.args.len(), 6);
        assert!(rendered.args.contains(&"name".to_string()));
        assert!(rendered.args.contains(&"Alice".to_string()));
        assert!(rendered.args.contains(&"age".to_string()));
        assert!(rendered.args.contains(&"30".to_string()));
    }

    #[test]
    fn render_xadd_falls_back_to_payload_for_non_object() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Xadd,
            key: Some(Mustache::parse("mystream")),
            field: None,
            value: Some(Mustache::parse("plain string")),
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let rendered = tmpl.render(&ctx()).unwrap();
        assert_eq!(
            rendered.args,
            vec![
                "mystream".to_string(),
                "*".to_string(),
                "payload".to_string(),
                "plain string".to_string()
            ]
        );
    }

    #[test]
    fn render_mustache_expression() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Get,
            key: Some(Mustache::parse("user:{{.args.id}}")),
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let ctx = Ctx { value: serde_json::json!({"args": {"id": "42"}}) };
        let rendered = tmpl.render(&ctx).unwrap();
        assert_eq!(rendered.args, vec!["user:42".to_string()]);
    }

    #[test]
    fn render_missing_key_fails() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Get,
            key: None,
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let err = tmpl.render(&ctx()).unwrap_err();
        assert!(
            err.to_string().contains("requires 'key'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn render_subscribe_fails() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Subscribe,
            key: None,
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: Some(Mustache::parse("events")),
            payload_type: RedisPayloadType::Json,
        };
        let err = tmpl.render(&ctx()).unwrap_err();
        assert!(
            err.to_string().contains("subscription-only"),
            "unexpected error: {err}"
        );
    }

    /// Regression test for `IoId` collisions across Redis connections:
    /// `@redis(dedupe: true)` fields that render the exact same command
    /// against *different* connections (`db: "cache"` vs
    /// `db: "sessions"`) must not share a cache key, or one connection's
    /// deduped/cached result could be served in place of the other's.
    #[test]
    fn cache_key_with_connection_differs_by_connection_id() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Get,
            key: Some(Mustache::parse("user:1")),
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let key_a = tmpl.cache_key_with_connection(&ctx(), "cache").unwrap();
        let key_b = tmpl.cache_key_with_connection(&ctx(), "sessions").unwrap();
        assert_ne!(
            key_a, key_b,
            "same command on different connections must not collide"
        );
    }

    #[test]
    fn cache_key_with_connection_is_stable_for_same_input() {
        let tmpl = RequestTemplate {
            operation: RedisOperation::Get,
            key: Some(Mustache::parse("user:1")),
            field: None,
            value: None,
            ttl: None,
            start: None,
            stop: None,
            channel: None,
            payload_type: RedisPayloadType::Json,
        };
        let key_a = tmpl.cache_key_with_connection(&ctx(), "cache").unwrap();
        let key_b = tmpl.cache_key_with_connection(&ctx(), "cache").unwrap();
        assert_eq!(key_a, key_b, "same input must hash to the same IoId");
    }
}
