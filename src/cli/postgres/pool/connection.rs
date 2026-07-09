use deadpool_postgres::{Config, Pool, Runtime};
use gqlrs_value::ConstValue;

use super::conversion::rows_to_const_value;
use super::types::TypedParam;
use crate::core::postgres::PostgresIO;

/// A connection pool backed by `deadpool-postgres`.
pub struct PostgresPool {
    pool: Pool,
}

impl PostgresPool {
    /// Create a new pool from a `PostgreSQL` connection string.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn new(connection_url: &str) -> anyhow::Result<Self> {
        let mut cfg = Config::new();
        cfg.url = Some(connection_url.to_string());

        let tls = crate::core::postgres::make_tls_connect()?;
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), tls)
            .map_err(|e| anyhow::anyhow!("Failed to create PostgreSQL pool: {e}"))?;

        Ok(Self { pool })
    }
}

#[async_trait::async_trait]
impl PostgresIO for PostgresPool {
    async fn execute(&self, query: &str, params: &[String]) -> anyhow::Result<ConstValue> {
        let client = self.pool.get().await?;
        let typed_params: Vec<TypedParam> = params.iter().map(|p| TypedParam(p.clone())).collect();
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = typed_params
            .iter()
            .map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = client.query(query, &param_refs).await?;
        rows_to_const_value(&rows)
    }
}
