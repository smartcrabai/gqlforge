/// Test runtime with streaming support (`execute_raw`).
///
/// This is a copy of the `test` module from `server_spec.rs` with an added
/// `execute_raw` implementation so that HTTP SSE subscription streaming works
/// end-to-end.
#[cfg(test)]
pub mod test {
    #![expect(clippy::expect_used, reason = "test code")]
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Result, anyhow};
    use bytes::Bytes;
    use gqlforge::cli::javascript::init_worker_io;
    use gqlforge::core::blueprint::{Script, Upstream};
    use gqlforge::core::cache::InMemoryCache;
    use gqlforge::core::http::Response;
    use gqlforge::core::runtime::TargetRuntime;
    use gqlforge::core::worker::{Command, Event};
    use gqlforge::core::{EnvIO, FileIO, HttpIO};
    use gqlrs::Value;
    use reqwest::Client;
    use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
            let mut builder = Client::builder()
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

            if upstream.http2_only {
                builder = builder.http2_prior_knowledge();
            }

            if let Some(ref proxy) = upstream.proxy {
                builder = builder.proxy(
                    reqwest::Proxy::http(proxy.url.clone())
                        .expect("Failed to set proxy in http client"),
                );
            }

            let client =
                ClientBuilder::new(builder.build().expect("Failed to build client")).build();

            Arc::new(Self { client })
        }
    }

    #[async_trait::async_trait]
    impl HttpIO for TestHttp {
        async fn execute(&self, request: reqwest::Request) -> Result<Response<Bytes>> {
            let response = self.client.execute(request).await;
            Response::from_reqwest(
                response?
                    .error_for_status()
                    .map_err(reqwest::Error::without_url)?,
            )
            .await
        }

        async fn execute_raw(&self, request: reqwest::Request) -> Result<reqwest::Response> {
            Ok(self.client.execute(request).await?)
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
        fn get(&self, key: &str) -> Option<Cow<'_, str>> {
            self.vars.get(key).map(Cow::from)
        }
    }

    impl TestEnvIO {
        pub fn init() -> Self {
            Self { vars: std::env::vars().collect() }
        }
    }

    #[must_use]
    pub fn init(script: &Option<Script>) -> TargetRuntime {
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
            cmd_worker: match &script {
                Some(script) => Some(init_worker_io::<Event, Command>(script.to_owned())),
                None => None,
            },
            worker: match &script {
                Some(script) => Some(init_worker_io::<Value, Value>(script.to_owned())),
                None => None,
            },
            postgres: HashMap::new(),
            postgres_listeners: HashMap::new(),
            redis: HashMap::new(),
            redis_listeners: HashMap::new(),
            s3: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod subscription_spec {
    #![expect(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
    use std::time::Duration;

    use gqlforge::cli::server::Server;
    use gqlforge::core::config::reader::ConfigReader;
    use reqwest::Client;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    /// Start a mock SSE upstream server that sends the given JSON events.
    ///
    /// Returns `(port, join_handle)`. The server accepts exactly one
    /// connection, sends all events as `data: {json}\n\n`, then closes the
    /// connection.
    async fn start_mock_sse_server(events: Vec<serde_json::Value>) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);

            // Consume HTTP request headers (read until blank line).
            let mut line = String::new();
            loop {
                line.clear();
                buf_reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }

            // Write HTTP/1.1 SSE response.
            writer
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      Content-Type: text/event-stream\r\n\
                      Cache-Control: no-cache\r\n\
                      Connection: close\r\n\
                      \r\n",
                )
                .await
                .unwrap();
            writer.flush().await.unwrap();

            for event in &events {
                let json_str = serde_json::to_string(event).unwrap();
                let data = format!("data: {json_str}\n\n");
                writer.write_all(data.as_bytes()).await.unwrap();
                writer.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Connection closes when writer is dropped.
        });

        (port, handle)
    }

    /// Generate a gqlforge GraphQL schema string that wires a subscription
    /// field to the given mock upstream SSE endpoint.
    fn generate_schema(server_port: u16, upstream_port: u16) -> String {
        format!(
            r#"schema @server(port: {server_port}) {{
  query: Query
  subscription: Subscription
}}

