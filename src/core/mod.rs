pub mod app_context;
mod auth;
pub mod blueprint;
pub mod cache;
pub mod config;
mod counter;
pub mod data_loader;
pub mod directive;
pub mod document;
pub mod endpoint;
mod errata;
pub mod error;
pub mod generator;
pub mod gqlrs_hyper;
pub mod graphql;
pub mod greptimedb;
pub mod grpc;
pub mod has_headers;
pub mod helpers;
pub mod http;
pub mod ir;
pub mod jit;
pub mod json;
pub mod merge_right;
pub mod mustache;
pub mod path;
pub mod postgres;
pub mod primitive;
pub mod print_schema;
pub mod proto_reader;
pub mod redis;
pub mod resource_reader;
pub mod rest;
pub mod runtime;
pub mod s3;
pub mod scalar;
pub mod schema_extension;
mod serde_value_ext;
pub mod tracing;
mod transform;
pub mod try_fold;
pub mod variance;
pub mod worker;
pub mod worker_hooks;
pub mod wrapping_type;

// Re-export everything from `gqlforge_macros` as `macros`
use std::borrow::Cow;
use std::hash::Hash;
use std::num::NonZeroU64;

pub use errata::Errata;
pub use error::{Error, Result};
pub use gqlforge_macros as macros;
use gqlrs::{Pos, Positioned};
use gqlrs_value::ConstValue;
use http::Response;
use ir::model::IoId;
pub use mustache::Mustache;
pub use transform::Transform;
pub use wrapping_type::Type;

const DEFAULT_VERIFY_SSL: bool = true;
#[must_use]
pub const fn default_verify_ssl() -> Option<bool> {
    Some(DEFAULT_VERIFY_SSL)
}

pub trait EnvIO: Send + Sync + 'static {
    fn get(&self, key: &str) -> Option<Cow<'_, str>>;
}

#[async_trait::async_trait]
pub trait HttpIO: Sync + Send + 'static {
    async fn execute(&self, request: reqwest::Request) -> anyhow::Result<Response<bytes::Bytes>>;

    /// Execute a request and return the raw streaming response.
    /// Used for streaming subscriptions (gRPC server-streaming, GraphQL SSE).
    async fn execute_raw(&self, request: reqwest::Request) -> anyhow::Result<reqwest::Response> {
        // Default implementation: not supported
        let _ = request;
        Err(anyhow::anyhow!(
            "Streaming not supported by this HttpIO implementation"
        ))
    }

    /// Send an HTTP/2 request for gRPC using a Vec<u8> body.
    async fn execute_h2(
        &self,
        url: &str,
        headers: ::http::HeaderMap,
        body: Vec<u8>,
    ) -> anyhow::Result<Response<bytes::Bytes>> {
        let mut req = reqwest::Request::new(reqwest::Method::POST, url.parse()?);
        *req.version_mut() = reqwest::Version::HTTP_2;
        *req.headers_mut() = headers;
        req.body_mut().replace(body.into());
        self.execute(req).await
    }
}

#[async_trait::async_trait]
pub trait FileIO: Send + Sync {
    async fn write<'a>(&'a self, path: &'a str, content: &'a [u8]) -> anyhow::Result<()>;
    async fn read<'a>(&'a self, path: &'a str) -> anyhow::Result<String>;
}

#[async_trait::async_trait]
pub trait Cache: Send + Sync {
    type Key: Hash + Eq;
    type Value;
    async fn set<'a>(
        &'a self,
        key: Self::Key,
        value: Self::Value,
        ttl: NonZeroU64,
    ) -> Result<(), cache::Error>;
    async fn get<'a>(&'a self, key: &'a Self::Key) -> Result<Option<Self::Value>, cache::Error>;

    fn hit_rate(&self) -> Option<f64>;
}

pub type EntityCache = dyn Cache<Key = IoId, Value = ConstValue>;

#[async_trait::async_trait]
pub trait WorkerIO<In, Out>: Send + Sync + 'static {
    /// Calls a global JS function
    async fn call(&self, name: &str, input: In) -> Result<Option<Out>, worker::Error>;
}

pub fn is_default<T: Default + Eq>(val: &T) -> bool {
    *val == T::default()
}

pub fn pos<A>(a: A) -> Positioned<A> {
    Positioned::new(a, Pos::default())
}

#[must_use]
///
/// # Panics
///
/// Panics if an internal assertion fails.
pub fn verify_ssl_is_default(val: &Option<bool>) -> bool {
    val.as_ref().is_none_or(|&v| v)
}

#[cfg(test)]
pub mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::collections::HashMap;

    use super::*;

    #[macro_export]
    macro_rules! include_config {
        ($file:literal) => {{
            use $crate::core::config::{Config, Source};
            let config_str = include_str!($file);
            let result = || {
                let source = Source::detect($file)?;
                Config::from_source(source, config_str)
            };
            result()
        }};
    }

    #[derive(Clone, Default)]
    pub struct TestEnvIO(HashMap<String, String>);

    impl TestEnvIO {
        #[must_use]
        pub fn init(env_vars: HashMap<String, String>) -> Self {
            Self(env_vars)
        }
    }

    impl EnvIO for TestEnvIO {
        fn get(&self, key: &str) -> Option<Cow<'_, str>> {
            self.0.get(key).map(Cow::from)
        }
    }

    impl FromIterator<(String, String)> for TestEnvIO {
        fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
            Self(HashMap::from_iter(iter))
        }
    }
    #[test]
    fn test_include_config() {
        let cfg = include_config!("fixtures/helloworld.graphql")
            .unwrap()
            .to_sdl();
        insta::assert_snapshot!(cfg);
    }
}
