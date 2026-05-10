use std::sync::OnceLock;

/// Generate an IAM authentication token for Aurora DSQL.
///
/// # Errors
///
/// Returns an error if AWS credential resolution fails, the signer
/// configuration is invalid, or the token generation API call fails.
pub async fn generate_dsql_token(
    config: &aws_types::sdk_config::SdkConfig,
    endpoint: &str,
    region: &str,
    admin: bool,
) -> anyhow::Result<String> {
    let signer = aws_sdk_dsql::auth_token::AuthTokenGenerator::new(
        aws_sdk_dsql::auth_token::Config::builder()
            .hostname(endpoint)
            .region(aws_sdk_dsql::config::Region::new(region.to_string()))
            .build()
            .map_err(|e| anyhow::anyhow!("DSQL auth config error: {e}"))?,
    );
    let token = if admin {
        signer.db_connect_admin_auth_token(config).await
    } else {
        signer.db_connect_auth_token(config).await
    };
    token
        .map(|t| t.to_string())
        .map_err(|e| anyhow::anyhow!("DSQL token generation failed: {e}"))
}

static CACHED_AWS_CONFIG: OnceLock<aws_types::sdk_config::SdkConfig> = OnceLock::new();

/// Load AWS config for DSQL operations. Config is cached and reused across
/// connection creations to avoid expensive credential chain resolution.
///
/// # Errors
///
/// Returns an error if the Tokio runtime cannot be created or if AWS
/// credential resolution fails.
pub fn load_dsql_aws_config() -> anyhow::Result<&'static aws_types::sdk_config::SdkConfig> {
    if let Some(config) = CACHED_AWS_CONFIG.get() {
        return Ok(config);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to create runtime: {e}"))?;
    let config = rt.block_on(aws_config::load_defaults(
        aws_config::BehaviorVersion::latest(),
    ));
    // set may fail if another thread initialized first - use the cached value
    // instead
    let _ = CACHED_AWS_CONFIG.set(config);
    CACHED_AWS_CONFIG
        .get()
        .ok_or_else(|| anyhow::anyhow!("DSQL AWS config initialization failed"))
}

const DSQL_PORT: u16 = 5432;
const DSQL_DATABASE: &str = "postgres";
const DSQL_SSL_MODE: &str = "require";

#[must_use]
pub fn build_dsql_url(endpoint: &str, token: &str, admin: bool) -> String {
    let user = if admin { "admin" } else { "iam_user" };
    format!(
        "postgresql://{user}:{}@{endpoint}:{DSQL_PORT}/{DSQL_DATABASE}?sslmode={DSQL_SSL_MODE}",
        urlencoding::encode(token),
    )
}
