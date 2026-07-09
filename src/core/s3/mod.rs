pub mod request_template;

use gqlrs_value::ConstValue;
pub use request_template::RequestTemplate;

/// Trait for executing S3 operations.
/// Concrete implementations live in the CLI crate (real AWS SDK client)
/// or in test utilities (mock).
#[async_trait::async_trait]
pub trait S3IO: Send + Sync + 'static {
    /// Generate a presigned GET URL for the given bucket/key.
    async fn get_presigned_url(
        &self,
        bucket: &str,
        key: &str,
        expiration_secs: u64,
    ) -> anyhow::Result<String>;

    /// Generate a presigned PUT URL for the given bucket/key.
    async fn put_presigned_url(
        &self,
        bucket: &str,
        key: &str,
        expiration_secs: u64,
        content_type: Option<&str>,
    ) -> anyhow::Result<String>;

    /// List objects in a bucket with an optional prefix.
    async fn list_objects(&self, bucket: &str, prefix: Option<&str>) -> anyhow::Result<ConstValue>;

    /// Delete an object from a bucket.
    async fn delete_object(&self, bucket: &str, key: &str) -> anyhow::Result<ConstValue>;
}