type Query {{
  dummy: String @expr(body: "ok")
}}

type Subscription {{
  sensorData: SensorData @http(url: "http://127.0.0.1:{upstream_port}/sse/sensors")
}}

type SensorData {{
  temperature: Float!
  humidity: Float!
}}"#
        )
    }

    /// Write the schema to a temp `.graphql` file, build config, and start the
    /// gqlforge server. Blocks until the server is ready to accept connections.
    async fn start_gqlforge_server(schema: &str) {
        let runtime = crate::test::init(&None);
        let reader = ConfigReader::init(runtime);

        let mut temp_file = tempfile::Builder::new()
            .suffix(".graphql")
            .tempfile()
            .unwrap();
        std::io::Write::write_all(&mut temp_file, schema.as_bytes()).unwrap();

        let config_path = temp_file.path().to_str().unwrap().to_string();
        let config = reader.read_all(&[config_path.as_str()]).await.unwrap();
        let mut server = Server::new(config);
        let server_up_receiver = server.server_up_receiver();

        tokio::spawn(async move {
            let _temp = temp_file; // prevent deletion until server exits
            Box::pin(server.start()).await.unwrap();
        });

        server_up_receiver
            .await
            .expect("Server did not start up correctly");
    }

    /// Parse SSE `data:` lines from the raw response body text.
    fn parse_sse_events(body: &str) -> Vec<serde_json::Value> {
        body.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect()
    }

    // ----------------------------------------------------------------
    // Tests
    // ----------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_http_subscription_sse_happy_path() {
        let events = vec![
            json!({"temperature": 25.0, "humidity": 60.0}),
            json!({"temperature": 26.5, "humidity": 55.0}),
            json!({"temperature": 28.0, "humidity": 50.0}),
        ];

        let (upstream_port, _upstream_handle) = start_mock_sse_server(events).await;
        let schema = generate_schema(8810, upstream_port);
        start_gqlforge_server(&schema).await;

        let client = Client::new();
        let query = json!({
            "query": "subscription { sensorData { temperature humidity } }"
        });

        let response = client
            .post("http://127.0.0.1:8810/graphql")
            .json(&query)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        let body = tokio::time::timeout(Duration::from_secs(10), response.text())
            .await
            .expect("Timeout reading SSE response")
            .unwrap();

        let sse_events = parse_sse_events(&body);

        assert_eq!(sse_events.len(), 3);
        assert_eq!(
            sse_events[0],
            json!({"data": {"sensorData": {"temperature": 25.0, "humidity": 60.0}}})
        );
        assert_eq!(
            sse_events[1],
            json!({"data": {"sensorData": {"temperature": 26.5, "humidity": 55.0}}})
        );
        assert_eq!(
            sse_events[2],
            json!({"data": {"sensorData": {"temperature": 28.0, "humidity": 50.0}}})
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_http_subscription_sse_response_headers() {
        let events = vec![json!({"temperature": 20.0, "humidity": 40.0})];

        let (upstream_port, _upstream_handle) = start_mock_sse_server(events).await;
        let schema = generate_schema(8811, upstream_port);
        start_gqlforge_server(&schema).await;

        let client = Client::new();
        let query = json!({
            "query": "subscription { sensorData { temperature humidity } }"
        });

        let response = client
            .post("http://127.0.0.1:8811/graphql")
            .json(&query)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-cache");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_http_subscription_sse_empty_stream() {
        let events = vec![];

        let (upstream_port, _upstream_handle) = start_mock_sse_server(events).await;
        let schema = generate_schema(8812, upstream_port);
        start_gqlforge_server(&schema).await;

        let client = Client::new();
        let query = json!({
            "query": "subscription { sensorData { temperature humidity } }"
        });

        let response = client
            .post("http://127.0.0.1:8812/graphql")
            .json(&query)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        let body = tokio::time::timeout(Duration::from_secs(10), response.text())
            .await
            .expect("Timeout reading SSE response")
            .unwrap();

        let sse_events = parse_sse_events(&body);
        assert!(sse_events.is_empty());
    }
}
