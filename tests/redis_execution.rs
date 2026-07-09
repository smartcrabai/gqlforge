//! Integration tests exercising the `IO::Redis` execution path end-to-end:
//! `Blueprint::try_from` -> `eval_io` -> `RedisIO` -> `decode_value_leaves`.
//!
//! Unlike the generic `tests/execution/*.md` specs (which always run with an
//! empty `TargetRuntime.redis` map and therefore cannot exercise real query
//! results), this file wires a `MockRedisIO` directly into the runtime so
//! query/mutation results and the exact `(command, args)` sent to Redis can
//! be asserted on.

#[cfg(test)]
mod redis_execution_spec {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use gqlforge::core::app_context::AppContext;
    use gqlforge::core::blueprint::Blueprint;
    use gqlforge::core::cache::InMemoryCache;
    use gqlforge::core::config::{Config, ConfigModule};
    use gqlforge::core::http::RequestContext;
    use gqlforge::core::jit::{ConstValueExecutor, Request as JitRequest};
    use gqlforge::core::redis::RedisIO;
    use gqlforge::core::rest::EndpointSet;
    use gqlforge::core::runtime::TargetRuntime;
    use gqlforge::core::{EnvIO, FileIO, HttpIO};
    use gqlforge_valid::Validator;
    use gqlrs_value::ConstValue;

    // ----------------------------------------------------------------
    // Mock RedisIO (self-contained, mirrors `tests/core/postgres.rs`'s
    // `MockPostgresIO`, extended to record every call so tests can assert on
    // the rendered `(command, args)`).
    // ----------------------------------------------------------------

    struct MockRedisIO {
        responses: HashMap<String, ConstValue>,
        default_response: ConstValue,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl MockRedisIO {
        fn new(response: ConstValue) -> Arc<Self> {
            Arc::new(Self {
                responses: HashMap::new(),
                default_response: response,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn with_responses(
            responses: HashMap<String, ConstValue>,
            default_response: ConstValue,
        ) -> Arc<Self> {
            Arc::new(Self { responses, default_response, calls: Mutex::new(Vec::new()) })
        }

        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }

        fn last_call(&self) -> Option<(String, Vec<String>)> {
            self.calls.lock().unwrap().last().cloned()
        }
    }

    #[async_trait::async_trait]
    impl RedisIO for MockRedisIO {
        async fn execute(&self, command: &str, args: &[String]) -> anyhow::Result<ConstValue> {
            self.calls
                .lock()
                .unwrap()
                .push((command.to_string(), args.to_vec()));

            Ok(self
                .responses
                .get(command)
                .cloned()
                .unwrap_or_else(|| self.default_response.clone()))
        }
    }

    // ----------------------------------------------------------------
    // Minimal runtime stubs
    //
    // None of these tests exercise HTTP/file/env IO, so the implementations
    // below only need to satisfy the `TargetRuntime` field types; they are
    // never actually invoked.
    // ----------------------------------------------------------------

    #[derive(Clone, Default)]
    struct UnusedHttp;

    #[async_trait::async_trait]
    impl HttpIO for UnusedHttp {
        async fn execute(
            &self,
            _request: reqwest::Request,
        ) -> anyhow::Result<gqlforge::core::http::Response<bytes::Bytes>> {
            Err(anyhow::anyhow!("HTTP IO is not used in this test"))
        }
    }

    #[derive(Clone, Default)]
    struct UnusedFileIO;

    #[async_trait::async_trait]
    impl FileIO for UnusedFileIO {
        async fn write<'a>(&'a self, _path: &'a str, _content: &'a [u8]) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("File IO is not used in this test"))
        }

        async fn read<'a>(&'a self, _path: &'a str) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("File IO is not used in this test"))
        }
    }

    #[derive(Clone, Default)]
    struct UnusedEnvIO;

    impl EnvIO for UnusedEnvIO {
        fn get(&self, _key: &str) -> Option<std::borrow::Cow<'_, str>> {
            None
        }
    }

    // ----------------------------------------------------------------
    // Test schema and app-context builder
    // ----------------------------------------------------------------

    const SCHEMA: &str = r#"schema
  @server(port: 0)
  @link(type: Redis, src: "redis://localhost:6379") {
  query: Query
  mutation: Mutation
}

type Query {
  getValue(id: ID!): JSON @redis(key: "user:{{.args.id}}")
  getHash(id: ID!): JSON @redis(operation: HGETALL, key: "user:{{.args.id}}")
  keyExists(key: String!): Boolean! @redis(operation: EXISTS, key: "{{.args.key}}")
  jobs: JSON @redis(operation: LRANGE, key: "queue:jobs")
}

