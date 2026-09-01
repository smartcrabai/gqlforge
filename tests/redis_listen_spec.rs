#[cfg(test)]
mod redis_listen_spec {
    #![expect(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use gqlforge::cli::javascript::init_worker_io;
    use gqlforge::core::app_context::AppContext;
    use gqlforge::core::blueprint::{Blueprint, Script, Upstream};
    use gqlforge::core::cache::InMemoryCache;
    use gqlforge::core::config::RedisPayloadType;
    use gqlforge::core::config::reader::ConfigReader;
    use gqlforge::core::http::{RequestContext, Response};
    use gqlforge::core::redis::{RedisListenerIO, RedisStreamSource};
    use gqlforge::core::rest::EndpointSet;
    use gqlforge::core::runtime::TargetRuntime;
    use gqlforge::core::worker::{Command, Event};
    use gqlforge::core::{EnvIO, FileIO, HttpIO};
    use gqlforge_valid::Validator;
    use gqlrs_value::ConstValue;
    use reqwest::Client;
    use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::broadcast;

    // ----------------------------------------------------------------
    // Mock RedisListenerIO (self-contained, mirrors `MockPostgresListener`
    // in tests/postgres_listen_spec.rs; a broadcast-channel-backed Pub/Sub
    // plus a fixed-sequence replay for Streams).
    // ----------------------------------------------------------------

    #[derive(Default)]
    struct MockRedisListener {
        channels: HashMap<String, broadcast::Sender<ConstValue>>,
        streams: HashMap<String, Vec<ConstValue>>,
    }

    impl MockRedisListener {
        fn new() -> Self {
            Self::default()
        }

        fn with_channel(mut self, channel: &str, tx: broadcast::Sender<ConstValue>) -> Self {
            self.channels.insert(channel.to_string(), tx);
            self
        }

        fn with_stream(mut self, key: &str, entries: Vec<ConstValue>) -> Self {
            self.streams.insert(key.to_string(), entries);
            self
        }

        fn into_arc(self) -> Arc<Self> {
            Arc::new(self)
        }
    }

    #[async_trait::async_trait]
    impl RedisListenerIO for MockRedisListener {
        async fn subscribe(
            &self,
            channel: &str,
            _payload_type: RedisPayloadType,
        ) -> anyhow::Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<ConstValue, anyhow::Error>> + Send>,
            >,
        > {
            let tx = self
                .channels
                .get(channel)
                .ok_or_else(|| anyhow::anyhow!("channel '{channel}' not found in mock listener"))?
                .clone();
            let rx = tx.subscribe();
            Ok(Box::pin(futures_util::stream::unfold(
                rx,
                |mut rx| async move {
                    match rx.recv().await {
                        Ok(value) => Some((Ok(value), rx)),
                        Err(broadcast::error::RecvError::Closed) => None,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            Some((Err(anyhow::anyhow!("lagged by {n}")), rx))
                        }
                    }
                },
            )))
        }

