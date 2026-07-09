#[cfg(test)]
mod postgres_listen_spec {
    #![expect(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::anyhow;
    use futures_util::{StreamExt, stream};
    use gqlforge::cli::javascript::init_worker_io;
    use gqlforge::core::blueprint::{Blueprint, Script, Upstream};
    use gqlforge::core::cache::InMemoryCache;
    use gqlforge::core::config::PostgresPayloadType;
    use gqlforge::core::config::reader::ConfigReader;
    use gqlforge::core::http::Response;
    use gqlforge::core::postgres::PostgresListenerIO;
    use gqlforge::core::runtime::TargetRuntime;
    use gqlforge::core::worker::{Command, Event};
    use gqlforge::core::{EnvIO, FileIO, HttpIO};
    use gqlrs::Value;
    use gqlrs_value::ConstValue;
    use reqwest::Client;
    use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::broadcast;

    // ----------------------------------------------------------------
    // Test runtime helpers
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
            file.write_all(content).await.map_err(|e| anyhow!("{e}"))?;
            Ok(())
        }

        async fn read<'a>(&'a self, path: &'a str) -> anyhow::Result<String> {
            let mut file = tokio::fs::File::open(path).await?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .await
                .map_err(|e| anyhow!("{e}"))?;
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
    fn init_runtime(script: Option<&Script>) -> TargetRuntime {
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
                Some(script) => Some(init_worker_io::<Value, Value>(script.clone())),
                None => None,
            },
            postgres: HashMap::new(),
            postgres_listeners: HashMap::new(),
            redis: HashMap::new(),
            redis_listeners: HashMap::new(),
            s3: HashMap::new(),
        }
    }

    // ----------------------------------------------------------------
    // Mock PostgresListenerIO using broadcast channel
    // ----------------------------------------------------------------

    struct MockPostgresListener {
        senders: HashMap<String, broadcast::Sender<ConstValue>>,
    }

    impl MockPostgresListener {
        fn new_with_sender(channel: &str, tx: broadcast::Sender<ConstValue>) -> Arc<Self> {
            let mut senders = HashMap::new();
            senders.insert(channel.to_string(), tx);
            Arc::new(Self { senders })
        }
    }

    #[async_trait::async_trait]
    impl PostgresListenerIO for MockPostgresListener {
        async fn subscribe(
            &self,
            channel: &str,
            _payload_type: PostgresPayloadType,
        ) -> anyhow::Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<ConstValue, anyhow::Error>> + Send>,
            >,
        > {
            let tx = self
                .senders
                .get(channel)
                .ok_or_else(|| anyhow!("channel '{channel}' not found in mock listener"))?
                .clone();
            let rx = tx.subscribe();
            Ok(Box::pin(stream::unfold(rx, |mut rx| async move {
                match rx.recv().await {
                    Ok(value) => Some((Ok(value), rx)),
                    Err(broadcast::error::RecvError::Closed) => None,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        Some((Err(anyhow!("lagged by {n}")), rx))
                    }
                }
            })))
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

type UserEvent {
  id: Int!
  name: String!
  action: String!
}

type Subscription {
  userChanges: UserEvent
    @postgres(table: "users", operation: LISTEN, channel: "users_changes")
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

    // ----------------------------------------------------------------
    // Tests
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn test_postgres_listen_blueprint_has_postgres_stream() {
        let runtime = init_runtime(None);
        let schema = listen_schema();
        let config = build_config(runtime.clone(), &schema).await;

        let blueprint = Blueprint::try_from(&config).unwrap();

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

        let user_changes_field = subscription_type
            .fields
            .iter()
            .find(|f| f.name == "userChanges")
            .expect("userChanges field not found");

        match &user_changes_field.resolver {
            Some(gqlforge::core::ir::model::IR::IO(io)) => {
                assert!(
                    matches!(
                        io.as_ref(),
                        gqlforge::core::ir::model::IO::PostgresStream { .. }
                    ),
                    "Expected IO::PostgresStream, got: {io:?}"
                );
            }
            other => panic!("Expected IR::IO resolver, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_postgres_listen_blueprint_includes_channel_and_connection() {
        let runtime = init_runtime(None);
        let schema = listen_schema();
        let config = build_config(runtime.clone(), &schema).await;

        let blueprint = Blueprint::try_from(&config).unwrap();

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

        let user_changes_field = subscription_type
            .fields
            .iter()
            .find(|f| f.name == "userChanges")
            .expect("userChanges field not found");

        match &user_changes_field.resolver {
            Some(gqlforge::core::ir::model::IR::IO(io)) => match io.as_ref() {
                gqlforge::core::ir::model::IO::PostgresStream {
                    connection_id,
                    channel,
                    payload_type,
                } => {
                    assert_eq!(connection_id, "default");
                    assert_eq!(channel, "users_changes");
                    assert_eq!(*payload_type, PostgresPayloadType::Json);
                }
                other => panic!("Expected IO::PostgresStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO resolver, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_postgres_listen_mock_subscriber_yields_events() {
        // Verify the mock listener itself works correctly
        let (tx, _) = broadcast::channel::<ConstValue>(16);
        let listener = MockPostgresListener::new_with_sender("users_changes", tx.clone());

        let mut stream = listener
            .subscribe("users_changes", PostgresPayloadType::Json)
            .await
            .unwrap();

        // Send an event
        let event = ConstValue::Object(
            [
                (gqlrs::Name::new("id"), ConstValue::Number(1.into())),
                (
                    gqlrs::Name::new("name"),
                    ConstValue::String("Alice".to_string()),
                ),
            ]
            .into(),
        );
        tx.send(event.clone()).unwrap();

        // Receive the event from the stream
        let result = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(result, event);
    }
}
