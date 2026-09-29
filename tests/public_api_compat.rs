#![expect(clippy::unwrap_used, reason = "public API compatibility tests")]

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use gqlforge::core::blueprint::{Blueprint, Index, QueryField};
use gqlforge::core::cache::InMemoryCache;
use gqlforge::core::config::{Config, ConfigModule, Extensions, Server};
use gqlforge::core::helpers::{body, gql_type, url};
use gqlforge::core::ir::model::IoId;
use gqlforge::core::jit::Variable;
use gqlforge::core::runtime::TargetRuntime;
use gqlforge::core::schema_extension::SchemaExtension;
use gqlforge::core::{EntityCache, EnvIO, FileIO, HttpIO};
use gqlforge_valid::Validator;
use gqlrs_value::ConstValue;

const SDL: &str = r#"
schema { query: Query }
interface Node { lookup(id: ID!): String }
type User implements Node { lookup(id: ID!): String }
input Filter { id: ID! }
type Query { lookup(id: ID!, filter: Filter!): User @http(url: "http://localhost:3000") }
"#;

struct UnusedHttp;

#[async_trait::async_trait]
impl HttpIO for UnusedHttp {
    async fn execute(
        &self,
        _request: reqwest::Request,
    ) -> anyhow::Result<gqlforge::core::http::Response<bytes::Bytes>> {
        Err(anyhow::anyhow!("unused test HTTP client"))
    }
}

struct UnusedEnv;

impl EnvIO for UnusedEnv {
    fn get(&self, _key: &str) -> Option<Cow<'_, str>> {
        None
    }
}

struct UnusedFile;

#[async_trait::async_trait]
impl FileIO for UnusedFile {
    async fn write<'a>(&'a self, _path: &'a str, _content: &'a [u8]) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("unused test file IO"))
    }

    async fn read<'a>(&'a self, _path: &'a str) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("unused test file IO"))
    }
}

struct NoopExtension;

impl gqlrs::extensions::Extension for NoopExtension {}

struct NoopExtensionFactory;

impl gqlrs::extensions::ExtensionFactory for NoopExtensionFactory {
    fn create(&self) -> Arc<dyn gqlrs::extensions::Extension> {
        Arc::new(NoopExtension)
    }
}

fn unused_runtime() -> TargetRuntime {
    TargetRuntime {
        http: Arc::new(UnusedHttp),
        http2_only: Arc::new(UnusedHttp),
        env: Arc::new(UnusedEnv),
        file: Arc::new(UnusedFile),
        cache: Arc::new(InMemoryCache::<IoId, ConstValue>::default()),
        extensions: Arc::new(Vec::new()),
        cmd_worker: None,
        worker: None,
        postgres: HashMap::new(),
        postgres_listeners: HashMap::new(),
        redis: HashMap::new(),
        redis_listeners: HashMap::new(),
        s3: HashMap::new(),
    }
}

#[test]
fn query_field_keeps_tuple_payload_and_argument_lookup() {
    let config = Config::from_sdl(SDL).to_result().unwrap();
    let config_module = ConfigModule::from(config);
    let blueprint = Blueprint::try_from(&config_module).unwrap();
    let index = Index::from(&blueprint);

    for type_name in ["Query", "Node", "User"] {
        let field = index.get_field(type_name, "lookup").unwrap();
        assert_eq!(field.get_arg("id").unwrap().name, "id");
        assert!(field.get_arg("missing").is_none());
    }

    let object_field = index.get_field("Query", "lookup").unwrap();
    assert!(matches!(object_field, QueryField::Field(_)));
    let QueryField::Field(payload) = object_field else {
        return;
    };
    assert_eq!(payload.0.name, "lookup");
    assert!(payload.1.contains_key("id"));

    let constructed = QueryField::Field(Box::new((payload.0.clone(), payload.1.clone())));
    assert!(constructed.get_arg("id").is_some());
    assert!(constructed.get_arg("missing").is_none());

    let input_field = index.get_field("Filter", "id").unwrap();
    assert!(matches!(input_field, QueryField::InputField(_)));
    assert!(input_field.get_arg("id").is_none());
}

#[test]
fn restored_helper_modules_keep_their_public_outputs() {
    let value = serde_json::json!({"name": "Ada"});
    let request_body = body::to_body(Some(&value)).to_result().unwrap().unwrap();
    assert_eq!(request_body.value, value.to_string());
    assert!(request_body.mustache.is_some());
    assert!(body::to_body(None).to_result().unwrap().is_none());

    let parsed_url = url::to_url("http://localhost:3000").to_result().unwrap();
    assert_eq!(
        parsed_url,
        gqlforge::core::Mustache::parse("http://localhost:3000")
    );

    assert_eq!(gql_type::detect_gql_data_type("3.14"), "Float");
    assert!(gql_type::is_valid_field_name("field_name1"));
    assert!(!gql_type::is_valid_field_name("not valid"));
    assert_eq!(gql_type::to_gql_type(&value["name"]), "String");
    assert!(gql_type::is_primitive(&serde_json::json!(true)));
    assert!(!gql_type::is_primitive(&serde_json::json!([])));
}

#[test]
fn restored_configuration_apis_keep_their_semantics() {
    let default_server = Server::default();
    assert_eq!(default_server.get_limit_complexity(), 1000);
    assert_eq!(default_server.get_limit_depth(), 15);
    assert_eq!(default_server.get_limit_directives(), 50);

    let server = Server {
        limit_complexity: Some(12),
        limit_depth: Some(4),
        limit_directives: Some(7),
        ..Server::default()
    };
    assert_eq!(server.get_limit_complexity(), 12);
    assert_eq!(server.get_limit_depth(), 4);
    assert_eq!(server.get_limit_directives(), 7);

    let left = Extensions { script: Some("left".to_owned()), ..Extensions::default() };
    let right = Extensions { script: Some("right".to_owned()), ..Extensions::default() };
    let config_module = ConfigModule::new(Config::default(), left).merge_extensions(right);
    assert_eq!(config_module.extensions().script.as_deref(), Some("right"));

    assert_eq!(Variable::new("name".to_owned()).into_string(), "name");
}

#[test]
fn restored_entity_cache_alias_and_runtime_extension_api_are_public() {
    let cache: InMemoryCache<IoId, ConstValue> = InMemoryCache::default();
    let _entity_cache: &EntityCache = &cache;

    let mut runtime = unused_runtime();
    let previous_extensions = Arc::clone(&runtime.extensions);
    runtime.add_extensions(vec![SchemaExtension::new(NoopExtensionFactory)]);
    assert_eq!(runtime.extensions.len(), 1);
    assert!(!Arc::ptr_eq(&previous_extensions, &runtime.extensions));
}