        async fn read_stream(
            &self,
            key: &str,
            _start_id: &str,
            _payload_type: RedisPayloadType,
        ) -> anyhow::Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<ConstValue, anyhow::Error>> + Send>,
            >,
        > {
            let entries =
                self.streams.get(key).cloned().ok_or_else(|| {
                    anyhow::anyhow!("stream key '{key}' not found in mock listener")
                })?;
            Ok(Box::pin(futures_util::stream::iter(
                entries.into_iter().map(Ok),
            )))
        }
    }

    // ----------------------------------------------------------------
    // Test runtime helpers (mirrors tests/postgres_listen_spec.rs)
    // ----------------------------------------------------------------

    #[derive(Clone)]
    struct TestHttp {
        client: ClientWithMiddleware,
    }

    impl Default for TestHttp {
        fn default() -> Self {
            Self { client: ClientBuilder::new(Client::new()).build() }
        }
    }

    impl TestHttp {
        fn init(upstream: &Upstream) -> Arc<Self> {
            let client = Client::builder()
                .tcp_keepalive(Some(Duration::from_secs(upstream.tcp_keep_alive)))
                .timeout(Duration::from_secs(upstream.timeout))
                .connect_timeout(Duration::from_secs(upstream.connect_timeout))
                .http2_keep_alive_interval(Some(Duration::from_secs(upstream.keep_alive_interval)))
                .http2_keep_alive_timeout(Duration::from_secs(upstream.keep_alive_timeout))
                .http2_keep_alive_while_idle(upstream.keep_alive_while_idle)
                .pool_idle_timeout(Some(Duration::from_secs(upstream.pool_idle_timeout)))
                .pool_max_idle_per_host(upstream.pool_max_idle_per_host)
                .user_agent(upstream.user_agent.clone())
                .danger_accept_invalid_certs(!upstream.verify_ssl);

            let client = if upstream.http2_only {
                client.http2_prior_knowledge()
            } else {
                client
            };

            let client = match upstream.proxy.as_ref() {
                Some(proxy) => client
                    .proxy(reqwest::Proxy::http(proxy.url.clone()).expect("Failed to set proxy")),
                None => client,
            };

            let client = ClientBuilder::new(client.build().expect("Failed to build client"));
            Arc::new(Self { client: client.build() })
        }
    }

    #[async_trait::async_trait]
    impl HttpIO for TestHttp {
        async fn execute(
            &self,
            request: reqwest::Request,
        ) -> anyhow::Result<Response<bytes::Bytes>> {
            let response = self.client.execute(request).await?;
            Response::from_reqwest_with_error_handling(response).await
        }
    }

    #[derive(Clone)]
    struct TestFileIO {}

    impl TestFileIO {
        fn init() -> Self {
            TestFileIO {}
        }
    }

    #[async_trait::async_trait]
    impl FileIO for TestFileIO {
        async fn write<'a>(&'a self, path: &'a str, content: &'a [u8]) -> anyhow::Result<()> {
            let mut file = tokio::fs::File::create(path).await?;
            file.write_all(content)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(())
        }

        async fn read<'a>(&'a self, path: &'a str) -> anyhow::Result<String> {
            let mut file = tokio::fs::File::open(path).await?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(String::from_utf8(buffer)?)
        }
    }

    #[derive(Clone)]
    struct TestEnvIO {
        vars: HashMap<String, String>,
    }

    impl EnvIO for TestEnvIO {
        fn get(&self, key: &str) -> Option<std::borrow::Cow<'_, str>> {
            self.vars.get(key).map(std::borrow::Cow::from)
        }
    }

    impl TestEnvIO {
        pub fn init() -> Self {
            Self { vars: std::env::vars().collect() }
        }
    }

    #[must_use]
    fn init_runtime(
        script: Option<&Script>,
        redis_listeners: HashMap<String, Arc<dyn RedisListenerIO>>,
    ) -> TargetRuntime {
        let http = TestHttp::init(&Upstream::default());
        let http2 = TestHttp::init(&Upstream::default().http2_only(true));

        let file = TestFileIO::init();
        let env = TestEnvIO::init();

        TargetRuntime {
            http,
            http2_only: http2,
            env: Arc::new(env),
            file: Arc::new(file),
            cache: Arc::new(InMemoryCache::default()),
            extensions: Arc::new(vec![]),
            cmd_worker: match script {
                Some(script) => Some(init_worker_io::<Event, Command>(script.clone())),
                None => None,
            },
            worker: match script {
                Some(script) => Some(init_worker_io::<gqlrs::Value, gqlrs::Value>(script.clone())),
                None => None,
            },
            postgres: HashMap::new(),
            postgres_listeners: HashMap::new(),
            redis: HashMap::new(),
            redis_listeners,
            s3: HashMap::new(),
        }
    }

    // ----------------------------------------------------------------
    // Schema helpers
    // ----------------------------------------------------------------

    fn listen_schema() -> String {
        r#"schema @server(port: 0) {
  query: Query
  subscription: Subscription
}

