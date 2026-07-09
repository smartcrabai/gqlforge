use std::sync::Arc;

use gqlforge::core::s3::S3IO;
use gqlrs_value::ConstValue;

/// A mock implementation of `S3IO` that returns fixed responses.
#[expect(
    clippy::struct_field_names,
    reason = "field names mirror S3IO method names"
)]
pub struct MockS3IO {
    get_presigned_url_response: String,
    put_presigned_url_response: String,
    list_response: ConstValue,
    delete_response: ConstValue,
}

impl MockS3IO {
    #[expect(dead_code)]
    pub fn new(
        get_presigned_url_response: impl Into<String>,
        put_presigned_url_response: impl Into<String>,
        list_response: ConstValue,
        delete_response: ConstValue,
    ) -> Arc<Self> {
        Arc::new(Self {
            get_presigned_url_response: get_presigned_url_response.into(),
            put_presigned_url_response: put_presigned_url_response.into(),
            list_response,
            delete_response,
        })
    }
}

#[async_trait::async_trait]
impl S3IO for MockS3IO {
    async fn get_presigned_url(
        &self,
        _bucket: &str,
        _key: &str,
        _expiration_secs: u64,
    ) -> anyhow::Result<String> {
        Ok(self.get_presigned_url_response.clone())
    }

    async fn put_presigned_url(
        &self,
        _bucket: &str,
        _key: &str,
        _expiration_secs: u64,
        _content_type: Option<&str>,
    ) -> anyhow::Result<String> {
        Ok(self.put_presigned_url_response.clone())
    }

    async fn list_objects(
        &self,
        _bucket: &str,
        _prefix: Option<&str>,
    ) -> anyhow::Result<ConstValue> {
        Ok(self.list_response.clone())
    }

    async fn delete_object(&self, _bucket: &str, _key: &str) -> anyhow::Result<ConstValue> {
        Ok(self.delete_response.clone())
    }
}
