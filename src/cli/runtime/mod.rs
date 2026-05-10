mod env;
mod file;
mod http;

use std::collections::HashMap;
use std::fs;
use std::hash::Hash;
use std::sync::Arc;

pub use http::NativeHttp;
use inquire::{Confirm, Select};

use crate::core::blueprint::{Blueprint, PostgresConnectionSpec};
use crate::core::cache::InMemoryCache;
use crate::core::runtime::TargetRuntime;
use crate::core::worker::{Command, Event};
use crate::core::{EnvIO, FileIO, HttpIO, WorkerIO, blueprint};

// Provides access to env in native rust environment
fn init_env() -> Arc<dyn EnvIO> {
    Arc::new(env::EnvNative::init())
}

// Provides access to file system in native rust environment
fn init_file() -> Arc<dyn FileIO> {
    Arc::new(file::NativeFileIO::init())
}

fn init_http_worker_io(
    script: Option<blueprint::Script>,
) -> Option<Arc<dyn WorkerIO<Event, Command>>> {
    #[cfg(feature = "js")]
    return Some(super::javascript::init_worker_io(script?));
    #[cfg(not(feature = "js"))]
    {
        let _ = script;
        None
    }
}

fn init_resolver_worker_io(
    script: Option<blueprint::Script>,
) -> Option<Arc<dyn WorkerIO<async_graphql::Value, async_graphql::Value>>> {
    #[cfg(feature = "js")]
    return Some(super::javascript::init_worker_io(script?));
    #[cfg(not(feature = "js"))]
    {
        let _ = script;
        None
    }
}

// Provides access to http in native rust environment
fn init_http(blueprint: &Blueprint) -> Arc<dyn HttpIO> {
    Arc::new(http::NativeHttp::init(
        &blueprint.upstream,
        &blueprint.telemetry,
    ))
}

// Provides access to http in native rust environment
fn init_http2_only(blueprint: &Blueprint) -> Arc<dyn HttpIO> {
    Arc::new(http::NativeHttp::init(
        &blueprint.upstream.clone().http2_only(true),
        &blueprint.telemetry,
    ))
}

fn init_in_memory_cache<K: Hash + Eq, V: Clone>() -> InMemoryCache<K, V> {
    InMemoryCache::default()
}

///
/// # Errors
///
/// Returns an error if the operation fails.
pub fn init(blueprint: &Blueprint) -> anyhow::Result<TargetRuntime> {
    let postgres = build_postgres_connections(blueprint)?;
    Ok(build_runtime(blueprint, postgres))
}

fn build_postgres_connections(
    blueprint: &Blueprint,
) -> anyhow::Result<HashMap<String, Arc<dyn crate::core::postgres::PostgresIO>>> {
    let mut postgres = HashMap::new();
    for (id, spec) in &blueprint.postgres_connections {
        let io: Arc<dyn crate::core::postgres::PostgresIO> = match spec {
            PostgresConnectionSpec::Url(url) => Arc::new(
                crate::cli::postgres::pool::PostgresPool::new(url)
                    .map_err(|e| anyhow::anyhow!("Failed to create Postgres pool '{id}': {e}"))?,
            ),
            PostgresConnectionSpec::AuroraDsql { endpoint, region, admin } => Arc::new(
                crate::cli::postgres::dsql_pool::AuroraDsqlPool::new(endpoint, region, *admin)
                    .map_err(|e| anyhow::anyhow!("Failed to create DSQL pool '{id}': {e}"))?,
            ),
        };
        postgres.insert(id.clone(), io);
    }
    Ok(postgres)
}

fn build_runtime(
    blueprint: &Blueprint,
    postgres: HashMap<String, Arc<dyn crate::core::postgres::PostgresIO>>,
) -> TargetRuntime {
    #[cfg(not(feature = "js"))]
    tracing::warn!("JS capabilities are disabled in this build");

    TargetRuntime {
        http: init_http(blueprint),
        http2_only: init_http2_only(blueprint),
        env: init_env(),
        file: init_file(),
        cache: Arc::new(init_in_memory_cache()),
        extensions: Arc::new(vec![]),
        cmd_worker: init_http_worker_io(blueprint.server.script.clone()),
        worker: init_resolver_worker_io(blueprint.server.script.clone()),
        postgres,
        s3: HashMap::new(),
    }
}

///
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn confirm_and_write(
    runtime: TargetRuntime,
    path: &str,
    content: &[u8],
) -> anyhow::Result<()> {
    let file_exists = fs::metadata(path).is_ok();

    if file_exists {
        let confirm = Confirm::new(&format!("Do you want to overwrite the file {path}?"))
            .with_default(false)
            .prompt()?;

        if !confirm {
            return Ok(());
        }
    }

    runtime.file.write(path, content).await?;

    Ok(())
}

///
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn create_directory(folder_path: &str) -> anyhow::Result<()> {
    let folder_exists = fs::metadata(folder_path).is_ok();

    if !folder_exists {
        let confirm = Confirm::new(&format!("Do you want to create the folder {folder_path}?"))
            .with_default(false)
            .prompt()?;

        if confirm {
            fs::create_dir_all(folder_path)?;
        } else {
            return Ok(());
        }
    }

    Ok(())
}

///
/// # Errors
///
/// Returns an error if the operation fails.
pub fn select_prompt<T: std::fmt::Display>(message: &str, options: Vec<T>) -> anyhow::Result<T> {
    Ok(Select::new(message, options).prompt()?)
}