type Mutation {
  setValue(id: ID!, value: String!): Boolean
    @redis(operation: SET, key: "user:{{.args.id}}", value: "{{.args.value}}")
}
"#;

    fn build_app_ctx(mock: Arc<MockRedisIO>) -> Arc<AppContext> {
        let config = Config::from_sdl(SCHEMA).to_result().unwrap();
        let config_module = ConfigModule::from(config);
        let blueprint = Blueprint::try_from(&config_module).unwrap();

        let mut redis: HashMap<String, Arc<dyn RedisIO>> = HashMap::new();
        redis.insert("default".to_string(), mock);

        let runtime = TargetRuntime {
            http: Arc::new(UnusedHttp),
            http2_only: Arc::new(UnusedHttp),
            env: Arc::new(UnusedEnvIO),
            file: Arc::new(UnusedFileIO),
            cache: Arc::new(InMemoryCache::default()),
            extensions: Arc::new(vec![]),
            cmd_worker: None,
            worker: None,
            postgres: HashMap::new(),
            postgres_listeners: HashMap::new(),
            redis,
            redis_listeners: HashMap::new(),
            s3: HashMap::new(),
        };

        Arc::new(AppContext::new(blueprint, runtime, EndpointSet::default()))
    }

    /// Executes a GraphQL request against `app_ctx`, wiring up the
    /// `Arc<RequestContext>` that field resolvers require (mirrors what
    /// `handle_request`/`handle_sse_request` do for real requests).
    async fn execute(app_ctx: &AppContext, request: gqlrs::Request) -> gqlrs::Response {
        let req_ctx = Arc::new(RequestContext::from(app_ctx));
        app_ctx.execute(request.data(req_ctx)).await
    }

    /// Executes a GraphQL request through the **JIT** engine
    /// (`ConstValueExecutor`/`Synth`), i.e. the same code path the real HTTP
    /// server uses for queries and mutations (see
    /// `core::http::request_handler::execute_query`, which drives requests
    /// through `JITExecutor` -> `ConstValueExecutor`). `execute()` above
    /// instead goes through `gqlrs::dynamic::Schema::execute`, which
    /// is only used for subscriptions in real serving; tests that must
    /// observe genuine JIT synth behavior (e.g. the scalar/list shape guard
    /// in `core::jit::synth::synth`) need this helper instead.
    async fn execute_jit(app_ctx: &Arc<AppContext>, query: &str) -> serde_json::Value {
        let req_ctx = RequestContext::from(app_ctx.as_ref());
        let jit_request = JitRequest::<ConstValue>::from(gqlrs::Request::new(query));
        let exec = ConstValueExecutor::try_new(&jit_request, app_ctx).unwrap();
        let response = exec.execute(app_ctx, &req_ctx, jit_request).await;
        serde_json::from_slice(&response.body).unwrap()
    }

    // ----------------------------------------------------------------
    // Tests
    // ----------------------------------------------------------------

    /// GET with `payloadType: JSON` (the default): a JSON-encoded string
    /// response from Redis is decoded into a GraphQL object. Also verifies
    /// that `{{.args.id}}` is expanded from a GraphQL query *variable*
    /// (not just an inline literal) into the Redis key argument.
    #[tokio::test]
    async fn test_redis_get_decodes_json_payload_and_expands_variable_into_key() {
        let mock = MockRedisIO::new(ConstValue::String(
            r#"{"name":"Alice","age":30}"#.to_string(),
        ));
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new("query($id: ID!) { getValue(id: $id) }").variables(
            gqlrs::Variables::from_json(serde_json::json!({ "id": "42" })),
        );

        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "getValue": { "name": "Alice", "age": 30 } })
        );

        assert_eq!(
            mock.last_call(),
            Some(("GET".to_string(), vec!["user:42".to_string()]))
        );
    }

    /// HGETALL: the driver-level `redis::Value::Map` conversion already
    /// produces a `ConstValue::Object`; `decode_value_leaves` must pass it
    /// through unchanged (aside from decoding any JSON string leaves).
    #[tokio::test]
    async fn test_redis_hgetall_returns_object() {
        let mock = MockRedisIO::new(ConstValue::Object(
            [
                (
                    gqlrs::Name::new("name"),
                    ConstValue::String("Bob".to_string()),
                ),
                (
                    gqlrs::Name::new("age"),
                    ConstValue::String("25".to_string()),
                ),
            ]
            .into(),
        ));
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new(r#"query { getHash(id: "7") }"#);
        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        // `decode_value_leaves` runs with `payloadType: JSON` (the field's
        // default), so the numeric-looking string leaf "25" is decoded into
        // a JSON number, while "Bob" (not valid JSON) stays a string.
        assert_eq!(
            json,
            serde_json::json!({ "getHash": { "name": "Bob", "age": 25 } })
        );

        assert_eq!(
            mock.last_call(),
            Some(("HGETALL".to_string(), vec!["user:7".to_string()]))
        );
    }

    /// SET mutation: Redis' `+OK` simple string is converted to
    /// `ConstValue::String("OK")` by the driver, and `normalize_command_result`
    /// turns it into `Boolean(true)` to match the field's declared `Boolean`
    /// return type (Bug C).
    #[tokio::test]
    async fn test_redis_set_mutation_returns_true() {
        let mock = MockRedisIO::new(ConstValue::String("OK".to_string()));
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new(r#"mutation { setValue(id: "9", value: "hello") }"#);
        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(json, serde_json::json!({ "setValue": true }));

        assert_eq!(
            mock.last_call(),
            Some((
                "SET".to_string(),
                vec!["user:9".to_string(), "hello".to_string()]
            ))
        );
    }

    /// EXISTS: Redis replies with an integer count; `normalize_command_result`
    /// converts it to `Boolean` to match the field's declared `Boolean!`
    /// return type (Bug C).
    #[tokio::test]
    async fn test_redis_exists_returns_boolean() {
        let mock = MockRedisIO::new(ConstValue::Number(1.into()));
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new(r#"query { keyExists(key: "user:1") }"#);
        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(json, serde_json::json!({ "keyExists": true }));
    }

    #[tokio::test]
    async fn test_redis_exists_zero_count_returns_false() {
        let mock = MockRedisIO::new(ConstValue::Number(0.into()));
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new(r#"query { keyExists(key: "missing") }"#);
        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(json, serde_json::json!({ "keyExists": false }));
    }

    /// HGETALL over RESP2 (the default protocol): the driver returns a flat
    /// `[field, value, field, value, ...]` list rather than a map;
    /// `normalize_command_result` folds it into an object before
    /// `decode_value_leaves` runs (Bug B-1).
    #[tokio::test]
    async fn test_redis_hgetall_resp2_flat_list_becomes_object() {
        let mock = MockRedisIO::new(ConstValue::List(vec![
            ConstValue::String("name".to_string()),
            ConstValue::String("Bob".to_string()),
            ConstValue::String("age".to_string()),
            ConstValue::String("25".to_string()),
        ]));
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new(r#"query { getHash(id: "7") }"#);
        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "getHash": { "name": "Bob", "age": 25 } })
        );
    }

    /// LRANGE result on a `JSON`-typed field: the JIT synth layer must not
    /// null out a scalar field just because its resolved value is an array
    /// (Bug B-2). Also exercises the full stack, not just the synth unit
    /// test in `src/core/jit/synth/synth.rs`.
    #[tokio::test]
    async fn test_redis_lrange_on_json_field_returns_array() {
        let mock = MockRedisIO::new(ConstValue::List(vec![
            ConstValue::String("job-1".to_string()),
            ConstValue::String("job-2".to_string()),
        ]));
        let app_ctx = build_app_ctx(mock.clone());

        // Uses `execute_jit`, not `execute`: this must exercise the real JIT
        // synth path (`core::jit::synth::synth`) that the actual HTTP server
        // uses for queries, since that's exactly where Bug B-2's shape guard
        // lived.
        let json = execute_jit(&app_ctx, "query { jobs }").await;
        assert_eq!(
            json.get("errors"),
            None,
            "unexpected GraphQL errors: {json:?}"
        );
        assert_eq!(
            json["data"],
            serde_json::json!({ "jobs": ["job-1", "job-2"] })
        );
    }

    /// Per-command canned responses: verifies `MockRedisIO::with_responses`
    /// dispatches by command name and that multiple distinct fields
    /// resolved in the same request each observe their own response.
    #[tokio::test]
    async fn test_redis_with_responses_dispatches_by_command() {
        let mut responses = HashMap::new();
        responses.insert(
            "GET".to_string(),
            ConstValue::String(r#"{"name":"Carol"}"#.to_string()),
        );
        responses.insert(
            "HGETALL".to_string(),
            ConstValue::Object(
                [(
                    gqlrs::Name::new("role"),
                    ConstValue::String("admin".to_string()),
                )]
                .into(),
            ),
        );
        let mock = MockRedisIO::with_responses(responses, ConstValue::Null);
        let app_ctx = build_app_ctx(mock.clone());

        let request = gqlrs::Request::new(r#"query { getValue(id: "1") getHash(id: "1") }"#);
        let response = execute(&app_ctx, request).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "getValue": { "name": "Carol" },
                "getHash": { "role": "admin" }
            })
        );

        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.contains(&("GET".to_string(), vec!["user:1".to_string()])));
        assert!(calls.contains(&("HGETALL".to_string(), vec!["user:1".to_string()])));
    }
}
