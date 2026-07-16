use std::sync::Arc;

use gqlforge::core::app_context::AppContext;
use gqlforge::core::blueprint::{Blueprint, PostgresConnectionSpec};
use gqlforge::core::config::reader::ConfigReader;
use gqlforge::core::http::RequestContext;
use gqlforge::core::jit::{ConstValueExecutor, Request as JitRequest};
use gqlforge::core::rest::EndpointSet;
use gqlrs_value::ConstValue;

const CONFIG: &str = r#"
schema @link(id: "metrics", type: GreptimeDB, src: "__GREPTIME_URL__") {
  query: Query
  mutation: Mutation
}

type Query {
  metrics: [Metric!]! @greptimedb(db: "metrics", table: "smoke_metrics")
}

type Mutation {
  insertMetric(input: MetricInput!): Int!
    @greptimedb(
      db: "metrics"
      table: "smoke_metrics"
      operation: INSERT
      input: "{{.args.input}}"
    )
  deleteMetric(host: String!): Int!
    @greptimedb(
      db: "metrics"
      table: "smoke_metrics"
      operation: DELETE
      filter: {host: "{{.args.host}}"}
    )
}

type Metric {
  host: String!
  ts: DateTime!
  metricValue: Float!
  note: String
  eventDay: DateTime!
  recordCount: String!
}

input MetricInput {
  host: String!
  ts: String!
  metricValue: Float!
  note: String
  eventDay: String!
  recordCount: String!
}
"#;

async fn execute_jit(app_ctx: &Arc<AppContext>, query: &str) -> anyhow::Result<serde_json::Value> {
    let request = JitRequest::<ConstValue>::from(gqlrs::Request::new(query));
    let request_context = RequestContext::from(app_ctx.as_ref());
    let executor = ConstValueExecutor::try_new(&request, app_ctx)?;
    let response = executor.execute(app_ctx, &request_context, request).await;
    Ok(serde_json::from_slice(&response.body)?)
}

#[tokio::test]
#[ignore = "requires a running GreptimeDB instance and GREPTIME_URL"]
async fn greptimedb_postgres_protocol_smoke() -> anyhow::Result<()> {
    let url = std::env::var("GREPTIME_URL")?;
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            "DROP TABLE IF EXISTS smoke_metrics;
             CREATE TABLE smoke_metrics (
               host STRING,
               ts TIMESTAMP TIME INDEX,
               metric_value FLOAT64,
               note STRING,
               event_day DATE,
               record_count INT64,
               PRIMARY KEY(host)
             )",
        )
        .await?;

    let schema = CONFIG.replace("__GREPTIME_URL__", &url);
    let mut config_file = tempfile::Builder::new().suffix(".graphql").tempfile()?;
    std::io::Write::write_all(&mut config_file, schema.as_bytes())?;
    let config_path = config_file
        .path()
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("temporary path is not UTF-8"))?;

    let reader_runtime = gqlforge::cli::runtime::init(&Blueprint::default())?;
    let config = ConfigReader::init(reader_runtime)
        .read_all(&[config_path])
        .await?;
    let blueprint = Blueprint::try_from(&config)?;
    assert!(matches!(
        blueprint.postgres_connections.as_slice(),
        [(id, PostgresConnectionSpec::GreptimeDbUrl(_))] if id == "metrics"
    ));

    let runtime = gqlforge::cli::runtime::init(&blueprint)?;
    let app_ctx = Arc::new(AppContext::new(blueprint, runtime, EndpointSet::default()));

    let inserted = execute_jit(
        &app_ctx,
        r#"mutation {
          insertMetric(input: {
            host: "api-1"
            ts: "2026-01-01 00:00:00"
            metricValue: 1.5
            note: null
            eventDay: "2026-01-01"
            recordCount: "9007199254740993"
          })
        }"#,
    )
    .await?;
    assert_eq!(inserted, serde_json::json!({"data": {"insertMetric": 1}}));

    let metrics = execute_jit(
        &app_ctx,
        "{ metrics { host ts metricValue note eventDay recordCount } }",
    )
    .await?;
    let metric = &metrics["data"]["metrics"][0];
    assert_eq!(metric["host"], "api-1");
    assert_eq!(metric["metricValue"], 1.5);
    assert!(metric["note"].is_null());
    let timestamp = metric["ts"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("timestamp is not a string"))?;
    assert!(chrono::DateTime::parse_from_rfc3339(timestamp).is_ok());
    let day = metric["eventDay"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("date is not a string"))?;
    assert!(chrono::DateTime::parse_from_rfc3339(day).is_ok());
    assert_eq!(metric["recordCount"], "9007199254740993");

    let deleted = execute_jit(&app_ctx, r#"mutation { deleteMetric(host: "api-1") }"#).await?;
    assert_eq!(deleted, serde_json::json!({"data": {"deleteMetric": 1}}));
    assert_eq!(
        execute_jit(&app_ctx, "{ metrics { host } }").await?,
        serde_json::json!({"data": {"metrics": []}})
    );

    Ok(())
}
