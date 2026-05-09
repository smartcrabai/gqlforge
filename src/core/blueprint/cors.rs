use derive_setters::Setters;
use gqlforge_valid::ValidationError;
use http::header::{self, HeaderName, HeaderValue, InvalidHeaderValue};
use http::request::Parts;

use super::BlueprintError;
use crate::core::config;

#[derive(Clone, Debug, Setters, Default)]
pub struct Cors {
    pub allow_credentials: bool,
    pub allow_headers: Option<HeaderValue>,
    pub allow_methods: Option<HeaderValue>,
    pub allow_origins: Vec<HeaderValue>,
    pub allow_private_network: bool,
    pub expose_headers: Option<HeaderValue>,
    pub max_age: Option<HeaderValue>,
    pub vary: Vec<HeaderValue>,
}

impl Cors {
    pub fn allow_origin_to_header(
        &self,
        origin: Option<&HeaderValue>,
    ) -> Option<(HeaderName, HeaderValue)> {
        if self.allow_origins.iter().any(is_wildcard) {
            Some((header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.cloned()?))
        } else {
            let allow_origin = origin.filter(|o| self.allow_origins.contains(o))?.clone();
            Some((header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin))
        }
    }

    pub fn allow_credentials_to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        self.allow_credentials.then(|| {
            (
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            )
        })
    }

    pub fn allow_private_network_to_header(
        &self,
        parts: &Parts,
    ) -> Option<(HeaderName, HeaderValue)> {
        let request_private_network =
            HeaderName::from_static("access-control-request-private-network");
        let allow_private_network = HeaderName::from_static("access-control-allow-private-network");
        let true_val = HeaderValue::from_static("true");

        if !self.allow_private_network {
            return None;
        }

        // Access-Control-Allow-Private-Network is only relevant if the request
        // has the Access-Control-Request-Private-Network header set, else skip
        if parts.headers.get(&request_private_network) != Some(&true_val) {
            return None;
        }

        self.allow_private_network
            .then_some((allow_private_network, true_val))
    }

    pub fn allow_methods_to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        Some((
            header::ACCESS_CONTROL_ALLOW_METHODS,
            self.allow_methods.clone()?,
        ))
    }

    pub fn allow_headers_to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        Some((
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            self.allow_headers.clone()?,
        ))
    }

    pub fn max_age_to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        Some((
            header::ACCESS_CONTROL_MAX_AGE,
            self.max_age.as_ref()?.clone(),
        ))
    }

    pub fn expose_headers_to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        Some((
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            self.expose_headers.as_ref()?.clone(),
        ))
    }

    pub fn vary_to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        let values = &self.vary;
        let mut res = values.first()?.as_bytes().to_owned();
        for val in &values[1..] {
            res.extend_from_slice(b", ");
            res.extend_from_slice(val.as_bytes());
        }

        let header_val = HeaderValue::from_bytes(&res).ok()?;
        Some((header::VARY, header_val))
    }

    pub fn expose_headers_is_wildcard(&self) -> bool {
        matches!(&self.expose_headers, Some(v) if v == HeaderValue::from_static("*"))
    }
}

fn ensure_usable_cors_rules(
    layer: &Cors,
) -> Result<(), ValidationError<crate::core::blueprint::BlueprintError>> {
    if layer.allow_credentials {
        let allowing_all_headers = layer
            .allow_headers
            .as_ref().as_ref().is_some_and(|val| is_wildcard(val));

        if allowing_all_headers {
            return Err(ValidationError::new(
                BlueprintError::InvalidCORSConfiguration(
                    "Access-Control-Allow-Headers".to_string(),
                ),
            ));
        }

        let allowing_all_methods = layer
            .allow_methods
            .as_ref().as_ref().is_some_and(|val| is_wildcard(val));

        if allowing_all_methods {
            return Err(ValidationError::new(
                BlueprintError::InvalidCORSConfiguration(
                    "Access-Control-Allow-Methods".to_string(),
                ),
            ));
        }

        let allowing_all_origins = layer.allow_origins.iter().any(is_wildcard);

        if allowing_all_origins {
            return Err(ValidationError::new(
                BlueprintError::InvalidCORSConfiguration("Access-Control-Allow-Origin".to_string()),
            ));
        }

        if layer.expose_headers_is_wildcard() {
            return Err(ValidationError::new(
                BlueprintError::InvalidCORSConfiguration(
                    "Access-Control-Expose-Headers".to_string(),
                ),
            ));
        }
    }
    Ok(())
}

impl TryFrom<config::cors::Cors> for Cors {
    type Error = ValidationError<crate::core::blueprint::BlueprintError>;

    fn try_from(
        value: config::cors::Cors,
    ) -> Result<Self, ValidationError<crate::core::blueprint::BlueprintError>> {
        let cors = Cors {
            allow_credentials: value.allow_credentials.unwrap_or_default(),
            allow_headers: (!value.allow_headers.is_empty()).then_some(
                value
                    .allow_headers
                    .join(", ")
                    .parse()
                    .map_err(|e: InvalidHeaderValue| ValidationError::new(e.into()))?,
            ),
            allow_methods: {
                Some(if value.allow_methods.is_empty() {
                    "*".parse()
                        .map_err(|e: InvalidHeaderValue| ValidationError::new(e.into()))?
                } else {
                    value
                        .allow_methods
                        .into_iter()
                        .map(|val| val.to_string())
                        .collect::<Vec<String>>()
                        .join(", ")
                        .parse()
                        .map_err(|e: InvalidHeaderValue| ValidationError::new(e.into()))?
                })
            },
            allow_origins: value
                .allow_origins
                .into_iter()
                .map(|val| {
                    val.parse()
                        .map_err(|e: InvalidHeaderValue| ValidationError::new(e.into()))
                })
                .collect::<Result<_, ValidationError<crate::core::blueprint::BlueprintError>>>()?,
            allow_private_network: value.allow_private_network.unwrap_or_default(),
            expose_headers: Some(
                value
                    .expose_headers
                    .join(", ")
                    .parse()
                    .map_err(|e: InvalidHeaderValue| ValidationError::new(e.into()))?,
            ),
            max_age: value.max_age.map(std::convert::Into::into),
            vary: value
                .vary
                .iter()
                .map(|val| {
                    val.parse()
                        .map_err(|e: InvalidHeaderValue| ValidationError::new(e.into()))
                })
                .collect::<Result<_, ValidationError<crate::core::blueprint::BlueprintError>>>()?,
        };
        ensure_usable_cors_rules(&cors)?;
        Ok(cors)
    }
}

pub fn is_wildcard(header_value: &HeaderValue) -> bool {
    header_value == HeaderValue::from_static("*")
}

#[cfg(test)]
mod tests {
    use http::header::HeaderValue;

    use super::*;

    #[test]
    fn test_allow_origin_to_header() {
        let cors = Cors {
            allow_origins: vec![HeaderValue::from_static("https://example.com")],
            ..std::default::Default::default()
        };
        let origin = Some(HeaderValue::from_static("https://example.com"));
        assert_eq!(
            cors.allow_origin_to_header(origin.as_ref()),
            Some((
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("https://example.com")
            ))
        );
    }
}
