use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

use gqlforge_hasher::GqlforgeHasher;

fn clone_request(req: &reqwest::Request) -> reqwest::Request {
    req.try_clone().unwrap_or_else(|| {
        let mut new_req = reqwest::Request::new(req.method().clone(), req.url().clone());
        new_req.headers_mut().extend(req.headers().clone());
        *new_req.timeout_mut() = req.timeout().copied();
        new_req
    })
}

#[derive(Debug)]
pub struct DataLoaderRequest {
    request: reqwest::Request,
    headers: BTreeSet<String>,
    /// used for request body batching.
    batching_value: Option<String>,
}

impl DataLoaderRequest {
    pub fn new(req: reqwest::Request, headers: BTreeSet<String>) -> Self {
        // TODO: req should already have headers builtin, no?
        Self { request: req, headers, batching_value: None }
    }

    #[must_use]
    pub fn with_batching_value(self, body: Option<String>) -> Self {
        Self { batching_value: body, ..self }
    }

    pub fn batching_value(&self) -> Option<&String> {
        self.batching_value.as_ref()
    }

    pub fn to_request(&self) -> reqwest::Request {
        clone_request(&self.request)
    }
    pub fn headers(&self) -> &BTreeSet<String> {
        &self.headers
    }
}
impl Hash for DataLoaderRequest {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.request.url().hash(state);
        // use body in hash for graphql queries with query operation as they
        // used to fetch data while http post and graphql mutation
        // should not be loaded through dataloader at all!
        if let Some(body) = self.request.body() {
            body.as_bytes().hash(state);
        }
        for name in &self.headers {
            if let Some(value) = self.request.headers().get(name) {
                name.hash(state);
                value.hash(state);
            }
        }
    }
}

impl PartialEq for DataLoaderRequest {
    fn eq(&self, other: &Self) -> bool {
        let mut hasher_self = GqlforgeHasher::default();
        self.hash(&mut hasher_self);
        let hash_self = hasher_self.finish();

        let mut hasher_other = GqlforgeHasher::default();
        other.hash(&mut hasher_other);
        let hash_other = hasher_other.finish();

        hash_self == hash_other
    }
}

impl Eq for DataLoaderRequest {}

impl Clone for DataLoaderRequest {
    fn clone(&self) -> Self {
        let req = clone_request(&self.request);
        DataLoaderRequest::new(req, self.headers.clone())
            .with_batching_value(self.batching_value.clone())
    }
}

impl Deref for DataLoaderRequest {
    type Target = reqwest::Request;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use http::header::{HeaderName, HeaderValue};

    use super::*;
    fn create_request_with_headers(url: &str, headers: Vec<(&str, &str)>) -> reqwest::Request {
        let mut req = reqwest::Request::new(reqwest::Method::GET, url.parse().unwrap());
        for (name, value) in headers {
            req.headers_mut().insert(
                name.parse::<HeaderName>().unwrap(),
                value.parse::<HeaderValue>().unwrap(),
            );
        }
        req
    }

    fn create_endpoint_key(
        url: &str,
        headers: Vec<(&str, &str)>,
        hash_key_headers: BTreeSet<String>,
    ) -> DataLoaderRequest {
        DataLoaderRequest::new(create_request_with_headers(url, headers), hash_key_headers)
    }

    #[test]
    fn test_hash_endpoint_key() {
        let endpoint_key_1 = create_endpoint_key("http://localhost:8080", vec![], BTreeSet::new());
        let endpoint_key_2 = create_endpoint_key("http://localhost:8080", vec![], BTreeSet::new());
        assert_eq!(endpoint_key_1, endpoint_key_2);
    }

    #[test]
    fn test_with_endpoint_key_with_headers() {
        let endpoint_key_1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "2")],
            BTreeSet::from(["a".to_string(), "b".to_string()]),
        );
        let endpoint_key_2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "2"), ("c", "3")],
            BTreeSet::from(["a".to_string(), "b".to_string()]),
        );
        assert_eq!(endpoint_key_1, endpoint_key_2);
    }

    #[test]
    fn test_with_endpoint_key_with_headers_ne() {
        let endpoint_key_1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "2"), ("c", "4")],
            BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()]),
        );
        let endpoint_key_2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "2"), ("c", "3")],
            BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()]),
        );
        assert_ne!(endpoint_key_1, endpoint_key_2);
    }
    #[test]
    fn test_different_http_methods() {
        let key1 = create_endpoint_key("http://localhost:8080", vec![], BTreeSet::new());
        let req = reqwest::Request::new(
            reqwest::Method::POST,
            "http://localhost:8080".parse().unwrap(),
        );
        let key2 = DataLoaderRequest::new(req, BTreeSet::new());
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_different_urls() {
        let key1 = create_endpoint_key("http://localhost:8080", vec![], BTreeSet::new());
        let key2 = create_endpoint_key("http://example.com:8080", vec![], BTreeSet::new());
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_mismatched_header_names() {
        let key1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1")],
            BTreeSet::from(["a".to_string()]),
        );
        let key2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("b", "1")],
            BTreeSet::from(["b".to_string()]),
        );
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_mismatched_header_values() {
        let key1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1")],
            BTreeSet::from(["a".to_string()]),
        );
        let key2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "2")],
            BTreeSet::from(["a".to_string()]),
        );
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_differing_number_of_headers() {
        let key1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1")],
            BTreeSet::from(["a".to_string()]),
        );
        let key2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "2")],
            BTreeSet::from(["a".to_string(), "b".to_string()]),
        );
        assert_ne!(key1, key2);
    }
    #[test]
    fn test_clone_trait() {
        let key1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1")],
            BTreeSet::from(["a".to_string()]),
        );
        let key2 = key1.clone();

        // The cloned key should be equal to the original
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_partial_eq_trait() {
        let key1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1")],
            BTreeSet::from(["a".to_string()]),
        );
        let key2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1")],
            BTreeSet::from(["a".to_string()]),
        );

        // Both keys have the same data, so they should be equal
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_partial_eq_not_equal() {
        let key1 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "1")],
            BTreeSet::from(["a".to_string(), "b".to_string()]),
        );
        let key2 = create_endpoint_key(
            "http://localhost:8080",
            vec![("a", "1"), ("b", "1")],
            BTreeSet::from(["a".to_string()]),
        );

        assert_ne!(key1, key2);
    }
}
