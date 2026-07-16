pub mod data_loader;
pub mod dsql_token;
pub mod introspector;
pub mod request_template;
pub mod schema;
pub mod sql_parser;

use gqlrs_value::ConstValue;
pub use request_template::RequestTemplate;
pub(crate) use request_template::quote_ident;
pub use schema::DatabaseSchema;

/// How to interpret NOTIFY payload bytes.
pub use crate::core::config::PostgresPayloadType;

/// Build a rustls-based TLS connector for `PostgreSQL` connections.
pub(crate) fn make_tls_connect() -> anyhow::Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() {
        tracing::warn!(
            count = native.errors.len(),
            "some native certificates could not be loaded: {:?}",
            native.errors
        );
    }
    let mut root_store = rustls::RootCertStore::empty();
    let mut added = 0u32;
    for cert in native.certs {
        match root_store.add(cert) {
            Ok(()) => added += 1,
            Err(e) => tracing::warn!("skipping certificate: {e}"),
        }
    }
    if added == 0 {
        anyhow::bail!("no valid TLS root certificates found; cannot establish TLS connections");
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// Trait for executing SQL queries against `PostgreSQL`.
/// Concrete implementations live in the CLI crate (real connection pool)
/// or in test utilities (mock).
#[async_trait::async_trait]
pub trait PostgresIO: Send + Sync + 'static {
    /// Execute a parameterised SQL query and return the result rows as a
    /// `ConstValue` (typically a JSON array of objects).
    async fn execute(&self, query: &str, params: &[Option<String>]) -> anyhow::Result<ConstValue>;

    /// Execute a data-modification statement and return its affected row count.
    async fn execute_affected(
        &self,
        query: &str,
        params: &[Option<String>],
    ) -> anyhow::Result<u64> {
        let _ = (query, params);
        anyhow::bail!("affected-row execution is not implemented for this PostgreSQL connection")
    }
}

/// Trait for subscribing to `PostgreSQL` LISTEN/NOTIFY channels.
/// Concrete implementations live in the CLI crate or in test utilities
/// (mock listener).
#[async_trait::async_trait]
pub trait PostgresListenerIO: Send + Sync + 'static {
    /// Subscribe to a `PostgreSQL` NOTIFY channel.
    /// Returns a stream that yields one `ConstValue` per notification event.
    async fn subscribe(
        &self,
        channel: &str,
        payload_type: PostgresPayloadType,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures_util::Stream<Item = Result<ConstValue, anyhow::Error>> + Send>,
        >,
    >;
}
