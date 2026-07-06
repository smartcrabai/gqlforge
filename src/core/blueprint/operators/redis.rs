use gqlforge_valid::{Valid, Validator};

use crate::core::blueprint::BlueprintError;
use crate::core::config::{ConfigModule, GraphQLOperationType, Redis, RedisOperation};
use crate::core::ir::model::{IO, IR};
use crate::core::mustache::Mustache;
use crate::core::redis::RedisStreamSource;
use crate::core::redis::request_template::RequestTemplate;

#[derive(Clone, Copy)]
pub struct CompileRedis<'a> {
    pub config_module: &'a ConfigModule,
    pub redis: &'a Redis,
    pub operation_type: &'a GraphQLOperationType,
}

#[must_use]
#[expect(clippy::too_many_lines)]
pub fn compile_redis(inputs: CompileRedis) -> Valid<IR, BlueprintError> {
    let redis = inputs.redis;
    let operation_type = inputs.operation_type;
    let dedupe = redis.dedupe.unwrap_or_default();

    // Resolve the connection id.
    let connection_id = if let Some(id) = &redis.db {
        id.clone()
    } else {
        let redis_links: Vec<_> = inputs
            .config_module
            .config()
            .links
            .iter()
            .filter(|link| link.type_of == crate::core::config::LinkType::Redis)
            .collect();
        if redis_links.len() == 1 {
            redis_links[0]
                .id
                .clone()
                .unwrap_or_else(|| "default".to_string())
        } else if redis_links.is_empty() {
            "default".to_string()
        } else {
            return Valid::fail(BlueprintError::Cause(
                "@redis requires 'db' when multiple Redis connections are defined".to_string(),
            ));
        }
    };

    let is_subscription = matches!(operation_type, GraphQLOperationType::Subscription);

    // SUBSCRIBE/XREAD are subscription-only operations.
    if matches!(
        redis.operation,
        RedisOperation::Subscribe | RedisOperation::Xread
    ) {
        if !is_subscription {
            return Valid::fail(BlueprintError::Cause(format!(
                "@redis(operation: {}) is only allowed on Subscription fields",
                operation_keyword(&redis.operation)
            )));
        }

        return match redis.operation {
            RedisOperation::Subscribe => {
                let channel = match redis.channel.as_deref() {
                    Some(c) if !c.is_empty() => c.to_string(),
                    _ => {
                        return Valid::fail(BlueprintError::Cause(
                            "@redis(operation: SUBSCRIBE) requires a non-empty 'channel'"
                                .to_string(),
                        ));
                    }
                };
                let payload_type = redis.payload_type.clone();
                Valid::succeed(IR::IO(Box::new(IO::RedisStream {
                    connection_id,
                    source: RedisStreamSource::PubSub { channel: Mustache::parse(&channel) },
                    payload_type,
                })))
            }
            RedisOperation::Xread => {
                let key = match redis.key.as_deref() {
                    Some(k) if !k.is_empty() => k.to_string(),
                    _ => {
                        return Valid::fail(BlueprintError::Cause(
                            "@redis(operation: XREAD) requires a non-empty 'key'".to_string(),
                        ));
                    }
                };
                let start_id = redis.start_id.as_deref().unwrap_or("$").to_string();
                let payload_type = redis.payload_type.clone();
                Valid::succeed(IR::IO(Box::new(IO::RedisStream {
                    connection_id,
                    source: RedisStreamSource::Stream {
                        key: Mustache::parse(&key),
                        start_id: Mustache::parse(&start_id),
                    },
                    payload_type,
                })))
            }
            _ => unreachable!("checked by outer match"),
        };
    }

    // Non-SUBSCRIBE/XREAD operations on Subscription fields are not allowed.
    if is_subscription {
        return Valid::fail(BlueprintError::Cause(
            "@redis on Subscription requires operation: SUBSCRIBE or XREAD".to_string(),
        ));
    }

    // Validate required fields per operation.
    let op = &redis.operation;
    let required_valid = match redis.operation {
        RedisOperation::Get
        | RedisOperation::Del
        | RedisOperation::Exists
        | RedisOperation::Incr
        | RedisOperation::Hgetall
        | RedisOperation::Smembers
        | RedisOperation::Lrange => require(redis.key.is_some(), "key", op),
        RedisOperation::Set
        | RedisOperation::Lpush
        | RedisOperation::Rpush
        | RedisOperation::Sadd
        | RedisOperation::Xadd => {
            require(redis.key.is_some(), "key", op).and(require(redis.value.is_some(), "value", op))
        }
        RedisOperation::Hget => {
            require(redis.key.is_some(), "key", op).and(require(redis.field.is_some(), "field", op))
        }
        RedisOperation::Hset => require(redis.key.is_some(), "key", op)
            .and(require(redis.field.is_some(), "field", op))
            .and(require(redis.value.is_some(), "value", op)),
        RedisOperation::Publish => require(redis.channel.is_some(), "channel", op).and(require(
            redis.value.is_some(),
            "value",
            op,
        )),
        RedisOperation::Subscribe | RedisOperation::Xread => {
            unreachable!("handled above")
        }
    };

    required_valid.map(|()| {
        let key = redis.key.as_ref().map(|v| Mustache::parse(v));
        let field = redis.field.as_ref().map(|v| Mustache::parse(v));
        let value = redis.value.as_ref().map(|v| Mustache::parse(v));
        let ttl = redis.ttl.as_ref().map(|v| Mustache::parse(v));
        let start = redis.start.as_ref().map(|v| Mustache::parse(v));
        let stop = redis.stop.as_ref().map(|v| Mustache::parse(v));
        let channel = redis.channel.as_ref().map(|v| Mustache::parse(v));

        let req_template = RequestTemplate {
            operation: redis.operation.clone(),
            key,
            field,
            value,
            ttl,
            start,
            stop,
            channel,
            payload_type: redis.payload_type.clone(),
        };

        IR::IO(Box::new(IO::Redis { req_template, dedupe, connection_id }))
    })
}

