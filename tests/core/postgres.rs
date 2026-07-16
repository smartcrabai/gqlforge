use std::sync::Arc;

use gqlforge::core::postgres::PostgresIO;
use gqlrs_value::ConstValue;

/// A mock implementation of `PostgresIO` that returns a fixed response.
pub struct MockPostgresIO {
    response: ConstValue,
}

impl MockPostgresIO {
    #[expect(dead_code)]
    pub fn new(response: ConstValue) -> Arc<Self> {
        Arc::new(Self { response })
    }
}

#[async_trait::async_trait]
impl PostgresIO for MockPostgresIO {
    async fn execute(
        &self,
        _query: &str,
        _params: &[Option<String>],
    ) -> anyhow::Result<ConstValue> {
        Ok(self.response.clone())
    }
}
