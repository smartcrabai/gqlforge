use http_cache_reqwest::{CacheManager, HttpResponse};
use http_cache_semantics::CachePolicy;
use serde::{Deserialize, Serialize};
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, BoxError>;
use std::sync::Arc;

use moka::future::Cache;
use moka::policy::EvictionPolicy;

pub struct HttpCacheManager {
    pub cache: Arc<Cache<String, Store>>,
}

impl Default for HttpCacheManager {
    fn default() -> Self {
        Self::new(42)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Store {
    response: HttpResponse,
    policy: CachePolicy,
}

impl HttpCacheManager {
    #[must_use]
    pub fn new(cache_size: u64) -> Self {
        let cache = Cache::builder()
            .eviction_policy(EvictionPolicy::lru())
            .max_capacity(cache_size)
            .build();
        Self { cache: Arc::new(cache) }
    }

    /// # Errors
    ///
    /// Returns an error if cache invalidation fails.
    pub async fn clear(&self) -> Result<()> {
        self.cache.invalidate_all();
        self.cache.run_pending_tasks().await;
        Ok(())
    }
}

impl CacheManager for HttpCacheManager {
    async fn get(&self, cache_key: &str) -> Result<Option<(HttpResponse, CachePolicy)>> {
        let store: Store = match self.cache.get(cache_key).await {
            Some(d) => d,
            None => return Ok(None),
        };
        Ok(Some((store.response, store.policy)))
    }

    async fn put(
        &self,
        cache_key: String,
        response: HttpResponse,
        policy: CachePolicy,
    ) -> Result<HttpResponse> {
        let data = Store { response: response.clone(), policy };
        self.cache.insert(cache_key, data).await;
        self.cache.run_pending_tasks().await;
        Ok(response)
    }

    async fn delete(&self, cache_key: &str) -> Result<()> {
        self.cache.invalidate(cache_key).await;
        self.cache.run_pending_tasks().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]

    use http_cache::{HttpHeaders, HttpVersion};
    use url::Url;

    use super::*;

    async fn insert_key_into_cache(manager: &HttpCacheManager, key: &str) {
        let request_url = "http://localhost:8080/test";
        let url = Url::parse(request_url).unwrap();

        let http_resp = HttpResponse {
            headers: HttpHeaders::new(),
            body: vec![1, 2, 3],
            status: 200,
            url: url.clone(),
            version: HttpVersion::Http11,
            metadata: None,
        };

        let request = http::Request::builder()
            .method("GET")
            .uri(request_url)
            .body(())
            .unwrap();
        let response = http::Response::builder().status(200).body(()).unwrap();

        let _ = manager
            .put(
                key.to_string(),
                http_resp,
                CachePolicy::new(&request, &response),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_get_when_key_present() {
        let manager = HttpCacheManager::default();
        insert_key_into_cache(&manager, "test").await;
        let value = manager.get("test").await.unwrap();
        assert!(value.is_some());
    }

    #[tokio::test]
    async fn test_get_when_key_not_present() {
        let manager = HttpCacheManager::default();
        let result = manager.get("test").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete_when_key_present() {
        let manager = HttpCacheManager::default();
        insert_key_into_cache(&manager, "test").await;

        assert_eq!(manager.cache.iter().count(), 1);
        let _ = manager.delete("test").await;
        assert_eq!(manager.cache.iter().count(), 0);
    }

    #[tokio::test]
    async fn test_clear() {
        let manager = HttpCacheManager::default();
        insert_key_into_cache(&manager, "test").await;
        assert_eq!(manager.cache.iter().count(), 1);
        let _ = manager.clear().await;
        assert_eq!(manager.cache.iter().count(), 0);
    }

    #[tokio::test]
    async fn test_lru_eviction_policy() {
        let manager = HttpCacheManager::new(2);
        insert_key_into_cache(&manager, "test-1").await;
        insert_key_into_cache(&manager, "test-2").await;
        insert_key_into_cache(&manager, "test-10").await;

        let res = manager.get("test-1").await.unwrap();
        assert!(res.is_none());

        let res = manager.get("test-2").await.unwrap();
        assert!(res.is_some());

        let res = manager.get("test-10").await.unwrap();
        assert!(res.is_some());

        assert_eq!(manager.cache.entry_count(), 2);
    }
}
