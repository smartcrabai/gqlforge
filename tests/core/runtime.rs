extern crate core;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_graphql::Value;
use derive_setters::Setters;
use gqlforge::cli::javascript::init_worker_io;
use gqlforge::core::blueprint::Script;
use gqlforge::core::cache::InMemoryCache;
use gqlforge::core::config::RuntimeConfig;
use gqlforge::core::postgres::PostgresIO;
use gqlforge::core::redis::{RedisIO, RedisListenerIO};
use gqlforge::core::runtime::TargetRuntime;
use gqlforge::core::s3::S3IO;
use gqlforge::core::worker::{Command, Event};

use super::env::Env;
use super::file::TestFileIO;
use super::http::Http;
use super::model::{APIRequest, Annotation, Mock};

#[derive(Clone, Setters)]
pub struct ExecutionSpec {
    pub path: PathBuf,
    pub name: String,
    pub safe_name: String,

    pub config: RuntimeConfig,
    pub mock: Option<Vec<Mock>>,
    pub env: Option<HashMap<String, String>>,
    pub test: Option<Vec<APIRequest>>,
    pub files: BTreeMap<String, String>,

    // Annotations for the runner
    pub runner: Option<Annotation>,

    pub check_identity: bool,
    pub sdl_error: bool,
}

#[derive(Clone, Debug)]
pub struct ExecutionMock {
    pub mock: Mock,
    pub actual_hits: Arc<AtomicUsize>,
}

impl ExecutionMock {
    pub fn test_hits(&self, path: impl AsRef<Path>) {
        let url = &self.mock.request.0.url;
        if !self.mock.assert_hits {
            return;
        }
        let expected_hits = self.mock.expected_hits;
        let actual_hits = self.actual_hits.load(Ordering::Relaxed);

        assert_eq!(
            expected_hits,
            actual_hits,
            "expected mock for {} to be hit exactly {} times, but it was hit {} times for file: {}",
            url,
            expected_hits,
            actual_hits,
            path.as_ref().display()
        );
    }
}

#[derive(Default)]
pub struct RuntimeIO {
    pub postgres: Option<HashMap<String, Arc<dyn PostgresIO>>>,
    pub redis: Option<HashMap<String, Arc<dyn RedisIO>>>,
    pub redis_listeners: Option<HashMap<String, Arc<dyn RedisListenerIO>>>,
    pub s3: Option<HashMap<String, Arc<dyn S3IO>>>,
}

pub fn create_runtime(
    http_client: &Arc<Http>,
    env: Option<HashMap<String, String>>,
    script: Option<&Script>,
    io: RuntimeIO,
) -> TargetRuntime {
    let http = http_client.clone();

    let http2 = http_client.clone();

    let file = TestFileIO::init();
    let env = Env::init(env);

    TargetRuntime {
        http,
        http2_only: http2,
        env: Arc::new(env),
        file: Arc::new(file),
        cache: Arc::new(InMemoryCache::default()),
        extensions: Arc::new(vec![]),
        cmd_worker: match script {
            Some(script) => Some(init_worker_io::<Event, Command>(script.to_owned())),
            None => None,
        },
        worker: match script {
            Some(script) => Some(init_worker_io::<Value, Value>(script.to_owned())),
            None => None,
        },
        postgres: io.postgres.unwrap_or_default(),
        postgres_listeners: HashMap::new(),
        redis: io.redis.unwrap_or_default(),
        redis_listeners: io.redis_listeners.unwrap_or_default(),
        s3: io.s3.unwrap_or_default(),
    }
}