fn require(present: bool, name: &str, operation: &RedisOperation) -> Valid<(), BlueprintError> {
    if present {
        Valid::succeed(())
    } else {
        Valid::fail(BlueprintError::Cause(format!(
            "@redis(operation: {}) requires '{name}'",
            operation_keyword(operation)
        )))
    }
}

/// The `SCREAMING_SNAKE_CASE` keyword used in the GraphQL schema for a given
/// operation (matches the `@redis(operation: ...)` enum value).
fn operation_keyword(operation: &RedisOperation) -> &'static str {
    match operation {
        RedisOperation::Get => "GET",
        RedisOperation::Set => "SET",
        RedisOperation::Del => "DEL",
        RedisOperation::Exists => "EXISTS",
        RedisOperation::Incr => "INCR",
        RedisOperation::Hget => "HGET",
        RedisOperation::Hset => "HSET",
        RedisOperation::Hgetall => "HGETALL",
        RedisOperation::Lpush => "LPUSH",
        RedisOperation::Rpush => "RPUSH",
        RedisOperation::Lrange => "LRANGE",
        RedisOperation::Sadd => "SADD",
        RedisOperation::Smembers => "SMEMBERS",
        RedisOperation::Publish => "PUBLISH",
        RedisOperation::Xadd => "XADD",
        RedisOperation::Subscribe => "SUBSCRIBE",
        RedisOperation::Xread => "XREAD",
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use gqlforge_valid::Validator;

    use super::*;
    use crate::core::config::{Config, Link, LinkType};
    use crate::core::redis::RedisPayloadType;

    fn make_config_module(links: Vec<Link>) -> ConfigModule {
        let config = Config { links, ..Default::default() };
        ConfigModule::from(config)
    }

    fn redis_link(id: Option<&str>) -> Link {
        Link {
            type_of: LinkType::Redis,
            src: "redis://localhost:6379".to_string(),
            id: id.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn get_succeeds_with_key() {
        let cm = make_config_module(vec![]);
        let redis = Redis { key: Some("user:1".to_string()), ..Default::default() };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_ok());
    }

    #[test]
    fn get_without_key_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis::default();
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'key'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn set_without_value_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Set,
            key: Some("user:1".to_string()),
            value: None,
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'value'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn hget_without_field_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Hget,
            key: Some("user:1".to_string()),
            field: None,
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'field'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn hget_succeeds_and_returns_io_redis() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Hget,
            key: Some("user:1".to_string()),
            field: Some("name".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Redis { connection_id, .. } => assert_eq!(connection_id, "default"),
                other => panic!("Expected IO::Redis, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn hset_without_field_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Hset,
            key: Some("user:1".to_string()),
            field: None,
            value: Some("Alice".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'field'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn hset_without_value_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Hset,
            key: Some("user:1".to_string()),
            field: Some("name".to_string()),
            value: None,
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'value'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn hset_succeeds_and_returns_io_redis() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Hset,
            key: Some("user:1".to_string()),
            field: Some("name".to_string()),
            value: Some("Alice".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Redis { connection_id, .. } => assert_eq!(connection_id, "default"),
                other => panic!("Expected IO::Redis, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn lrange_succeeds_with_key_only() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Lrange,
            key: Some("mylist".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_ok());
    }

    #[test]
    fn publish_without_channel_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Publish,
            value: Some("hello".to_string()),
            channel: None,
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'channel'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn publish_succeeds() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Publish,
            channel: Some("events".to_string()),
            value: Some("hello".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        assert!(result.to_result().is_ok());
    }

    #[test]
    fn xadd_succeeds() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Xadd,
            key: Some("mystream".to_string()),
            value: Some(r#"{"a": "b"}"#.to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        assert!(result.to_result().is_ok());
    }

    #[test]
    fn subscribe_on_subscription_succeeds_and_returns_redis_stream() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Subscribe,
            channel: Some("events".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::RedisStream { connection_id, source, payload_type } => {
                    assert_eq!(connection_id, "default");
                    assert!(matches!(source, RedisStreamSource::PubSub { .. }));
                    assert_eq!(*payload_type, RedisPayloadType::Json);
                }
                other => panic!("Expected IO::RedisStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn subscribe_without_channel_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Subscribe,
            channel: None,
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("SUBSCRIBE) requires a non-empty 'channel'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn subscribe_on_query_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Subscribe,
            channel: Some("events".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("SUBSCRIBE) is only allowed on Subscription"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn xread_on_subscription_succeeds_and_returns_redis_stream() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Xread,
            key: Some("mystream".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::RedisStream { source, .. } => {
                    assert!(matches!(source, RedisStreamSource::Stream { .. }));
                }
                other => panic!("Expected IO::RedisStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn xread_without_key_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Xread,
            key: None,
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("XREAD) requires a non-empty 'key'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn xread_on_mutation_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis {
            operation: RedisOperation::Xread,
            key: Some("mystream".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("XREAD) is only allowed on Subscription"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn get_on_subscription_fails() {
        let cm = make_config_module(vec![]);
        let redis = Redis { key: Some("k".to_string()), ..Default::default() };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("@redis on Subscription requires operation: SUBSCRIBE or XREAD"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn multiple_redis_links_no_db_fails() {
        let cm = make_config_module(vec![redis_link(Some("cache")), redis_link(Some("pubsub"))]);
        let redis = Redis { key: Some("k".to_string()), ..Default::default() };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("requires 'db'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn multiple_redis_links_with_db_succeeds() {
        let cm = make_config_module(vec![redis_link(Some("cache")), redis_link(Some("pubsub"))]);
        let redis = Redis {
            key: Some("k".to_string()),
            db: Some("cache".to_string()),
            ..Default::default()
        };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Redis { connection_id, .. } => assert_eq!(connection_id, "cache"),
                other => panic!("Expected IO::Redis, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn single_redis_link_no_db_resolves_to_link_id() {
        let cm = make_config_module(vec![redis_link(Some("cache"))]);
        let redis = Redis { key: Some("k".to_string()), ..Default::default() };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Redis { connection_id, .. } => assert_eq!(connection_id, "cache"),
                other => panic!("Expected IO::Redis, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    /// Boundary case: a single `@link(type: Redis)` with neither an `id`
    /// nor `@redis(db: ...)` set on the field must still resolve to the
    /// `"default"` connection id (mirrors the zero-links case, since an
    /// id-less link is indistinguishable from "no link" for this purpose).
    #[test]
    fn single_redis_link_no_id_no_db_resolves_to_default() {
        let cm = make_config_module(vec![redis_link(None)]);
        let redis = Redis { key: Some("k".to_string()), ..Default::default() };
        let result = compile_redis(CompileRedis {
            config_module: &cm,
            redis: &redis,
            operation_type: &GraphQLOperationType::Query,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Redis { connection_id, .. } => assert_eq!(connection_id, "default"),
                other => panic!("Expected IO::Redis, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }
}