type Query {
  dummy: String @expr(body: "ok")
}

type Subscription {
  alerts: JSON @redis(operation: SUBSCRIBE, channel: "alerts")
  streamEvents: JSON @redis(operation: XREAD, key: "events")
}
"#
        .to_string()
    }

    fn subscribe_on_query_schema() -> String {
        r#"schema @server(port: 0) {
  query: Query
}

type Query {
  alerts: JSON @redis(operation: SUBSCRIBE, channel: "alerts")
}
"#
        .to_string()
    }

    async fn build_config(
        runtime: TargetRuntime,
        schema: &str,
    ) -> gqlforge::core::config::ConfigModule {
        let reader = ConfigReader::init(runtime);

        let mut temp_file = tempfile::Builder::new()
            .suffix(".graphql")
            .tempfile()
            .unwrap();
        std::io::Write::write_all(&mut temp_file, schema.as_bytes()).unwrap();

        let config_path = temp_file.path().to_str().unwrap().to_string();
        let config = reader.read_all(&[config_path.as_str()]).await.unwrap();

        drop(temp_file);
        config
    }

    fn find_subscription_field<'a>(
        blueprint: &'a Blueprint,
        field_name: &str,
    ) -> &'a gqlforge::core::blueprint::FieldDefinition {
        let subscription_type = blueprint
            .definitions
            .iter()
            .find_map(|def| {
                if let gqlforge::core::blueprint::Definition::Object(obj) = def {
                    if obj.name == "Subscription" {
                        Some(obj)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("Subscription type not found in blueprint");

        subscription_type
            .fields
            .iter()
            .find(|f| f.name == field_name)
            .unwrap_or_else(|| panic!("{field_name} field not found"))
    }

    // ----------------------------------------------------------------
    // Blueprint conversion tests (integration grain: SDL -> Blueprint)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn test_redis_subscribe_blueprint_has_redis_stream_pubsub() {
        let runtime = init_runtime(None, HashMap::new());
        let schema = listen_schema();
        let config = build_config(runtime.clone(), &schema).await;

        let blueprint = Blueprint::try_from(&config).unwrap();
        let field = find_subscription_field(&blueprint, "alerts");

        match &field.resolver {
            Some(gqlforge::core::ir::model::IR::IO(io)) => match io.as_ref() {
                gqlforge::core::ir::model::IO::RedisStream {
                    connection_id,
                    source,
                    payload_type,
                } => {
                    assert_eq!(connection_id, "default");
                    assert_eq!(*payload_type, RedisPayloadType::Json);
                    match source {
                        RedisStreamSource::PubSub { channel } => {
                            assert_eq!(channel.to_string(), "alerts");
                        }
                        RedisStreamSource::Stream { .. } => {
                            panic!("Expected PubSub source, got: {source:?}")
                        }
                    }
                }
                other => panic!("Expected IO::RedisStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO resolver, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_redis_xread_blueprint_has_redis_stream_with_stream_source() {
        let runtime = init_runtime(None, HashMap::new());
        let schema = listen_schema();
        let config = build_config(runtime.clone(), &schema).await;

        let blueprint = Blueprint::try_from(&config).unwrap();
        let field = find_subscription_field(&blueprint, "streamEvents");

        match &field.resolver {
            Some(gqlforge::core::ir::model::IR::IO(io)) => match io.as_ref() {
                gqlforge::core::ir::model::IO::RedisStream { source, .. } => match source {
                    RedisStreamSource::Stream { key, start_id } => {
                        assert_eq!(key.to_string(), "events");
                        // Default start_id when unset is "$" (only new
                        // entries).
                        assert_eq!(start_id.to_string(), "$");
                    }
                    RedisStreamSource::PubSub { .. } => {
                        panic!("Expected Stream source, got: {source:?}")
                    }
                },
                other => panic!("Expected IO::RedisStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO resolver, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_redis_subscribe_on_query_field_fails() {
        let runtime = init_runtime(None, HashMap::new());
        let schema = subscribe_on_query_schema();
        let config = build_config(runtime.clone(), &schema).await;

        let result = Blueprint::try_from(&config);
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("SUBSCRIBE) is only allowed on Subscription"),
            "unexpected error: {err}"
        );
    }

    // ----------------------------------------------------------------
    // MockRedisListener unit tests
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn test_redis_mock_listener_pubsub_yields_events() {
        let (tx, _) = broadcast::channel::<ConstValue>(16);
        let listener = MockRedisListener::new()
            .with_channel("alerts", tx.clone())
            .into_arc();

        let mut stream = listener
            .subscribe("alerts", RedisPayloadType::Json)
            .await
            .unwrap();

        let event = ConstValue::Object(
            [(
                gqlrs::Name::new("message"),
                ConstValue::String("fire".to_string()),
            )]
            .into(),
        );
        tx.send(event.clone()).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(result, event);
    }

    #[tokio::test]
    async fn test_redis_mock_listener_read_stream_yields_id_values_entries() {
        let entry_one = ConstValue::Object(
            [
                (
                    gqlrs::Name::new("id"),
                    ConstValue::String("1-0".to_string()),
                ),
                (
                    gqlrs::Name::new("values"),
                    ConstValue::Object(
                        [(
                            gqlrs::Name::new("name"),
                            ConstValue::String("Alice".to_string()),
                        )]
                        .into(),
                    ),
                ),
            ]
            .into(),
        );
        let entry_two = ConstValue::Object(
            [
                (
                    gqlrs::Name::new("id"),
                    ConstValue::String("2-0".to_string()),
                ),
                (
                    gqlrs::Name::new("values"),
                    ConstValue::Object(
                        [(
                            gqlrs::Name::new("name"),
                            ConstValue::String("Bob".to_string()),
                        )]
                        .into(),
                    ),
                ),
            ]
            .into(),
        );

        let listener = MockRedisListener::new()
            .with_stream("events", vec![entry_one.clone(), entry_two.clone()])
            .into_arc();

        let stream = listener
            .read_stream("events", "$", RedisPayloadType::Json)
            .await
            .unwrap();

        let results: Vec<ConstValue> = stream.map(|r| r.unwrap()).collect::<Vec<_>>().await;

        assert_eq!(results, vec![entry_one, entry_two]);
    }

    // ----------------------------------------------------------------
    // Full-stack GraphQL subscription test: MockRedisListener -> execute_stream
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn test_redis_subscription_execute_stream_pubsub_full_stack() {
        let schema_sdl = r#"schema
  @server(port: 0)
  @link(type: Redis, src: "redis://localhost:6379") {
  query: Query
  subscription: Subscription
}

type Query {
  dummy: String @expr(body: "ok")
}

type Subscription {
  alerts: JSON @redis(operation: SUBSCRIBE, channel: "alerts")
}
"#;

        let config = gqlforge::core::config::Config::from_sdl(schema_sdl)
            .to_result()
            .unwrap();
        let config_module = gqlforge::core::config::ConfigModule::from(config);
        let blueprint = Blueprint::try_from(&config_module).unwrap();

        let (tx, _) = broadcast::channel::<ConstValue>(16);
        let listener = MockRedisListener::new()
            .with_channel("alerts", tx.clone())
            .into_arc();

        let mut redis_listeners: HashMap<String, Arc<dyn RedisListenerIO>> = HashMap::new();
        redis_listeners.insert("default".to_string(), listener);

        let runtime = init_runtime(None, redis_listeners);

        let app_ctx = Arc::new(AppContext::new(blueprint, runtime, EndpointSet::default()));

        let req_ctx = Arc::new(RequestContext::from(app_ctx.as_ref()));
        let request = gqlrs::Request::new("subscription { alerts }").data(req_ctx);

        let mut stream = app_ctx.schema.execute_stream(request);

        // Publish the event once the resolver has had a chance to subscribe.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let event = ConstValue::Object(
                [(
                    gqlrs::Name::new("level"),
                    ConstValue::String("critical".to_string()),
                )]
                .into(),
            );
            let _ = tx.send(event);
        });

        let response = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for subscription event")
            .expect("stream ended without yielding a response");

        assert!(
            response.errors.is_empty(),
            "unexpected GraphQL errors: {:?}",
            response.errors
        );

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "alerts": { "level": "critical" } })
        );
    }

    #[tokio::test]
    async fn test_redis_subscription_execute_stream_xread_full_stack() {
        let schema_sdl = r#"schema
  @server(port: 0)
  @link(type: Redis, src: "redis://localhost:6379") {
  query: Query
  subscription: Subscription
}

type Query {
  dummy: String @expr(body: "ok")
}

type Subscription {
  streamEvents: JSON @redis(operation: XREAD, key: "events", startId: "0")
}
"#;

        let config = gqlforge::core::config::Config::from_sdl(schema_sdl)
            .to_result()
            .unwrap();
        let config_module = gqlforge::core::config::ConfigModule::from(config);
        let blueprint = Blueprint::try_from(&config_module).unwrap();

        // Sanity-check the blueprint wiring for `key`/`start_id`/
        // `connection_id` before exercising it end-to-end, mirroring
        // `test_redis_xread_blueprint_has_redis_stream_with_stream_source`.
        let field = find_subscription_field(&blueprint, "streamEvents");
        match &field.resolver {
            Some(gqlforge::core::ir::model::IR::IO(io)) => match io.as_ref() {
                gqlforge::core::ir::model::IO::RedisStream { connection_id, source, .. } => {
                    assert_eq!(connection_id, "default");
                    match source {
                        RedisStreamSource::Stream { key, start_id } => {
                            assert_eq!(key.to_string(), "events");
                            assert_eq!(start_id.to_string(), "0");
                        }
                        RedisStreamSource::PubSub { .. } => {
                            panic!("Expected Stream source, got: {source:?}")
                        }
                    }
                }
                other => panic!("Expected IO::RedisStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO resolver, got: {other:?}"),
        }

        let entry = ConstValue::Object(
            [
                (
                    gqlrs::Name::new("id"),
                    ConstValue::String("1-0".to_string()),
                ),
                (
                    gqlrs::Name::new("values"),
                    ConstValue::Object(
                        [(
                            gqlrs::Name::new("message"),
                            ConstValue::String("fire".to_string()),
                        )]
                        .into(),
                    ),
                ),
            ]
            .into(),
        );

        let listener = MockRedisListener::new()
            .with_stream("events", vec![entry.clone()])
            .into_arc();

        let mut redis_listeners: HashMap<String, Arc<dyn RedisListenerIO>> = HashMap::new();
        redis_listeners.insert("default".to_string(), listener);

        let runtime = init_runtime(None, redis_listeners);

        let app_ctx = Arc::new(AppContext::new(blueprint, runtime, EndpointSet::default()));

        let req_ctx = Arc::new(RequestContext::from(app_ctx.as_ref()));
        let request = gqlrs::Request::new("subscription { streamEvents }").data(req_ctx);

        let mut stream = app_ctx.schema.execute_stream(request);

        let response = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for subscription event")
            .expect("stream ended without yielding a response");

        assert!(
            response.errors.is_empty(),
            "unexpected GraphQL errors: {:?}",
            response.errors
        );

        let json = serde_json::to_value(&response.data).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "streamEvents": {
                    "id": "1-0",
                    "values": { "message": "fire" }
                }
            })
        );
    }
}
