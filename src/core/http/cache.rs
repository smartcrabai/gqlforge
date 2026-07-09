use cache_control::CacheControl;

use super::Response;

pub fn cache_policy(res: &Response<gqlrs::Value>) -> Option<CacheControl> {
    let header = res.headers.get(http::header::CACHE_CONTROL)?;
    let value = header.to_str().ok()?;

    CacheControl::from_value(value)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::time::Duration;

    use cache_control::Cachability;
    use http::header::HeaderMap;

    use crate::core::http::Response;

    pub fn max_age(res: &Response<gqlrs::Value>) -> Option<Duration> {
        match super::cache_policy(res) {
            Some(value) => value.max_age,
            None => None,
        }
    }

    /// Returns the minimum TTL of the given responses.
    pub fn min_ttl<'a>(res_vec: impl Iterator<Item = &'a Response<gqlrs::Value>>) -> i32 {
        let mut min = -1;

        for res in res_vec {
            if let Some(max_age) = max_age(res) {
                let ttl = i32::try_from(max_age.as_secs()).unwrap_or(i32::MAX);
                if min == -1 || ttl < min {
                    min = ttl;
                }
            }
        }
        min
    }

    pub fn cache_visibility(res: &Response<gqlrs::Value>) -> String {
        let cachability = super::cache_policy(res).and_then(|value| value.cachability);

        match cachability {
            Some(Cachability::Public) => "public".to_owned(),
            Some(Cachability::Private) => "private".to_owned(),
            Some(Cachability::NoCache) => "no-cache".to_owned(),
            _ => String::new(),
        }
    }

    fn cache_control_header(i: i32) -> HeaderMap {
        let mut headers = headers::HeaderMap::default();
        headers.append("Cache-Control", format!("max-age={i}").parse().unwrap());
        headers
    }

    fn cache_control_header_visibility(i: i32, visibility: &str) -> HeaderMap {
        let mut headers = headers::HeaderMap::default();
        headers.append(
            "Cache-Control",
            format!("max-age={i}, {visibility}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn test_max_age_none() {
        let response = Response::default();
        assert_eq!(max_age(&response), None);
    }

    #[test]
    fn test_max_age_some() {
        let headers = cache_control_header(3600);
        let response = Response::default().headers(headers);

        assert_eq!(max_age(&response), Some(Duration::from_hours(1)));
    }

    #[test]
    fn test_min_ttl() {
        let max_ages =
            [3600, 1800, 7200].map(|i| Response::default().headers(cache_control_header(i)));
        let min = min_ttl(max_ages.iter());
        assert_eq!(min, 1800);
    }

    #[test]
    fn test_cache_visibility_public() {
        let headers = cache_control_header_visibility(3600, "public");
        let response = Response::default().headers(headers);

        assert_eq!(max_age(&response), Some(Duration::from_hours(1)));
        assert_eq!(cache_visibility(&response), "public");
    }

    #[test]
    fn test_cache_visibility_private() {
        let headers = cache_control_header_visibility(3600, "private");
        let response = Response::default().headers(headers);

        assert_eq!(max_age(&response), Some(Duration::from_hours(1)));
        assert_eq!(cache_visibility(&response), "private");
    }
}
