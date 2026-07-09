use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gqlrs::async_trait;
use gqlrs_value::ConstValue;

use crate::core::config::Batch;
use crate::core::config::group_by::GroupBy;
use crate::core::data_loader::{DataLoader, Loader};
use crate::core::http::Response;
use crate::core::postgres::PostgresIO;
use crate::core::postgres::request_template::RenderedQuery;

/// Key type for the Postgres `DataLoader`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PostgresDataLoaderRequest {
    pub query: RenderedQuery,
}

/// `DataLoader` implementation for `PostgreSQL` that batches
/// identical queries via deduplication.
#[derive(Clone)]
pub struct PostgresDataLoader {
    pub(crate) postgres: Arc<dyn PostgresIO>,
    #[expect(dead_code, reason = "reserved for future group-by batching support")]
    pub(crate) group_by: Option<GroupBy>,
}

impl PostgresDataLoader {
    #[must_use]
    pub fn into_data_loader(self, batch: &Batch) -> DataLoader<PostgresDataLoaderRequest, Self> {
        DataLoader::new(self)
            .delay(Duration::from_millis(batch.delay as u64))
            .max_batch_size(batch.max_size.unwrap_or_default())
    }
}

#[async_trait::async_trait]
impl Loader<PostgresDataLoaderRequest> for PostgresDataLoader {
    type Value = Response<ConstValue>;
    type Error = Arc<anyhow::Error>;

    async fn load(
        &self,
        keys: &[PostgresDataLoaderRequest],
    ) -> Result<HashMap<PostgresDataLoaderRequest, Self::Value>, Self::Error> {
        let mut result = HashMap::new();

        // For now, execute each unique query individually.
        // Batching with `WHERE col IN (...)` can be added later when
        // group_by is set.
        for key in keys {
            let value = self
                .postgres
                .execute(&key.query.sql, &key.query.params)
                .await
                .map_err(Arc::new)?;

            result.insert(key.clone(), Response { body: value, ..Default::default() });
        }

        Ok(result)
    }
}
