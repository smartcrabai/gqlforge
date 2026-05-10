use async_graphql_value::ConstValue;
use deadpool::managed::{Manager, Metrics, Pool, RecycleError, RecycleResult};

use super::pool::{TypedParam, rows_to_const_value};
use crate::core::postgres::PostgresIO;

/// IAM-authenticated connection pool for Amazon Aurora DSQL.
///
/// A new IAM token is generated on every connection creation, which keeps the
/// pool alive beyond the 15-minute token expiry.
pub struct AuroraDsqlPool {
    inner: Pool<DsqlManager>,
}

struct DsqlManager {
    config: &'static aws_types::sdk_config::SdkConfig,
    endpoint: String,
    region: String,
    admin: bool,
}

impl DsqlManager {
    async fn create_client(&self) -> anyhow::Result<tokio_postgres::Client> {
        let token = crate::core::postgres::dsql_token::generate_dsql_token(
            self.config,
            &self.endpoint,
            &self.region,
            self.admin,
        )
        .await?;
        let url =
            crate::core::postgres::dsql_token::build_dsql_url(&self.endpoint, &token, self.admin);
        let tls = crate::core::postgres::make_tls_connect()?;
        let (client, connection) = tokio_postgres::connect(&url, tls).await?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("DSQL connection error: {e}");
            }
        });
        Ok(client)
    }
}

impl Manager for DsqlManager {
    type Type = tokio_postgres::Client;
    type Error = anyhow::Error;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        self.create_client().await
    }

    async fn recycle(
        &self,
        conn: &mut Self::Type,
        _metrics: &Metrics,
    ) -> RecycleResult<Self::Error> {
        conn.simple_query("")
            .await
            .map_err(|e| RecycleError::Backend(e.into()))?;
        Ok(())
    }
}

impl AuroraDsqlPool {
    /// Create a new pool for the given Aurora DSQL cluster.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool builder configuration is invalid.
    pub fn new(endpoint: &str, region: &str, admin: bool) -> anyhow::Result<Self> {
        let config = crate::core::postgres::dsql_token::load_dsql_aws_config()?;
        let manager = DsqlManager {
            config,
            endpoint: endpoint.to_string(),
            region: region.to_string(),
            admin,
        };
        let pool = Pool::builder(manager)
            .max_size(16)
            .runtime(deadpool::Runtime::Tokio1)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to create DSQL pool: {e}"))?;
        Ok(Self { inner: pool })
    }
}

#[async_trait::async_trait]
impl PostgresIO for AuroraDsqlPool {
    async fn execute(&self, query: &str, params: &[String]) -> anyhow::Result<ConstValue> {
        let client = self
            .inner
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("DSQL pool error: {e}"))?;
        let typed_params: Vec<TypedParam> = params.iter().map(|p| TypedParam(p.clone())).collect();
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = typed_params
            .iter()
            .map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = client.query(query, &param_refs).await?;
        rows_to_const_value(&rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires AWS credentials"]
    async fn new_with_valid_params_succeeds() {
        let result = AuroraDsqlPool::new("cluster123.dsql.us-east-1.on.aws", "us-east-1", false);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
    }

    #[tokio::test]
    #[ignore = "requires AWS credentials"]
    async fn new_with_admin_true_succeeds() {
        let result = AuroraDsqlPool::new("cluster.dsql.us-east-1.on.aws", "us-east-1", true);
        assert!(result.is_ok());
    }

    #[test]
    fn token_url_encoding_handles_base64_chars() {
        let token = "abc+def/ghi==";
        let encoded = urlencoding::encode(token).into_owned();
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        assert!(encoded.contains("%2B") || encoded.contains("%2b"));
        assert!(encoded.contains("%2F") || encoded.contains("%2f"));
        assert!(encoded.contains("%3D") || encoded.contains("%3d"));
    }

    #[test]
    fn token_url_format_uses_admin_user_and_sslmode() {
        let url = crate::core::postgres::dsql_token::build_dsql_url(
            "cluster.dsql.us-east-1.on.aws",
            "mytokenvalue",
            false,
        );
        assert!(url.starts_with("postgresql://iam_user:"));
        assert!(url.contains("@cluster.dsql.us-east-1.on.aws:5432/postgres"));
        assert!(url.ends_with("sslmode=require"));
    }

    #[tokio::test]
    #[ignore = "requires live Aurora DSQL cluster and AWS credentials"]
    async fn execute_select_returns_rows() -> anyhow::Result<()> {
        let pool = AuroraDsqlPool::new("cluster.dsql.us-east-1.on.aws", "us-east-1", true)?;
        let result = pool.execute("SELECT 1 AS n", &[]).await?;
        if let ConstValue::List(rows) = result {
            assert_eq!(rows.len(), 1);
        } else {
            panic!("expected list");
        }
        Ok(())
    }
}
