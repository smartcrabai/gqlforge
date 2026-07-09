pub mod client {
    use std::time::Duration;

    use gqlrs_value::ConstValue;
    use indexmap::IndexMap;

    use crate::core::s3::S3IO;

    /// An S3 client backed by `aws-sdk-s3`.
    pub struct S3Client {
        client: aws_sdk_s3::Client,
    }

    impl S3Client {
        /// Create a new S3 client from the given endpoint configuration.
        ///
        /// # Errors
        ///
        /// Returns an error if the operation fails.
        pub async fn new(
            endpoint: Option<&str>,
            region: &str,
            force_path_style: bool,
        ) -> anyhow::Result<Self> {
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new(region.to_string()))
                .load()
                .await;

            let mut s3_config_builder =
                aws_sdk_s3::config::Builder::from(&config).force_path_style(force_path_style);

            if let Some(ep) = endpoint {
                s3_config_builder = s3_config_builder.endpoint_url(ep);
            }

            let client = aws_sdk_s3::Client::from_conf(s3_config_builder.build());

            Ok(Self { client })
        }
    }

    #[async_trait::async_trait]
    impl S3IO for S3Client {
        async fn get_presigned_url(
            &self,
            bucket: &str,
            key: &str,
            expiration_secs: u64,
        ) -> anyhow::Result<String> {
            let presigning_config = aws_sdk_s3::presigning::PresigningConfig::expires_in(
                Duration::from_secs(expiration_secs),
            )?;

            let presigned = self
                .client
                .get_object()
                .bucket(bucket)
                .key(key)
                .presigned(presigning_config)
                .await?;

            Ok(presigned.uri().to_string())
        }

        async fn put_presigned_url(
            &self,
            bucket: &str,
            key: &str,
            expiration_secs: u64,
            content_type: Option<&str>,
        ) -> anyhow::Result<String> {
            let presigning_config = aws_sdk_s3::presigning::PresigningConfig::expires_in(
                Duration::from_secs(expiration_secs),
            )?;

            let mut req = self.client.put_object().bucket(bucket).key(key);

            if let Some(ct) = content_type {
                req = req.content_type(ct);
            }

            let presigned = req.presigned(presigning_config).await?;

            Ok(presigned.uri().to_string())
        }

        async fn list_objects(
            &self,
            bucket: &str,
            prefix: Option<&str>,
        ) -> anyhow::Result<ConstValue> {
            let mut req = self.client.list_objects_v2().bucket(bucket);

            if let Some(p) = prefix {
                req = req.prefix(p);
            }

            // NOTE: ListObjectsV2 returns up to 1,000 keys per request.
            // Pagination is intentionally not implemented here; callers should
            // use the `prefix` parameter to narrow results within a single page.
            let output = req.send().await?;
            let contents = output.contents();

            let items: Vec<ConstValue> = contents
                .iter()
                .map(|obj| {
                    let mut map = IndexMap::new();
                    if let Some(key) = obj.key() {
                        map.insert(gqlrs::Name::new("key"), ConstValue::String(key.to_string()));
                    }
                    if let Some(size) = obj.size() {
                        map.insert(gqlrs::Name::new("size"), ConstValue::Number(size.into()));
                    }
                    if let Some(last_modified) = obj.last_modified() {
                        map.insert(
                            gqlrs::Name::new("lastModified"),
                            ConstValue::String(last_modified.to_string()),
                        );
                    }
                    if let Some(etag) = obj.e_tag() {
                        map.insert(
                            gqlrs::Name::new("etag"),
                            ConstValue::String(etag.to_string()),
                        );
                    }
                    ConstValue::Object(map)
                })
                .collect();

            Ok(ConstValue::List(items))
        }

        async fn delete_object(&self, bucket: &str, key: &str) -> anyhow::Result<ConstValue> {
            self.client
                .delete_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await?;

            Ok(ConstValue::Boolean(true))
        }
    }
}
